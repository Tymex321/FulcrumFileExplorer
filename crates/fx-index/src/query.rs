//! Matching, filtering, ranking and sorting.
//!
//! Deliberately platform-independent and shared by every backend, so the tests that run on
//! macOS cover the same code that answers queries on Windows. Entry point: [`execute`].
//!
//! The performance shape, in order:
//!
//! 1. **Cheap rejects first.** Tombstone, hidden/system, kind, size, date and extension
//!    checks run before any string matching — comparing a `u64` is far cheaper than
//!    scanning a name.
//! 2. **Substring is the hot path.** The needle is compiled once into a SIMD
//!    [`memchr::memmem::Finder`] and run over the store's pre-folded name arena, so there is
//!    no per-entry `to_lowercase` and no allocation per candidate.
//! 3. **Parallel by chunk.** Records are split into fixed chunks across rayon workers.
//!    Each worker keeps its own candidate list, trimmed to the requested window with
//!    `select_nth_unstable` whenever it grows past twice that size, and the lists are
//!    merged at the end. No shared mutex anywhere on the hot path.
//! 4. **Only score when asked.** `nucleo` runs only for [`MatchMode::Fuzzy`]; regex and
//!    glob patterns are compiled once up front, never per entry.
//! 5. **Resolve paths last.** Path reconstruction walks the parent chain, so it happens
//!    only for the rows that survive limiting (or per candidate when the caller explicitly
//!    asked for `match_full_path` / path sorting, which is why those are opt-in).

use std::cmp::Ordering;
use std::time::Instant;

use fx_core::{
    BatchSink, EntryKind, Filters, FxError, MatchMode, Query, QuerySummary, Result, SearchHit,
    SearchScope, Sort, SortKey, TimeRange,
};
use memchr::memmem;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use rayon::prelude::*;

use crate::store::{flags, fold_lower, kind_bits, EntryStore, Record, RecordIx, CHUNK, NO_RECORD};

/// Upper bound on query text. Queries also arrive over the privileged helper's pipe, so
/// nothing user-controlled is allowed to be unbounded.
pub const MAX_QUERY_TEXT: usize = 4096;

/// Compiled-regex budget. The `regex` crate guarantees linear-time matching, but compile
/// size still scales with the pattern; this caps memory for a hostile `a{1000}{1000}`.
const REGEX_SIZE_LIMIT: usize = 1 << 21;

#[derive(Debug, Clone)]
enum Kind {
    All,
    /// Whitespace-separated terms, all of which must occur (any order) — the convention
    /// Everything users expect. Double quotes keep a phrase with spaces together.
    Substring(Vec<memmem::Finder<'static>>),
    Fuzzy(Box<Pattern>),
    Regex(regex::Regex),
    /// The anchored regex, plus a literal every match must contain (see
    /// [`glob_literal`]) for the arena fast path.
    /// The finder is boxed: on x86_64 Windows it is ~300 bytes of SIMD state, which would
    /// otherwise make every `Kind` that large.
    Glob(regex::Regex, Option<Box<memmem::Finder<'static>>>),
}

/// A compiled query: everything derivable from a [`Query`] once, hoisted out of the
/// per-entry loop.
#[derive(Debug, Clone)]
pub struct Matcher {
    kind: Kind,
    mode: MatchMode,
    case_sensitive: bool,
    /// The needle as compared: folded unless case-sensitive.
    needle: String,
    full_path: bool,
}

/// Per-worker scratch space, so the hot loop never allocates.
pub struct Scratch {
    nucleo: Option<nucleo::Matcher>,
    chars: Vec<char>,
    indices: Vec<u32>,
    path: String,
    /// `(store, parent)` whose path is cached in `parent_path`.
    parent_key: (usize, RecordIx),
    parent_path: String,
    /// Directory paths built so far by this worker. Records are not grouped by parent
    /// (MFT order follows creation order), so the single-entry cache above alone misses
    /// constantly and rebuilds whole parent chains. Keyed without case because one
    /// `Scratch` serves one query, whose folding never changes.
    dir_paths: std::collections::HashMap<(usize, RecordIx), Box<str>>,
}

impl std::fmt::Debug for Scratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scratch").finish_non_exhaustive()
    }
}

impl Scratch {
    fn new(m: &Matcher) -> Self {
        let nucleo = matches!(m.kind, Kind::Fuzzy(_)).then(|| {
            let cfg = if m.full_path {
                nucleo::Config::DEFAULT.match_paths()
            } else {
                nucleo::Config::DEFAULT
            };
            nucleo::Matcher::new(cfg)
        });
        Self {
            nucleo,
            chars: Vec::new(),
            indices: Vec::new(),
            path: String::new(),
            parent_key: (usize::MAX, NO_RECORD),
            parent_path: String::new(),
            dir_paths: std::collections::HashMap::new(),
        }
    }
}

impl Matcher {
    /// Compiles a query. Fails with [`FxError::BadQuery`] for an invalid or oversized
    /// pattern — the only way a query can fail before touching the index.
    pub fn new(query: &Query) -> Result<Self> {
        if query.text.len() > MAX_QUERY_TEXT {
            return Err(FxError::BadQuery(format!(
                "query longer than {MAX_QUERY_TEXT} bytes"
            )));
        }
        let case_sensitive = query.is_case_sensitive();
        let text = query.text.trim();
        let needle = if case_sensitive {
            text.to_owned()
        } else {
            fold_lower(text)
        };
        let kind = if text.is_empty() {
            Kind::All
        } else {
            match query.mode {
                MatchMode::Substring => {
                    let terms: Vec<_> = split_terms(&needle)
                        .into_iter()
                        .map(|t| memmem::Finder::new(t.as_bytes()).into_owned())
                        .collect();
                    if terms.is_empty() {
                        Kind::All
                    } else {
                        Kind::Substring(terms)
                    }
                }
                MatchMode::Fuzzy => {
                    let case = if case_sensitive {
                        CaseMatching::Respect
                    } else {
                        CaseMatching::Ignore
                    };
                    let p = Pattern::parse(text, case, Normalization::Smart);
                    if p.atoms.is_empty() {
                        Kind::All
                    } else {
                        Kind::Fuzzy(Box::new(p))
                    }
                }
                MatchMode::Regex => Kind::Regex(build_regex(text, case_sensitive)?),
                MatchMode::Glob => Kind::Glob(
                    build_regex(&glob_to_regex(text), case_sensitive)?,
                    glob_literal(text, case_sensitive)
                        .map(|l| Box::new(memmem::Finder::new(l.as_bytes()).into_owned())),
                ),
            }
        };
        Ok(Self {
            kind,
            mode: query.mode,
            case_sensitive,
            needle,
            full_path: query.match_full_path,
        })
    }

    pub fn is_case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    pub fn needle(&self) -> &str {
        &self.needle
    }

    pub fn mode(&self) -> MatchMode {
        self.mode
    }

    /// Whether this query matches everything, letting callers skip matching entirely and
    /// go straight to filtering and sorting.
    pub fn matches_all(&self) -> bool {
        matches!(self.kind, Kind::All)
    }

    fn is_fuzzy(&self) -> bool {
        matches!(self.kind, Kind::Fuzzy(_))
    }

