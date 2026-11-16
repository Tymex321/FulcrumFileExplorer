//! Query-latency benchmark.
//!
//! The number this produces is the project's headline claim, so it is measured rather than
//! asserted: a synthetic index of 1M entries, queried end to end through
//! [`fx_index::query::execute`] — matcher compile, prefilters, parallel scan, ranking, path
//! resolution for the returned page, and streaming into a sink. Exactly what a keystroke in
//! the search box costs, minus IPC.
//!
//! Target: a substring query over 1M entries well under 50 ms, which is where search stops
//! feeling like a search and starts feeling like filtering.
//!
//! CI publishes these results to the job summary so regressions show up in the PR.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use fx_core::{
    EntryKind, Filters, MatchMode, NullSink, Query, SearchScope, SizeRange, Sort, SortKey,
};
use fx_index::query::{execute, Matcher};
use fx_index::store::{EntryStore, Meta, NO_RECORD};

const ENTRIES: usize = 1_000_000;

/// A store shaped roughly like a real volume: ~2% directories in a three-level tree, and a
/// long tail of files with repeated stems, so substring queries hit a realistic number of
/// candidates and paths have realistic depth.
fn corpus(n: usize) -> EntryStore {
    let stems = [
        "main", "lib", "mod", "index", "README", "config", "test", "utils", "Report", "photo",
        "invoice", "setup", "notes", "build", "package", "styles",
    ];
    let exts = [
        "rs", "ts", "toml", "json", "md", "png", "jpg", "pdf", "dll", "txt",
    ];
    let dir_words = [
        "src",
        "assets",
        "Documents",
        "cache",
        "node",
        "Program Files",
        "users",
    ];

    let mut s = EntryStore::with_mount("C".into(), r"C:\", '\\');
    let root = s.push(5, NO_RECORD, ".", Meta::dir());
    s.set_root(root);

    let mut id = 100u64;
    let mut dirs = vec![root];
    // 20k directories: each new one goes under an earlier one, so depth grows gradually.
    for i in 0..n / 50 {
        let parent = dirs[(i * 7919) % dirs.len()];
        id += 1;
        let d = s.push(
            id,
            parent,
            &format!("{}{}", dir_words[i % dir_words.len()], i),
            Meta::dir(),
        );
        dirs.push(d);
    }
    let mut i = 0usize;
    while s.len() < n {
        let parent = dirs[(i * 104_729) % dirs.len()];
        id += 1;
        let name = format!(
            "{}_{}.{}",
            stems[i % stems.len()],
            i,
            exts[(i / stems.len()) % exts.len()]
        );
        let meta = Meta::new(
            EntryKind::File,
            &Default::default(),
            (i as u64 * 2_654_435_761) % 10_000_000,
            1_600_000_000_000 + (i as i64 * 1_000),
            1_500_000_000_000 + (i as i64 * 1_000),
        );
        s.push(id, parent, &name, meta);
        i += 1;
    }
    s.shrink_to_fit();
    s
}

fn q(text: &str) -> Query {
    Query {
        text: text.into(),
        // The UI asks for a screenful plus a buffer, never the whole result set.
        limit: 200,
        ..Default::default()
    }
}

fn bench_query(c: &mut Criterion) {
    let store = corpus(ENTRIES);
    let stores = [&store];
    eprintln!(
        "corpus: {} entries, {:.1} MB resident ({:.1} bytes/entry)",
        store.len(),
        store.memory_bytes() as f64 / 1e6,
        store.memory_bytes() as f64 / store.len() as f64
    );

    let mut group = c.benchmark_group("query_1m");
    group.sample_size(20);

    let cases: Vec<(&str, Query)> = vec![
        // The headline: a common substring, ~62k hits ranked down to one page.
        ("substring", q("readme")),
        // Matches almost nothing: pure scan throughput without result-collection cost.
        ("substring_rare", q("zzzzzz")),
        // Two terms, ANDed: the second Finder only runs on first-term hits.
        ("substring_two_terms", q("report pdf")),
        // Smart case turns this case-sensitive: scans the original arena, not the mirror.
        ("substring_case_sensitive", q("README")),
        ("fuzzy", {
            let mut q = q("rdme");
            q.mode = MatchMode::Fuzzy;
            q.sort.key = SortKey::Relevance;
            q
        }),
        ("glob", {
            let mut q = q("invoice_*.pdf");
            q.mode = MatchMode::Glob;
            q
        }),
        ("regex", {
            let mut q = q(r"^photo_\d+7\.jpg$");
            q.mode = MatchMode::Regex;
            q
        }),
        // Empty text with a filter: the "all PDFs over 1 MB" browse, all prefilter.
        ("filter_only", {
            let mut q = q("");
            q.filters = Filters {
                extensions: vec!["pdf".into()],
                size: Some(SizeRange {
                    min: Some(1_000_000),
                    max: None,
                }),
                ..Default::default()
            };
            q
        }),
        // Ranking a large match set by a numeric key.
        ("sort_by_size_desc", {
            let mut q = q("config");
            q.sort = Sort {
                key: SortKey::Size,
                desc: true,
                dirs_first: true,
            };
            q
        }),
        // Opt-in and expected to be slower: builds a path per candidate.
        ("full_path", {
            let mut q = q(r"documents");
            q.match_full_path = true;
            q
        }),
        ("subtree_scope", {
            let mut q = q("index");
            q.scope = SearchScope::Subtree(r"C:\src0".into());
            q
        }),
    ];

    for (name, query) in &cases {
        group.bench_function(*name, |b| {
            b.iter(|| execute(&stores, query, &NullSink, false).map(|s| s.total_matched))
        });
    }

    group.bench_function("matcher_compile_fuzzy", |b| {
        // Compiling happens on every keystroke, so it must stay negligible.
        b.iter_batched(
            || Query {
                text: "readme".into(),
                mode: MatchMode::Fuzzy,
                ..Default::default()
            },
            |q| Matcher::new(&q).map(|m| m.matches_all()),
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

criterion_group!(benches, bench_query);
criterion_main!(benches);