    /// The finder to run over a whole chunk of the name arena, when this query can use
    /// that fast path: matching names (not paths) in substring mode — the longest term,
    /// being the most selective — or glob mode with a required literal. It only selects
    /// candidates; every candidate is still fully tested, so this cannot change results.
    /// The arena searched is the folded one exactly when the query is case-insensitive.
    fn arena_finder(&self) -> Option<&memmem::Finder<'static>> {
        if self.full_path {
            return None;
        }
        match &self.kind {
            Kind::Substring(terms) => terms.iter().max_by_key(|f| f.needle().len()),
            Kind::Glob(_, lit) => lit.as_deref(),
            _ => None,
        }
    }

    /// Whether the hot loop should hand [`Matcher::test`] the folded haystack.
    fn wants_folded(&self) -> bool {
        matches!(self.kind, Kind::Substring(_)) && !self.case_sensitive
    }

    /// Convenience for tests and one-off callers: matches a name given in its original
    /// case, folding it if needed. The hot path tests pre-folded data directly.
    pub fn matches(&self, name: &str) -> bool {
        let folded;
        let hay = if self.wants_folded() {
            folded = fold_lower(name);
            folded.as_str()
        } else {
            name
        };
        let mut scratch = Scratch::new(self);
        self.test(hay.as_bytes(), name, &mut scratch).is_some()
    }

    /// Tests one candidate. `hay` is the folded text when [`Matcher::wants_folded`],
    /// otherwise the original; `original` is always the original. Returns the score
    /// (non-zero only for fuzzy) on a match.
    #[inline]
    fn test(&self, hay: &[u8], original: &str, s: &mut Scratch) -> Option<u32> {
        match &self.kind {
            Kind::All => Some(0),
            Kind::Substring(terms) => terms.iter().all(|f| f.find(hay).is_some()).then_some(0),
            Kind::Regex(re) | Kind::Glob(re, _) => re.is_match(original).then_some(0),
            Kind::Fuzzy(p) => {
                let m = s.nucleo.as_mut()?;
                let h = nucleo::Utf32Str::new(original, &mut s.chars);
                p.score(h, m)
            }
        }
    }

    /// Byte ranges of `original` to highlight, flattened as `[start, end, start, end, …]`
    /// half-open pairs, sorted and non-overlapping. Computed only for returned rows.
    fn ranges(&self, original: &str, folded: &str, s: &mut Scratch) -> Vec<u32> {
        let mut spans: Vec<(usize, usize)> = Vec::new();
        match &self.kind {
            Kind::All | Kind::Glob(..) => {}
            Kind::Substring(terms) => {
                let hay = if self.case_sensitive {
                    original
                } else {
                    folded
                };
                for f in terms {
                    if let Some(i) = f.find(hay.as_bytes()) {
                        spans.push((i, i + f.needle().len()));
                    }
                }
            }
            Kind::Regex(re) => {
                if let Some(m) = re.find(original) {
                    if m.start() != m.end() {
                        spans.push((m.start(), m.end()));
                    }
                }
            }
            Kind::Fuzzy(p) => {
                if let Some(m) = s.nucleo.as_mut() {
                    s.indices.clear();
                    let h = nucleo::Utf32Str::new(original, &mut s.chars);
                    if p.indices(h, m, &mut s.indices).is_some() {
                        s.indices.sort_unstable();
                        s.indices.dedup();
                        // nucleo reports char indices; the contract wants byte offsets.
                        let starts: Vec<(usize, usize)> = original
                            .char_indices()
                            .map(|(b, c)| (b, b + c.len_utf8()))
                            .collect();
                        for &ci in &s.indices {
                            if let Some(&(b, e)) = starts.get(ci as usize) {
                                spans.push((b, e));
                            }
                        }
                    }
                }
            }
        }
        merge_spans(spans)
    }
}

fn merge_spans(mut spans: Vec<(usize, usize)>) -> Vec<u32> {
    spans.sort_unstable();
    let mut out: Vec<u32> = Vec::with_capacity(spans.len() * 2);
    for (s, e) in spans {
        match out.len() {
            n if n >= 2 && s as u32 <= out[n - 1] => {
                if e as u32 > out[n - 1] {
                    out[n - 1] = e as u32;
                }
            }
            _ => {
                out.push(s as u32);
                out.push(e as u32);
            }
        }
    }
    out
}

/// Splits on whitespace, keeping `"quoted phrases"` intact.
fn split_terms(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                if !quoted && !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn build_regex(pattern: &str, case_sensitive: bool) -> Result<regex::Regex> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| FxError::BadQuery(e.to_string()))
}

/// Translates a shell glob into an anchored regex. `*` and `?` are the wildcards users
/// expect; `[abc]` / `[!abc]` classes pass through; everything else is literal. The
/// pattern must match the whole name, so `*.rs` does not match `foo.rs.bak`.
pub fn glob_to_regex(glob: &str) -> String {
    let mut re = String::with_capacity(glob.len() * 2 + 8);
    re.push_str("^(?s:");
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                // Collapse runs: `**` would otherwise compile to a pointless `.*.*`.
                while chars.peek() == Some(&'*') {
                    chars.next();
                }
                re.push_str(".*");
            }
            '?' => re.push('.'),
            '[' => {
                // Only treat as a class if it is closed; a lone `[` is a literal.
                let rest: String = chars.clone().collect();
                match rest.find(']') {
                    Some(end) if end > 0 || rest.starts_with("]") && rest[1..].contains(']') => {
                        let body: String = chars.by_ref().take(end).collect();
                        chars.next(); // the closing ']'
                        re.push('[');
                        let mut body = body.as_str();
                        if let Some(b) = body.strip_prefix('!') {
                            re.push('^');
                            body = b;
                        }
                        for bc in body.chars() {
                            if matches!(bc, '\\' | '[' | ']' | '&' | '~' | '^') {
                                re.push('\\');
                            }
                            re.push(bc);
                        }
                        re.push(']');
                    }
                    _ => re.push_str(&regex::escape("[")),
                }
            }
            c => re.push_str(&regex::escape(c.encode_utf8(&mut [0u8; 4]))),
        }
    }
    re.push_str(")$");
    re
}

/// The longest literal run in a glob — text no wildcard or class can stand in for — which
/// every matching name must therefore contain. `None` when there is no run worth searching
/// for (shorter than 2 bytes), or when a case-insensitive search for it could disagree
/// with the regex.
///
/// That last case is real: case-insensitive regex treats `K` (KELVIN SIGN) as `k` and `ſ`
/// (LONG S) as `s`, but the store's length-preserving fold leaves both untouched, so a
/// folded-arena search for `k` would miss a name the regex accepts. Case-insensitive
/// literals are therefore used only when ASCII and free of `k` and `s`; anything else
/// falls back to running the regex on every name, which is slower but never wrong.
pub fn glob_literal(glob: &str, case_sensitive: bool) -> Option<String> {
    let mut best = String::new();
    let mut cur = String::new();
    let mut chars = glob.chars().peekable();
    let flush = |cur: &mut String, best: &mut String| {
        if cur.len() > best.len() {
            *best = std::mem::take(cur);
        } else {
            cur.clear();
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '*' | '?' => flush(&mut cur, &mut best),
            '[' => {
                // Mirrors `glob_to_regex`: a closed bracket is a class (not literal), an
                // unclosed one is a literal `[`.
                let rest: String = chars.clone().collect();
                match rest.find(']') {
                    Some(end) if end > 0 || rest.starts_with(']') && rest[1..].contains(']') => {
                        flush(&mut cur, &mut best);
                        for _ in 0..=end {
                            chars.next();
                        }
                    }
                    _ => cur.push('['),
                }
            }
            c => cur.push(c),
        }
    }
    flush(&mut cur, &mut best);
    if best.len() < 2 {
        return None;
    }
    if case_sensitive {
        return Some(best);
    }
    let folded = fold_lower(&best);
    (folded.is_ascii() && !folded.contains(['k', 's'])).then_some(folded)
}

/// Whether an extension passes the filter. Empty filter means no constraint.
pub fn ext_allowed(filters: &Filters, ext: Option<&str>) -> bool {
    if filters.extensions.is_empty() {
        return true;
    }
    match ext {
        Some(e) => filters.extensions.iter().any(|f| f == e),
        None => false,
    }
}

/// Whether a size passes the filter.
pub fn size_allowed(filters: &Filters, size: u64) -> bool {
    match &filters.size {
        None => true,
        Some(r) => r.min.is_none_or(|m| size >= m) && r.max.is_none_or(|m| size <= m),
    }
}

/// Whether a timestamp passes the filter.
pub fn time_allowed(range: Option<&TimeRange>, when: i64) -> bool {
    match range {
        None => true,
        Some(r) => r.from.is_none_or(|f| when >= f) && r.to.is_none_or(|t| when <= t),
    }
}

/// The folded extension of a folded name, by the same rule as
/// [`fx_core::FileEntry::split_ext`]: leading-dot names and trailing dots have none.
#[inline]
fn ext_of(name: &[u8]) -> &[u8] {
    match memchr::memrchr(b'.', name) {
        Some(i) if i > 0 && i + 1 < name.len() => &name[i + 1..],
        _ => &[],
    }
}

/// Filters hoisted into flat comparisons.
#[derive(Debug)]
struct Prefilter {
    reject_flags: u16,
    /// Bit per `EntryKind`; 0 means "any".
    kinds: u8,
    exts: Vec<Vec<u8>>,
    size: (u64, u64),
    modified: (i64, i64),
    created: (i64, i64),
}

impl Prefilter {
    fn new(f: &Filters) -> Self {
        let mut reject_flags = flags::TOMBSTONE;
        if !f.include_hidden {
            reject_flags |= flags::HIDDEN;
        }
        if !f.include_system {
            reject_flags |= flags::SYSTEM;
        }
        let kinds = f.kinds.iter().fold(0u8, |m, k| m | (1 << kind_bits(*k)));
        let range = |r: Option<&TimeRange>| match r {
            None => (i64::MIN, i64::MAX),
            Some(r) => (r.from.unwrap_or(i64::MIN), r.to.unwrap_or(i64::MAX)),
        };
        Self {
            reject_flags,
            kinds,
            exts: f
                .extensions
                .iter()
                .map(|e| fold_lower(e.trim_start_matches('.')).into_bytes())
                .collect(),
            size: f.size.map_or((0, u64::MAX), |r| {
                (r.min.unwrap_or(0), r.max.unwrap_or(u64::MAX))
            }),
            modified: range(f.modified.as_ref()),
            created: range(f.created.as_ref()),
        }
    }

    #[inline]
    fn pass(&self, r: &Record, name_lower: &[u8]) -> bool {
        if r.flags & self.reject_flags != 0 {
            return false;
        }
        if self.kinds != 0 && self.kinds & (1 << (r.flags & flags::KIND_MASK)) == 0 {
            return false;
        }
        if r.size < self.size.0 || r.size > self.size.1 {
            return false;
        }
        if r.modified < self.modified.0 || r.modified > self.modified.1 {
            return false;
        }
        if r.created < self.created.0 || r.created > self.created.1 {
            return false;
        }
        if !self.exts.is_empty() {
            if r.is_container() {
                return false;
            }
            let e = ext_of(name_lower);
            if !self.exts.iter().any(|x| x.as_slice() == e) {
                return false;
            }
        }
        true
    }
}

/// What part of a store a query covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Coverage {
    Skip,
    Whole,
    Under(RecordIx),
}

/// Path split into folded components, accepting either separator. Comparisons are
/// case-insensitive because both target filesystems (NTFS, default APFS) are.
fn components(path: &str) -> Vec<String> {
    path.split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
        .map(fold_lower)
        .collect()
}

/// Whether a store belongs to a volume id. MFT stores carry the drive letter directly;
/// walk stores are keyed by root path, so a volume id like `C` or `/` matches them by
/// mount prefix.
pub fn store_on_volume(store: &EntryStore, volume: &str) -> bool {
    if store.volume == volume {
        return true;
    }
    let v = volume.trim_end_matches([':', '\\', '/']);
    if v.is_empty() {
        // "/" — the Unix root volume holds every absolute path.
        return store.mount.starts_with('/');
    }
    let m = components(&store.mount);
    let vc = components(volume);
    m.first().is_some_and(|first| {
        vc.first()
            .is_some_and(|f| first.trim_end_matches(':') == f.trim_end_matches(':'))
    })
}

fn coverage(store: &EntryStore, scope: &SearchScope) -> Coverage {
    match scope {
        SearchScope::All => Coverage::Whole,
        SearchScope::Volume(v) => {
            if store_on_volume(store, v) {
                Coverage::Whole
            } else {
                Coverage::Skip
            }
        }
        SearchScope::Subtree(path) => {
            let want = components(path);
            let mount = components(&store.mount);
            if want.len() <= mount.len() {
                // Scope is the mount itself or above it: the whole store is inside.
                if mount.starts_with(&want) {
                    Coverage::Whole
                } else {
                    Coverage::Skip
                }
            } else if want.starts_with(&mount) {
                match resolve_dir(store, &want[mount.len()..]) {
                    Some(ix) => Coverage::Under(ix),
                    None => Coverage::Skip,
                }
            } else {
                Coverage::Skip
            }
        }
    }
}

/// Finds the directory at `rel` (folded components below the mount). Works without a
/// child index: candidates are found by name in one parallel pass, then each is verified
/// by walking its parents. Run once per query, so O(n) is acceptable.
fn resolve_dir(store: &EntryStore, rel: &[String]) -> Option<RecordIx> {
    let last = rel.last()?.as_bytes();
    (0..store.records.len())
        .into_par_iter()
        .filter(|&ix| {
            let r = &store.records[ix];
            !r.is_tombstone() && r.is_container() && store.name_lower_bytes(r) == last
        })
        .find_any(|&ix| {
            let mut cur = ix as RecordIx;
            for comp in rel.iter().rev() {
                if cur == NO_RECORD || Some(cur) == store.root() {
                    return false;
                }
                let r = store.record(cur);
                if store.name_lower_bytes(r) != comp.as_bytes() {
                    return false;
                }
                cur = r.parent;
            }
            cur == store.root().unwrap_or(NO_RECORD)
        })
        .map(|ix| ix as RecordIx)
}

/// One match awaiting ranking.
#[derive(Debug, Clone, Copy)]
struct Cand {
    store: u32,
    ix: RecordIx,
    score: u32,
    /// Index into the precomputed path keys, for [`SortKey::Path`] only.
    key: u32,
}

/// Explorer-style "natural" ordering: digit runs compare by numeric value, so `file2`
/// sorts before `file10`. Operates on folded bytes.
pub fn natural_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (ca, cb) = (a[i], b[j]);
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let na = trim_zeros(&a[si..i]);
            let nb = trim_zeros(&b[sj..j]);
            let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
            if ord != Ordering::Equal {
                return ord;
            }
            // Equal value: fewer leading zeros first, so `1` < `01`.
            let ord = (i - si).cmp(&(j - sj));
            if ord != Ordering::Equal {
                return ord;
            }
        } else {
            if ca != cb {
                return ca.cmp(&cb);
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn trim_zeros(d: &[u8]) -> &[u8] {
    let n = d.iter().take_while(|&&c| c == b'0').count();
    &d[n..]
}

fn kind_rank(k: EntryKind) -> u8 {
    match k {
        EntryKind::Volume => 0,
        EntryKind::Dir => 1,
        EntryKind::Symlink => 2,
        EntryKind::File => 3,
        EntryKind::Unknown => 4,
    }
}

struct Ranker<'a> {
    stores: &'a [&'a EntryStore],
    sort: Sort,
    fuzzy: bool,
    keys: Vec<String>,
}

impl Ranker<'_> {
    #[inline]
    fn rec(&self, c: &Cand) -> &Record {
        self.stores[c.store as usize].record(c.ix)
    }

    #[inline]
    fn lower(&self, c: &Cand) -> &[u8] {
        let s = self.stores[c.store as usize];
        s.name_lower_bytes(s.record(c.ix))
    }

    fn cmp(&self, a: &Cand, b: &Cand) -> Ordering {
        let (ra, rb) = (self.rec(a), self.rec(b));
        if self.sort.dirs_first {
            let (da, db) = (ra.is_container(), rb.is_container());
            if da != db {
                return db.cmp(&da);
            }
        }
        let name = || natural_cmp(self.lower(a), self.lower(b));
        let ord = match self.sort.key {
            SortKey::Name => name(),
            SortKey::Size => ra.size.cmp(&rb.size),
            SortKey::Modified => ra.modified.cmp(&rb.modified),
            SortKey::Created => ra.created.cmp(&rb.created),
            SortKey::Kind => kind_rank(ra.kind())
                .cmp(&kind_rank(rb.kind()))
                .then_with(|| natural_cmp(ext_of(self.lower(a)), ext_of(self.lower(b)))),
            SortKey::Extension => {
                let ea = if ra.is_container() {
                    &[][..]
                } else {
                    ext_of(self.lower(a))
                };
                let eb = if rb.is_container() {
                    &[][..]
                } else {
                    ext_of(self.lower(b))
                };
                natural_cmp(ea, eb)
            }
            SortKey::Path => natural_cmp(
                self.keys[a.key as usize].as_bytes(),
                self.keys[b.key as usize].as_bytes(),
            ),
            // "Ascending" relevance means best first: nobody wants the worst match on top.
            SortKey::Relevance if self.fuzzy => b.score.cmp(&a.score),
            SortKey::Relevance => name(),
        };
        let ord = if self.sort.desc { ord.reverse() } else { ord };
        // Deterministic tie-breaks so paging through results never shows a row twice.
        ord.then_with(name)
            .then_with(|| {
                let sa = self.stores[a.store as usize];
                let sb = self.stores[b.store as usize];
                sa.name_bytes(ra).cmp(sb.name_bytes(rb))
            })
            .then_with(|| (a.store, a.ix).cmp(&(b.store, b.ix)))
    }

    /// Keeps the best `n` of `v`, sorted. O(len) selection plus O(n log n) sort.
    fn top(&self, v: &mut Vec<Cand>, n: usize) {
        if n == 0 {
            v.clear();
            return;
        }
        if v.len() > n {
            v.select_nth_unstable_by(n - 1, |a, b| self.cmp(a, b));
            v.truncate(n);
        }
        v.sort_unstable_by(|a, b| self.cmp(a, b));
    }

    fn trim(&self, v: &mut Vec<Cand>, n: usize) {
        if n > 0 && v.len() > n {
            v.select_nth_unstable_by(n - 1, |a, b| self.cmp(a, b));
            v.truncate(n);
        }
    }
}

/// Directory paths a worker caches before starting over. Bounds memory at a few MB per
/// worker even on volumes with millions of directories.
const DIR_CACHE_MAX: usize = 1 << 16;

/// Builds the path of `ix` into `scratch.path` from its parent's cached path, so full-path
/// matching costs roughly one string append per candidate rather than a tree walk.
fn candidate_path(store: &EntryStore, sidx: usize, ix: RecordIx, lower: bool, s: &mut Scratch) {
    let r = store.record(ix);
    if r.parent == NO_RECORD {
        store.write_path(ix, lower, &mut s.path);
        return;
    }
    let key = (sidx, r.parent);
    if s.parent_key != key {
        if let Some(p) = s.dir_paths.get(&key) {
            s.parent_path.clear();
            s.parent_path.push_str(p);
        } else {
            store.write_path(r.parent, lower, &mut s.parent_path);
            if s.dir_paths.len() >= DIR_CACHE_MAX {
                s.dir_paths.clear();
            }
            s.dir_paths
                .insert(key, s.parent_path.clone().into_boxed_str());
        }
        s.parent_key = key;
    }
    s.path.clear();
    s.path.push_str(&s.parent_path);
    if !s.path.ends_with(store.sep) {
        s.path.push(store.sep);
    }
    let bytes = if lower {
        store.name_lower_bytes(r)
    } else {
        store.name_bytes(r)
    };
    s.path.push_str(std::str::from_utf8(bytes).unwrap_or(""));
}

/// Scans one chunk of one store, returning its candidates (trimmed to `keep` when
/// `keep > 0`) and the untrimmed match count.
#[allow(clippy::too_many_arguments)]
fn scan_chunk(
    store: &EntryStore,
    sidx: usize,
    range: std::ops::Range<usize>,
    m: &Matcher,
    pf: &Prefilter,
    cov: Coverage,
    ranker: &Ranker<'_>,
    keep: usize,
    s: &mut Scratch,
) -> (Vec<Cand>, u64) {
    let mut out = Vec::new();
    let mut count = 0u64;
    let root = store.root().unwrap_or(NO_RECORD);
    let folded = m.wants_folded();
    let mut sub_cache: (RecordIx, bool) = (NO_RECORD, false);

    // Full evaluation of one record: prefilters, match, scope. Every path into the result
    // set goes through here, so the arena fast path below cannot change semantics — it
    // only decides which records are worth considering.
    let mut consider = |ix: usize, s: &mut Scratch| {
        let r = &store.records[ix];
        let lower = store.name_lower_bytes(r);
        if !pf.pass(r, lower) || ix as RecordIx == root {
            return;
        }

        let score = if m.matches_all() {
            0
        } else if m.full_path {
            candidate_path(store, sidx, ix as RecordIx, folded, s);
            let path = std::mem::take(&mut s.path);
            let res = m.test(path.as_bytes(), &path, s);
            s.path = path;
            match res {
                Some(sc) => sc,
                None => return,
            }
        } else {
            let hay = if folded { lower } else { store.name_bytes(r) };
            let orig = if folded {
                ""
            } else {
                std::str::from_utf8(store.name_bytes(r)).unwrap_or("")
            };
            match m.test(hay, orig, s) {
                Some(sc) => sc,
                None => return,
            }
        };

        if let Coverage::Under(top) = cov {
            let inside = if r.parent == sub_cache.0 {
                sub_cache.1
            } else {
                let v = r.parent == top || store.is_descendant(ix as RecordIx, top);
                sub_cache = (r.parent, v);
                v
            };
            if !inside {
                return;
            }
        }

        count += 1;
        out.push(Cand {
            store: sidx as u32,
            ix: ix as RecordIx,
            score,
            key: 0,
        });
        if keep > 0 && out.len() >= keep * 2 {
            ranker.trim(&mut out, keep);
        }
    };

    match m.arena_finder() {
        // Substring on names, and this chunk's names are back to back in the arena: one
        // SIMD search over the whole slice finds candidates at memory bandwidth, and
        // records with no hit are never touched. Per-name `find` calls cost more in setup
        // than in searching, since most names are shorter than a vector register's worth
        // of work.
        Some(finder) if !range.is_empty() && store.chunk_is_contiguous(range.start / CHUNK) => {
            let arena: &[u8] = if !m.case_sensitive {
                &store.names_lower
            } else {
                &store.names
            };
            let end_of = |ix: usize| {
                let r = &store.records[ix];
                r.name_off as usize + r.name_len as usize
            };
            let lo = store.records[range.start].name_off as usize;
            let hi = end_of(range.end - 1);
            let needle_len = finder.needle().len();
            let mut ix = range.start;
            let mut pos = lo;
            while pos < hi {
                let Some(off) = finder.find(&arena[pos..hi]) else {
                    break;
                };
                let at = pos + off;
                // Advance to the record whose name contains `at`. Records are in arena
                // order within a contiguous chunk, so this is a forward walk, never a search.
                while ix < range.end && end_of(ix) <= at {
                    ix += 1;
                }
                if ix >= range.end {
                    break;
                }
                // A hit straddling into the next name is an artefact of concatenation, not
                // a match. Either way this record is settled: any later occurrence inside
                // it would straddle too, so resume at the next name.
                if at + needle_len <= end_of(ix) {
                    consider(ix, s);
                }
                pos = end_of(ix);
                ix += 1;
            }
        }
        _ => {
            for ix in range {
                consider(ix, s);
            }
        }
    }

    if keep > 0 {
        ranker.trim(&mut out, keep);
    }
    (out, count)
}

/// Answers `query` over `stores`, streaming hits into `sink` in
/// [`Query::BATCH_SIZE`] batches. `degraded` is passed through to the summary so the UI
/// can tell a walk-backed answer from an MFT one.
pub fn execute(
    stores: &[&EntryStore],
    query: &Query,
    sink: &dyn BatchSink<SearchHit>,
    degraded: bool,
) -> Result<QuerySummary> {
    let started = Instant::now();
    let m = Matcher::new(query)?;
    let pf = Prefilter::new(&query.filters);
    let offset = query.offset as usize;
    let want = offset.saturating_add(query.effective_limit() as usize);
    let path_sort = query.sort.key == SortKey::Path;

    let mut ranker = Ranker {
        stores,
        sort: query.sort,
        fuzzy: m.is_fuzzy(),
        keys: Vec::new(),
    };
    // Path sort needs every candidate's path before ranking, so per-chunk trimming (which
    // ranks) is disabled for it.
    let keep = if path_sort { 0 } else { want };

    let mut cands: Vec<Cand> = Vec::new();
    let mut total: u64 = 0;
    for (sidx, store) in stores.iter().enumerate() {
        let cov = coverage(store, &query.scope);
        if cov == Coverage::Skip {
            continue;
        }
        let n = store.records.len();
        let chunks: Vec<_> = (0..n.div_ceil(CHUNK))
            .map(|c| c * CHUNK..((c + 1) * CHUNK).min(n))
            .collect();
        let results: Vec<(Vec<Cand>, u64)> = chunks
            .into_par_iter()
            .map_init(
                || Scratch::new(&m),
                |s, range| scan_chunk(store, sidx, range, &m, &pf, cov, &ranker, keep, s),
            )
            .collect();
        for (v, c) in results {
            total += c;
            cands.extend(v);
            if keep > 0 && cands.len() >= keep * 2 {
                ranker.trim(&mut cands, keep);
            }
        }
    }

    if path_sort {
        ranker.keys = cands
            .par_iter()
            .map(|c| {
                let mut s = String::new();
                stores[c.store as usize].write_path(c.ix, true, &mut s);
                s
            })
            .collect();
        for (i, c) in cands.iter_mut().enumerate() {
            c.key = i as u32;
        }
    }
    ranker.top(&mut cands, want);

    let page = cands.get(offset.min(cands.len())..).unwrap_or(&[]);
    sink.hint_total(total);

    // Materialise only the surviving rows: entry, path, highlight ranges.
    let mut scratch = Scratch::new(&m);
    let mut batch = Vec::with_capacity(page.len().min(Query::BATCH_SIZE));
    let mut returned = 0u64;
    for c in page {
        let store = stores[c.store as usize];
        let entry = store.to_entry(c.ix);
        let path = store.path_of(c.ix);
        let match_ranges = if m.full_path {
            m.ranges(&path, &fold_lower(&path), &mut scratch)
        } else {
            let folded = std::str::from_utf8(store.name_lower_bytes(store.record(c.ix)))
                .unwrap_or("")
                .to_owned();
            m.ranges(&entry.name, &folded, &mut scratch)
        };
        batch.push(SearchHit {
            entry,
            volume: store.volume.clone(),
            path,
            score: c.score,
            match_ranges,
        });
        returned += 1;
        if batch.len() == Query::BATCH_SIZE && !sink.push(std::mem::take(&mut batch)) {
            return Err(FxError::Cancelled);
        }
    }
    if !batch.is_empty() && !sink.push(batch) {
        return Err(FxError::Cancelled);
    }

    Ok(QuerySummary {
        returned,
        total_matched: total,
        elapsed_micros: started.elapsed().as_micros() as u64,
        truncated: false,
        degraded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Meta;
    use fx_core::{Attributes, CaseMode, CollectSink, SizeRange};

    #[test]
    fn empty_query_matches_everything() {
        let m = Matcher::new(&Query::default()).unwrap();
        assert!(m.matches_all());
        assert!(m.matches("anything at all"));
    }

    #[test]
    fn substring_match_respects_smart_case() {
        let m = Matcher::new(&Query {
            text: "readme".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(!m.is_case_sensitive());
        assert!(m.matches("README.md"));

        // Uppercase present: case-sensitive, needle kept verbatim.
        let m = Matcher::new(&Query {
            text: "ReadMe".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(m.is_case_sensitive());
        assert_eq!(m.needle(), "ReadMe");
        assert!(m.matches("ReadMe.md"));
        assert!(!m.matches("readme.md"));
    }

    #[test]
    fn empty_filters_are_permissive_not_restrictive() {
        let f = Filters::default();
        assert!(ext_allowed(&f, Some("rs")));
        assert!(ext_allowed(&f, None));
        assert!(size_allowed(&f, 0));
        assert!(size_allowed(&f, u64::MAX));
    }

    #[test]
    fn extension_filter_excludes_extensionless_files() {
        let f = Filters {
            extensions: vec!["rs".into(), "toml".into()],
            ..Default::default()
        };
        assert!(ext_allowed(&f, Some("rs")));
        assert!(!ext_allowed(&f, Some("md")));
        assert!(!ext_allowed(&f, None));
    }

    #[test]
    fn size_and_time_ranges_are_inclusive_and_open_ended() {
        let f = Filters {
            size: Some(SizeRange {
                min: Some(100),
                max: Some(200),
            }),
            ..Default::default()
        };
        assert!(!size_allowed(&f, 99));
        assert!(size_allowed(&f, 100));
        assert!(size_allowed(&f, 200));
        assert!(!size_allowed(&f, 201));

        let r = TimeRange {
            from: Some(1_000),
            to: None,
        };
        assert!(time_allowed(Some(&r), 1_000));
        assert!(!time_allowed(Some(&r), 999));
        assert!(time_allowed(None, 0));
    }

    #[test]
    fn case_modes_override_smart_detection() {
        let forced = Query {
            text: "ABC".into(),
            case: CaseMode::Insensitive,
            ..Default::default()
        };
        assert!(!Matcher::new(&forced).unwrap().is_case_sensitive());
        let forced = Query {
            text: "abc".into(),
            case: CaseMode::Sensitive,
            ..Default::default()
        };
        assert!(Matcher::new(&forced).unwrap().is_case_sensitive());
    }

    #[test]
    fn substring_terms_are_anded_and_quotes_group() {
        let m = Matcher::new(&Query {
            text: "foo bar".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(m.matches("bar_foo.txt"));
        assert!(!m.matches("foo.txt"));

        let m = Matcher::new(&Query {
            text: "\"foo bar\"".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(m.matches("my foo bar.txt"));
        assert!(!m.matches("bar foo.txt"));
        assert_eq!(split_terms("a  \"b c\" d"), vec!["a", "b c", "d"]);
    }

    #[test]
    fn glob_is_anchored_and_supports_classes() {
        let q = |t: &str| {
            Matcher::new(&Query {
                text: t.into(),
                mode: MatchMode::Glob,
                ..Default::default()
            })
            .unwrap()
        };
        assert!(q("*.rs").matches("main.rs"));
        assert!(
            q("*.rs").matches("MAIN.RS"),
            "lowercase glob is case-insensitive"
        );
        assert!(!q("*.rs").matches("main.rs.bak"));
        assert!(q("file?.txt").matches("file1.txt"));
        assert!(!q("file?.txt").matches("file10.txt"));
        assert!(q("[ab]*").matches("apple"));
        assert!(!q("[!ab]*").matches("apple"));
        // Regex metacharacters in a glob are literal.
        assert!(q("a+b(1).txt").matches("a+b(1).txt"));
        assert!(!q("a+b(1).txt").matches("aab1.txt"));
        // An unclosed bracket is a literal, not an error.
        assert!(q("[x").matches("[x"));
    }

    #[test]
    fn glob_literals_are_the_longest_required_run() {
        assert_eq!(
            glob_literal("invoice_*.pdf", true).as_deref(),
            Some("invoice_")
        );
        assert_eq!(glob_literal("*.tar.gz", true).as_deref(), Some(".tar.gz"));
        assert_eq!(glob_literal("a?bcd[xy]ef", true).as_deref(), Some("bcd"));
        assert_eq!(
            glob_literal("[x", true).as_deref(),
            Some("[x"),
            "unclosed is literal"
        );
        assert_eq!(glob_literal("*a*", true), None, "too short to be worth it");
        assert_eq!(glob_literal("*.PDF", false).as_deref(), Some(".pdf"));
        // Case-insensitive literals that regex folding could match differently: skipped.
        assert_eq!(glob_literal("*.mkv", false), None, "k matches KELVIN SIGN");
        assert_eq!(glob_literal("*.css", false), None, "s matches LONG S");
        assert_eq!(glob_literal("*.mkv", true).as_deref(), Some(".mkv"));
    }

    #[test]
    fn glob_fast_path_agrees_with_regex_on_every_name() {
        let mut s = EntryStore::with_mount("v".into(), "/v", '/');
        let root = s.push(0, NO_RECORD, "v", Meta::dir());
        s.set_root(root);
        let names = [
            "invoice_1.pdf",
            "INVOICE_2.PDF",
            "invoice_.pdf",
            "xinvoice_3.pdf",
            "invoice_4.pdf.bak",
            "invoice",
            "_.pdf",
            "invoice_5.pdfinvoice_",
        ];
        for i in 1..(CHUNK as u64 * 2 + 50) {
            s.push(i, root, names[i as usize % names.len()], Meta::file(i));
        }
        for i in (CHUNK as u64..CHUNK as u64 + 40).step_by(7) {
            s.rename(i, 0, "invoice_9.pdf");
        }
        for glob in ["invoice_*.pdf", "INVOICE_*", "*_?.pdf", "*voice_[0-9]*"] {
            let mut query = q(glob);
            query.mode = MatchMode::Glob;
            query.limit = u32::MAX;
            let sink = CollectSink::new();
            execute(&[&s], &query, &sink, false).unwrap();
            let got = sink.into_inner().len();
            let m = Matcher::new(&query).unwrap();
            let want = (1..s.len() as RecordIx)
                .filter(|&ix| m.matches(s.name(ix)))
                .count();
            assert_eq!(got, want, "glob {glob:?}");
        }
    }

    #[test]
    fn regex_mode_compiles_once_and_reports_bad_patterns() {
        let m = Matcher::new(&Query {
            text: r"^\d{4}-report".into(),
            mode: MatchMode::Regex,
            ..Default::default()
        })
        .unwrap();
        assert!(m.matches("2024-report.pdf"));
        assert!(!m.matches("report-2024.pdf"));

        let bad = Matcher::new(&Query {
            text: "(unclosed".into(),
            mode: MatchMode::Regex,
            ..Default::default()
        });
        assert!(matches!(bad, Err(FxError::BadQuery(_))));
    }

    #[test]
    fn oversized_queries_are_rejected() {
        let q = Query {
            text: "a".repeat(MAX_QUERY_TEXT + 1),
            ..Default::default()
        };
        assert!(matches!(Matcher::new(&q), Err(FxError::BadQuery(_))));
    }

    #[test]
    fn fuzzy_matches_subsequences() {
        let m = Matcher::new(&Query {
            text: "mnrs".into(),
            mode: MatchMode::Fuzzy,
            ..Default::default()
        })
        .unwrap();
        assert!(m.matches("main.rs"));
        assert!(!m.matches("cargo.toml"));
    }

    #[test]
    fn natural_order_compares_numbers_by_value() {
        let mut v = vec!["file10", "file2", "file1", "File01", "file", "a"];
        v.sort_by(|a, b| natural_cmp(fold_lower(a).as_bytes(), fold_lower(b).as_bytes()));
        assert_eq!(v, vec!["a", "file", "file1", "File01", "file2", "file10"]);
        assert_eq!(natural_cmp(b"x9", b"x10"), Ordering::Less);
        assert_eq!(natural_cmp(b"abc", b"abc"), Ordering::Equal);
    }

    #[test]
    fn merge_spans_flattens_and_coalesces() {
        assert_eq!(merge_spans(vec![(4, 6), (0, 2), (1, 3)]), vec![0, 3, 4, 6]);
        assert_eq!(merge_spans(vec![(0, 1), (1, 2)]), vec![0, 2]);
        assert!(merge_spans(vec![]).is_empty());
    }

    // ---- end-to-end over a store ----

    struct Fixture {
        store: EntryStore,
    }

    fn attrs_hidden() -> Attributes {
        Attributes {
            hidden: true,
            ..Default::default()
        }
    }

    /// /home
    ///   docs/                 dir
    ///     Report2.pdf         200 B, modified 2000
    ///     report10.pdf        100 B, modified 3000
    ///     notes.txt           50 B,  modified 1000
    ///   src/                  dir
    ///     main.rs             300 B
    ///     lib.rs              10 B
    ///     reports/            dir
    ///       q1.csv            5 B
    ///   .secret.txt           hidden
    ///   sys.dat               system
    ///   README                no extension
    fn fixture() -> Fixture {
        let mut s = EntryStore::with_mount("home".into(), "/home", '/');
        let root = s.push(1, NO_RECORD, "home", Meta::dir());
        s.set_root(root);
        let docs = s.push(2, root, "docs", Meta::dir());
        let f = |size, m| Meta::new(EntryKind::File, &Attributes::default(), size, m, m / 2);
        s.push(3, docs, "Report2.pdf", f(200, 2000));
        s.push(4, docs, "report10.pdf", f(100, 3000));
        s.push(5, docs, "notes.txt", f(50, 1000));
        let src = s.push(6, root, "src", Meta::dir());
        s.push(7, src, "main.rs", f(300, 500));
        s.push(8, src, "lib.rs", f(10, 600));
        let reports = s.push(9, src, "reports", Meta::dir());
        s.push(10, reports, "q1.csv", f(5, 700));
        s.push(
            11,
            root,
            ".secret.txt",
            Meta::new(EntryKind::File, &attrs_hidden(), 1, 1, 1),
        );
        s.push(
            12,
            root,
            "sys.dat",
            Meta::new(
                EntryKind::File,
                &Attributes {
                    system: true,
                    ..Default::default()
                },
                1,
                1,
                1,
            ),
        );
        s.push(13, root, "README", f(1, 1));
        Fixture { store: s }
    }

    fn run(fx: &Fixture, q: Query) -> (Vec<SearchHit>, QuerySummary) {
        let sink = CollectSink::new();
        let sum = execute(&[&fx.store], &q, &sink, false).unwrap();
        (sink.into_inner(), sum)
    }

    fn names(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|h| h.entry.name.as_str()).collect()
    }

    fn q(text: &str) -> Query {
        Query {
            text: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn substring_query_finds_names_case_insensitively_with_paths() {
        let fx = fixture();
        let (hits, sum) = run(&fx, q("report"));
        // dirs first, then natural name order: report10 after Report2.
        assert_eq!(names(&hits), vec!["reports", "Report2.pdf", "report10.pdf"]);
        assert_eq!(sum.total_matched, 3);
        assert_eq!(sum.returned, 3);
        assert!(!sum.degraded);
        assert_eq!(hits[1].path, "/home/docs/Report2.pdf");
        assert_eq!(hits[1].volume, "home");
        assert_eq!(hits[1].match_ranges, vec![0, 6]);
        assert_eq!(hits[0].entry.kind, EntryKind::Dir);
    }

    #[test]
    fn root_tombstones_hidden_and_system_are_excluded_by_default() {
        let fx = fixture();
        let (hits, _) = run(&fx, q(""));
        let n = names(&hits);
        assert!(!n.contains(&"home"), "the mount itself is never a result");
        assert!(!n.contains(&".secret.txt"));
        assert!(!n.contains(&"sys.dat"));
        assert_eq!(n.len(), 10);

        let mut with = q("");
        with.filters.include_hidden = true;
        with.filters.include_system = true;
        let (hits, _) = run(&fx, with);
        assert_eq!(hits.len(), 12);

        let mut fx = fixture();
        fx.store.remove(7);
        let (hits, _) = run(&fx, q("main"));
        assert!(hits.is_empty(), "tombstoned entries must not be returned");
    }

    #[test]
    fn filters_combine_kind_extension_size_and_dates() {
        let fx = fixture();
        let mut query = q("");
        query.filters.extensions = vec!["PDF".into()];
        let (hits, _) = run(&fx, query.clone());
        assert_eq!(names(&hits), vec!["Report2.pdf", "report10.pdf"]);

        query.filters.size = Some(SizeRange {
            min: Some(150),
            max: None,
        });
        let (hits, _) = run(&fx, query);
        assert_eq!(names(&hits), vec!["Report2.pdf"]);

        let mut query = q("");
        query.filters.kinds = vec![EntryKind::Dir];
        let (hits, _) = run(&fx, query);
        assert_eq!(names(&hits), vec!["docs", "reports", "src"]);

        let mut query = q("");
        query.filters.modified = Some(TimeRange {
            from: Some(1000),
            to: Some(2000),
        });
        let (hits, _) = run(&fx, query);
        assert_eq!(names(&hits), vec!["notes.txt", "Report2.pdf"]);

        let mut query = q("");
        query.filters.created = Some(TimeRange {
            from: Some(1500),
            to: None,
        });
        let (hits, _) = run(&fx, query);
        assert_eq!(names(&hits), vec!["report10.pdf"]);
    }

    #[test]
    fn extension_filter_never_matches_directories_or_extensionless_files() {
        let mut fx = fixture();
        fx.store.push(20, 0, "archive.pdf", Meta::dir());
        let mut query = q("");
        query.filters.extensions = vec!["pdf".into()];
        let (hits, _) = run(&fx, query);
        assert!(!names(&hits).contains(&"archive.pdf"));
        assert!(!names(&hits).contains(&"README"));
    }

    #[test]
    fn every_sort_key_orders_as_documented() {
        let fx = fixture();
        let sorted = |key, desc, dirs_first| {
            let mut query = q("");
            query.filters.kinds = vec![EntryKind::File];
            query.sort = Sort {
                key,
                desc,
                dirs_first,
            };
            let (hits, _) = run(&fx, query);
            hits.into_iter().map(|h| h.entry.name).collect::<Vec<_>>()
        };
        assert_eq!(
            sorted(SortKey::Size, false, true),
            vec![
                "README",
                "q1.csv",
                "lib.rs",
                "notes.txt",
                "report10.pdf",
                "Report2.pdf",
                "main.rs"
            ]
        );
        assert_eq!(sorted(SortKey::Size, true, true)[0], "main.rs");
        assert_eq!(sorted(SortKey::Modified, true, true)[0], "report10.pdf");
        assert_eq!(sorted(SortKey::Created, false, true)[0], "README");
        assert_eq!(
            sorted(SortKey::Extension, false, true),
            // "" < csv < pdf < pdf < rs < rs < txt, ties by name.
            vec![
                "README",
                "q1.csv",
                "Report2.pdf",
                "report10.pdf",
                "lib.rs",
                "main.rs",
                "notes.txt"
            ]
        );
        assert_eq!(
            sorted(SortKey::Path, false, true),
            vec![
                "notes.txt",
                "Report2.pdf",
                "report10.pdf",
                "README",
                "lib.rs",
                "main.rs",
                "q1.csv"
            ]
        );
        // Relevance outside fuzzy mode falls back to name.
        assert_eq!(
            sorted(SortKey::Relevance, false, true),
            sorted(SortKey::Name, false, true)
        );
    }

    #[test]
    fn dirs_first_groups_containers_regardless_of_key_and_direction() {
        let fx = fixture();
        let mut query = q("r");
        query.sort = Sort {
            key: SortKey::Name,
            desc: true,
            dirs_first: true,
        };
        let (hits, _) = run(&fx, query.clone());
        let kinds: Vec<bool> = hits.iter().map(|h| h.entry.kind.is_container()).collect();
        let first_file = kinds.iter().position(|d| !d).unwrap();
        assert!(kinds[..first_file].iter().all(|d| *d));
        assert!(kinds[first_file..].iter().all(|d| !*d));

        query.sort.dirs_first = false;
        query.sort.desc = false;
        let (hits, _) = run(&fx, query);
        // Pure name order interleaves dirs and files.
        assert_eq!(
            names(&hits),
            vec![
                "lib.rs",
                "main.rs",
                "README",
                "Report2.pdf",
                "report10.pdf",
                "reports",
                "src"
            ]
        );
    }

    #[test]
    fn limit_and_offset_page_through_a_stable_order() {
        let fx = fixture();
        let (all, _) = run(&fx, q(""));
        let mut paged = Vec::new();
        for page in 0..4 {
            let mut query = q("");
            query.limit = 3;
            query.offset = page * 3;
            let (hits, sum) = run(&fx, query);
            assert_eq!(sum.total_matched, all.len() as u64);
            paged.extend(hits);
        }
        assert_eq!(names(&paged), names(&all));

        let mut query = q("");
        query.offset = 1000;
        let (hits, sum) = run(&fx, query);
        assert!(hits.is_empty());
        assert_eq!(sum.returned, 0);
    }

    #[test]
    fn subtree_scope_restricts_to_descendants() {
        let fx = fixture();
        let mut query = q("");
        query.scope = SearchScope::Subtree("/home/SRC/".into());
        let (hits, _) = run(&fx, query.clone());
        assert_eq!(names(&hits), vec!["reports", "lib.rs", "main.rs", "q1.csv"]);

        query.scope = SearchScope::Subtree("/home/src/reports".into());
        let (hits, _) = run(&fx, query.clone());
        assert_eq!(names(&hits), vec!["q1.csv"]);

        query.scope = SearchScope::Subtree("/elsewhere".into());
        assert!(run(&fx, query.clone()).0.is_empty());

        query.scope = SearchScope::Subtree("/".into());
        assert_eq!(run(&fx, query.clone()).0.len(), 10);

        query.scope = SearchScope::Subtree("/home/nope".into());
        assert!(run(&fx, query).0.is_empty());
    }

    #[test]
    fn volume_scope_selects_stores() {
        let fx = fixture();
        let mut query = q("");
        query.scope = SearchScope::Volume("home".into());
        assert_eq!(run(&fx, query.clone()).0.len(), 10);
        query.scope = SearchScope::Volume("/".into());
        assert_eq!(run(&fx, query.clone()).0.len(), 10);
        query.scope = SearchScope::Volume("D".into());
        assert!(run(&fx, query).0.is_empty());

        let c = EntryStore::with_mount("C".into(), r"C:\", '\\');
        assert!(store_on_volume(&c, "C"));
        let walk = EntryStore::with_mount(r"C:\Users\me".into(), r"C:\Users\me", '\\');
        assert!(store_on_volume(&walk, "C"));
        assert!(store_on_volume(&walk, r"C:\"));
        assert!(!store_on_volume(&walk, "D"));
    }

    #[test]
    fn full_path_matching_reports_ranges_into_the_path() {
        let fx = fixture();
        let mut query = q("src/rep");
        query.match_full_path = true;
        let (hits, _) = run(&fx, query);
        // The directory matches, and so does everything beneath it: its path contains the
        // needle too. Same behaviour as Everything's path matching.
        assert_eq!(names(&hits), vec!["reports", "q1.csv"]);
        assert_eq!(hits[0].path, "/home/src/reports");
        assert_eq!(hits[1].path, "/home/src/reports/q1.csv");
        for h in &hits {
            let (s, e) = (h.match_ranges[0] as usize, h.match_ranges[1] as usize);
            assert_eq!(&h.path[s..e], "src/rep");
        }

        // Without full-path matching, directory names are not part of the haystack.
        let (hits, _) = run(&fx, q("src/rep"));
        assert!(hits.is_empty());
    }

    #[test]
    fn fuzzy_ranks_by_score_and_reports_byte_ranges() {
        let fx = fixture();
        let mut query = q("rprt");
        query.mode = MatchMode::Fuzzy;
        query.sort = Sort {
            key: SortKey::Relevance,
            desc: false,
            dirs_first: false,
        };
        let (hits, _) = run(&fx, query);
        assert!(hits.len() >= 3);
        assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
        assert!(hits[0].score > 0);
        for h in &hits {
            // Every highlighted range must address real bytes of the name.
            for pair in h.match_ranges.chunks(2) {
                assert!(pair[0] < pair[1] && pair[1] as usize <= h.entry.name.len());
            }
        }
    }

    #[test]
    fn unicode_names_report_byte_offsets() {
        let mut s = EntryStore::with_mount("v".into(), "/v", '/');
        let root = s.push(1, NO_RECORD, "v", Meta::dir());
        s.set_root(root);
        s.push(2, root, "Ärger_Übersicht.txt", Meta::file(1));
        let sink = CollectSink::new();
        execute(&[&s], &q("übersicht"), &sink, false).unwrap();
        let hits = sink.into_inner();
        assert_eq!(hits.len(), 1);
        let (a, b) = (
            hits[0].match_ranges[0] as usize,
            hits[0].match_ranges[1] as usize,
        );
        assert_eq!(&hits[0].entry.name[a..b], "Übersicht");
    }

    #[test]
    fn regex_and_glob_run_end_to_end() {
        let fx = fixture();
        let mut query = q(r"report\d+");
        query.mode = MatchMode::Regex;
        let (hits, _) = run(&fx, query);
        assert_eq!(names(&hits), vec!["Report2.pdf", "report10.pdf"]);

        let mut query = q("*.rs");
        query.mode = MatchMode::Glob;
        let (hits, _) = run(&fx, query);
        assert_eq!(names(&hits), vec!["lib.rs", "main.rs"]);
        assert!(hits[0].match_ranges.is_empty());
    }

    #[test]
    fn many_chunks_and_stores_merge_correctly() {
        // Enough records to span several parallel chunks, in two stores.
        let mut stores = Vec::new();
        for v in 0..2u64 {
            let mut s = EntryStore::with_mount(format!("v{v}"), format!("/v{v}"), '/');
            let root = s.push(0, NO_RECORD, "root", Meta::dir());
            s.set_root(root);
            for i in 1..(CHUNK as u64 * 3) {
                s.push(i, root, &format!("f{i}.dat"), Meta::file(i));
            }
            stores.push(s);
        }
        let refs: Vec<&EntryStore> = stores.iter().collect();
        let mut query = q("f");
        query.limit = 5;
        query.sort = Sort {
            key: SortKey::Size,
            desc: true,
            dirs_first: true,
        };
        let sink = CollectSink::new();
        let sum = execute(&refs, &query, &sink, true).unwrap();
        let hits = sink.into_inner();
        assert_eq!(sum.total_matched, 2 * (CHUNK as u64 * 3 - 1));
        assert!(sum.degraded);
        let sizes: Vec<u64> = hits.iter().map(|h| h.entry.size).collect();
        let max = CHUNK as u64 * 3 - 1;
        assert_eq!(sizes, vec![max, max, max - 1, max - 1, max - 2]);
    }

    #[test]
    fn arena_fast_path_agrees_with_per_record_matching() {
        // Names chosen so that concatenation creates false hits ("ab"+"cd" contains "bc"),
        // across enough records to span several chunks, some made non-contiguous by renames.
        let pieces = ["ab", "cd", "abc", "bcd", "x", "", "aBc", "dab", "c", "bcbc"];
        let mut s = EntryStore::with_mount("v".into(), "/v", '/');
        let root = s.push(0, NO_RECORD, "v", Meta::dir());
        s.set_root(root);
        let n = CHUNK as u64 * 3 + 77;
        for i in 1..n {
            let a = pieces[(i * 7 % 10) as usize];
            let b = pieces[(i * 3 % 10) as usize];
            s.push(i, root, &format!("{a}{b}"), Meta::file(i));
        }
        for i in (CHUNK as u64 + 1..CHUNK as u64 * 2).step_by(997) {
            s.rename(i, 0, &format!("renamed-bc-{i}"));
        }
        for i in (5..n).step_by(1013) {
            s.remove(i);
        }
        assert!(s.chunk_is_contiguous(0) && !s.chunk_is_contiguous(1));

        for needle in ["bc", "cd", "abc", "dab", "bcbc", "zz", "BC"] {
            let mut query = q(needle);
            query.limit = u32::MAX;
            let sink = CollectSink::new();
            let sum = execute(&[&s], &query, &sink, false).unwrap();
            let mut got: Vec<u64> = sink.into_inner().iter().map(|h| h.entry.id).collect();
            got.sort_unstable();

            let case_sensitive = query.is_case_sensitive();
            let mut want: Vec<u64> = (0..s.len() as RecordIx)
                .filter(|&ix| {
                    let r = s.record(ix);
                    let name = s.name(ix);
                    let hay = if case_sensitive {
                        name.to_owned()
                    } else {
                        name.to_lowercase()
                    };
                    let nd = if case_sensitive {
                        needle.to_owned()
                    } else {
                        needle.to_lowercase()
                    };
                    !r.is_tombstone() && Some(ix) != s.root() && hay.contains(&nd)
                })
                .map(|ix| s.id(ix))
                .collect();
            want.sort_unstable();
            assert_eq!(got, want, "needle {needle:?}");
            assert_eq!(sum.total_matched, want.len() as u64);
        }
    }

    #[test]
    fn a_sink_that_goes_away_cancels_the_query() {
        struct Gone;
        impl BatchSink<SearchHit> for Gone {
            fn push(&self, _batch: Vec<SearchHit>) -> bool {
                false
            }
        }
        let fx = fixture();
        assert_eq!(
            execute(&[&fx.store], &q(""), &Gone, false),
            Err(FxError::Cancelled)
        );
    }

    #[test]
    fn results_stream_in_batches() {
        use std::sync::Mutex;
        struct Sizes(Mutex<Vec<usize>>, Mutex<u64>);
        impl BatchSink<SearchHit> for Sizes {
            fn push(&self, b: Vec<SearchHit>) -> bool {
                self.0.lock().unwrap().push(b.len());
                true
            }
            fn hint_total(&self, t: u64) {
                *self.1.lock().unwrap() = t;
            }
        }
        let mut s = EntryStore::with_mount("v".into(), "/v", '/');
        let root = s.push(0, NO_RECORD, "v", Meta::dir());
        s.set_root(root);
        for i in 1..=1200u64 {
            s.push(i, root, &format!("n{i}"), Meta::file(0));
        }
        let sink = Sizes(Mutex::new(Vec::new()), Mutex::new(0));
        let mut query = q("");
        query.limit = 1100;
        execute(&[&s], &query, &sink, false).unwrap();
        assert_eq!(*sink.0.lock().unwrap(), vec![500, 500, 100]);
        assert_eq!(
            *sink.1.lock().unwrap(),
            1200,
            "hint carries the full match count"
        );
    }
}
