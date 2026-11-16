//! Directory enumeration.
//!
//! Portable, so it is fully testable on macOS.
//!
//! Requirements that shape this:
//!
//! - **Stream.** Push batches of [`fx_core::Query::BATCH_SIZE`] through the sink as they
//!   are read, so a 200k-entry directory shows its first screenful immediately.
//! - **Sort in Rust.** The UI renders a virtualised window over an already-ordered set.
//!   Ordering must be applied before streaming, which means reading the whole directory
//!   first. The expensive part of that is one `stat` per entry, so large directories
//!   fan the stats out across the rayon pool.
//! - **Never fail the whole listing for one bad entry.** Count permission errors into
//!   [`fx_core::ListSummary::inaccessible`] and keep going.
//! - **Long paths on Windows** need no handling here: `std::fs` already rewrites long
//!   absolute paths to the `\\?\` form internally.
//!
//! ## Directory sizes
//!
//! With [`ListOptions::compute_dir_sizes`], the listing is streamed first exactly as
//! without it, and the subtree sizes are computed afterwards on the rayon pool. Each
//! directory row is then **re-sent** with its size filled in; the re-sent row has the same
//! [`FileEntry::id`] and name, and consumers treat it as an update. The one exception is a
//! listing sorted by size: the order depends on the sizes, so they are computed first.

use fx_core::{
    Attributes, BatchSink, EntryId, EntryKind, FileEntry, FxError, ListOptions, ListSummary, Query,
    Result, SortKey,
};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::time::Instant;

/// Above this many entries, stats and sorting run in parallel. Below it, thread hand-off
/// costs more than it saves.
const PARALLEL_THRESHOLD: usize = 2_048;

/// Lists a directory, streaming results through `sink`.
pub fn list_dir(
    dir: &str,
    options: &ListOptions,
    sink: &dyn BatchSink<FileEntry>,
) -> Result<ListSummary> {
    let started = Instant::now();
    let read = std::fs::read_dir(dir)?;
    let parent_id = std::fs::metadata(dir)
        .map(|m| entry_id(&m, ""))
        .unwrap_or(0);

    // Names first: `read_dir` itself is cheap, and collecting before the per-entry stat
    // lets a large directory fan the stats out.
    let mut inaccessible = 0u64;
    let items: Vec<std::fs::DirEntry> = read
        .filter_map(|item| match item {
            Ok(i) => Some(i),
            Err(_) => {
                inaccessible += 1;
                None
            }
        })
        .collect();

    let build = |item: &std::fs::DirEntry| entry_from_dir_entry(item, parent_id).ok();
    let built: Vec<Option<FileEntry>> = if items.len() >= PARALLEL_THRESHOLD {
        items.par_iter().map(build).collect()
    } else {
        items.iter().map(build).collect()
    };
    let mut entries = Vec::with_capacity(built.len());
    for e in built {
        match e {
            Some(e) => entries.push(e),
            None => inaccessible += 1,
        }
    }

    entries.retain(|e| is_visible(&e.attrs, options));

    let dir_path = Path::new(dir);
    let sizes_first = options.compute_dir_sizes && options.sort.key == SortKey::Size;
    if sizes_first {
        let cancelled = AtomicBool::new(false);
        fill_dir_sizes(dir_path, &mut entries, &cancelled);
    }

    sort_entries(&mut entries, options);

    let total = entries.len() as u64;
    sink.hint_total(total);

    for batch in entries.chunks(Query::BATCH_SIZE) {
        if !sink.push(batch.to_vec()) {
            // Consumer navigated away; stop rather than finish work nobody wants.
            return Err(FxError::Cancelled);
        }
    }

    if options.compute_dir_sizes && !sizes_first {
        stream_dir_sizes(dir_path, &entries, sink)?;
    }

    Ok(ListSummary {
        total,
        elapsed_micros: started.elapsed().as_micros() as u64,
        inaccessible,
    })
}

/// Hidden/system filtering, matching Explorer's two separate toggles: "hidden items", and
/// "protected operating system files", which are the ones that are *both* hidden and
/// system (`desktop.ini`, `$RECYCLE.BIN`).
fn is_visible(attrs: &Attributes, options: &ListOptions) -> bool {
    if attrs.hidden && attrs.system {
        return options.show_hidden && options.show_system;
    }
    !attrs.hidden || options.show_hidden
}

/// Computes each directory row's size, then re-sends those rows as update batches.
fn stream_dir_sizes(
    dir: &Path,
    entries: &[FileEntry],
    sink: &dyn BatchSink<FileEntry>,
) -> Result<()> {
    let mut dirs: Vec<FileEntry> = entries
        .iter()
        .filter(|e| e.kind == EntryKind::Dir)
        .cloned()
        .collect();
    if dirs.is_empty() {
        return Ok(());
    }
    // A sink cannot be polled for liveness without pushing, so a refused update batch is
    // what stops the remaining walks. The flag stays false: the walks share it only so
    // `subtree_size` keeps one signature for both callers.
    let cancelled = AtomicBool::new(false);
    // Small chunks so the first sizes appear quickly rather than after the slowest
    // subtree in a 500-row batch.
    for chunk in dirs.chunks_mut(32) {
        fill_dir_sizes(dir, chunk, &cancelled);
        if !sink.push(chunk.to_vec()) {
            return Err(FxError::Cancelled);
        }
    }
    Ok(())
}

fn fill_dir_sizes(dir: &Path, entries: &mut [FileEntry], cancelled: &AtomicBool) {
    entries
        .par_iter_mut()
        .filter(|e| e.kind == EntryKind::Dir)
        .for_each(|e| e.size = subtree_size(&dir.join(&e.name), cancelled));
}

/// Total size of the files under `root`. Does not follow symlinks or junctions: following
/// them double-counts, and a junction loop would never terminate.
pub fn subtree_size(root: &Path, cancelled: &AtomicBool) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if cancelled.load(AtomicOrdering::Relaxed) {
            break;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in read.flatten() {
            let Ok(ft) = item.file_type() else { continue };
            if ft.is_dir() {
                stack.push(item.path());
            } else if ft.is_file() {
                total += item.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// Orders entries per [`ListOptions`], grouping directories first when requested.
///
/// Every key falls back to natural name order, so equal sizes or dates still come out in
/// a stable, predictable order instead of whatever the filesystem returned.
pub fn sort_entries(entries: &mut [FileEntry], options: &ListOptions) {
    let sort = options.sort;
    let cmp = |a: &FileEntry, b: &FileEntry| {
        if sort.dirs_first {
            let ord = b.kind.is_container().cmp(&a.kind.is_container());
            if ord != Ordering::Equal {
                return ord;
            }
        }
        let by_name = || natural_cmp(&a.name, &b.name);
        let ord = match sort.key {
            SortKey::Name | SortKey::Path | SortKey::Relevance => by_name(),
            SortKey::Size => a.size.cmp(&b.size).then_with(by_name),
            SortKey::Modified => a.modified.cmp(&b.modified).then_with(by_name),
            SortKey::Created => a.created.cmp(&b.created).then_with(by_name),
            SortKey::Extension => a.ext.cmp(&b.ext).then_with(by_name),
            SortKey::Kind => b
                .kind
                .is_container()
                .cmp(&a.kind.is_container())
                .then_with(|| a.ext.cmp(&b.ext))
                .then_with(by_name),
        };
        if sort.desc {
            ord.reverse()
        } else {
            ord
        }
    };
    if entries.len() >= PARALLEL_THRESHOLD {
        entries.par_sort_by(cmp);
    } else {
        entries.sort_by(cmp);
    }
}

/// Natural, case-insensitive ordering: `file2` before `file10`, `Apple` beside `apple`.
///
/// This is what users expect from a file manager, and what Explorer does via
/// `StrCmpLogicalW`. Digit runs compare by numeric value of any length, so a 30-digit
/// run cannot overflow. Ties (`a` vs `A`, `01` vs `1`) are broken deterministically so the
/// order is total and a re-listing never shuffles rows. Allocation-free, because it runs
/// `n log n` times on a 500k-entry sort.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    // First difference in leading zeros, consulted only if everything else ties.
    let mut zeros_tiebreak = Ordering::Equal;

    loop {
        match (ab.get(i), bb.get(j)) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (a_end, b_end) = (digit_run_end(ab, i), digit_run_end(bb, j));
                let (a_run, b_run) = (&ab[i..a_end], &bb[j..b_end]);
                let (a_sig, b_sig) = (trim_zeros(a_run), trim_zeros(b_run));
                // More significant digits means a bigger number; equal length compares
                // lexicographically, which for digits is numeric.
                let ord = a_sig.len().cmp(&b_sig.len()).then_with(|| a_sig.cmp(b_sig));
                if ord != Ordering::Equal {
                    return ord;
                }
                if zeros_tiebreak == Ordering::Equal {
                    // Fewer leading zeros first: `1` before `01`.
                    zeros_tiebreak = a_run.len().cmp(&b_run.len());
                }
                i = a_end;
                j = b_end;
            }
            (Some(_), Some(_)) => {
                let (ca, la) = char_at(a, i);
                let (cb, lb) = char_at(b, j);
                let ord = fold(ca).cmp(&fold(cb));
                if ord != Ordering::Equal {
                    return ord;
                }
                i += la;
                j += lb;
            }
        }
    }
    zeros_tiebreak.then_with(|| a.cmp(b))
}

fn digit_run_end(s: &[u8], mut i: usize) -> usize {
    while s.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    i
}

fn trim_zeros(run: &[u8]) -> &[u8] {
    let first = run.iter().position(|&d| d != b'0').unwrap_or(run.len());
    &run[first..]
}

/// The char starting at byte `i` and its UTF-8 length. `i` is always a char boundary:
/// it only ever advances by whole chars or by ASCII digit runs.
fn char_at(s: &str, i: usize) -> (char, usize) {
    s.get(i..)
        .and_then(|rest| rest.chars().next())
        .map(|c| (c, c.len_utf8()))
        .unwrap_or(('\0', 1))
}

/// Simple case and accent folding. Multi-char lowercase expansions (rare in file names)
/// fold to their first char, which keeps this allocation-free.
///
/// Latin-1 accented letters fold to their base letter so `été` sorts beside `ete` rather
/// than after `zz` as raw code points would — closer to the locale-aware collation users
/// see in Explorer. Accents still order deterministically through the final tie-break.
fn fold(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    // Base letters for U+00C0..=U+00FF; non-letters (×, ÷, þ) map to themselves.
    const LATIN1: [char; 64] = [
        'a', 'a', 'a', 'a', 'a', 'a', 'a', 'c', 'e', 'e', 'e', 'e', 'i', 'i', 'i', 'i', //
        'd', 'n', 'o', 'o', 'o', 'o', 'o', '×', 'o', 'u', 'u', 'u', 'u', 'y', 'þ', 's', //
        'a', 'a', 'a', 'a', 'a', 'a', 'a', 'c', 'e', 'e', 'e', 'e', 'i', 'i', 'i', 'i', //
        'd', 'n', 'o', 'o', 'o', 'o', 'o', '÷', 'o', 'u', 'u', 'u', 'u', 'y', 'þ', 'y', //
    ];
    match c as u32 {
        cp @ 0xC0..=0xFF => LATIN1[(cp - 0xC0) as usize],
        _ => c.to_lowercase().next().unwrap_or(c),
    }
}

/// Metadata for a single path.
pub fn stat(path: &str) -> Result<FileEntry> {
    let meta = std::fs::symlink_metadata(path)?;
    let name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let parent = Path::new(path)
        .parent()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| entry_id(&m, ""))
        .unwrap_or(0);
    let id = entry_id(&meta, &name);
    Ok(build_entry(name, &meta, id, parent))
}

fn entry_from_dir_entry(item: &std::fs::DirEntry, parent: EntryId) -> Result<FileEntry> {
    // On Windows this metadata comes straight from the FindNextFileW record, so it costs
    // no extra syscall; on Unix it is an lstat (never follows symlinks).
    let meta = item.metadata()?;
    let name = item.file_name().to_string_lossy().into_owned();
    let id = entry_id(&meta, &name);
    Ok(build_entry(name, &meta, id, parent))
}

/// Stable per-volume id for a listing row.
///
/// Unix: the inode, which is exactly the "stable per-volume identifier" the contract
/// wants. Windows: `std` does not expose the file index without an extra handle open per
/// file, which would double listing cost, so this is a hash of the name — unique within
/// the directory, which is all a listing row needs to be keyed and updated by.
#[cfg(unix)]
fn entry_id(meta: &std::fs::Metadata, _name: &str) -> EntryId {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
fn entry_id(_meta: &std::fs::Metadata, name: &str) -> EntryId {
    // FNV-1a over the case-folded name: NTFS names are case-insensitive.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for c in name.chars().map(fold) {
        let mut buf = [0u8; 4];
        for b in c.encode_utf8(&mut buf).as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn build_entry(name: String, meta: &std::fs::Metadata, id: EntryId, parent: EntryId) -> FileEntry {
    let (_, ext) = FileEntry::split_ext(&name);
    let ft = meta.file_type();
    let kind = if ft.is_symlink() {
        EntryKind::Symlink
    } else if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_file() {
        EntryKind::File
    } else {
        EntryKind::Unknown
    };

    let attrs = attributes(&name, meta);
    FileEntry {
        id,
        parent,
        attrs,
        ext: if kind == EntryKind::Dir { None } else { ext },
        size: if kind == EntryKind::Dir {
            0
        } else {
            meta.len()
        },
        modified: system_time_millis(meta.modified().ok()),
        created: system_time_millis(meta.created().ok()),
        kind,
        name,
    }
}

/// Windows attribute bits, from the same record `GetFileAttributesW` reads.
#[cfg(windows)]
fn attributes(_name: &str, meta: &std::fs::Metadata) -> Attributes {
    use std::os::windows::fs::MetadataExt;
    let a = meta.file_attributes();
    Attributes {
        readonly: a & 0x1 != 0,
        hidden: a & 0x2 != 0,
        system: a & 0x4 != 0,
        archive: a & 0x20 != 0,
        sparse: a & 0x200 != 0,
        reparse: a & 0x400 != 0,
        compressed: a & 0x800 != 0,
        encrypted: a & 0x4000 != 0,
    }
}

/// Unix: dotfiles are hidden by convention; macOS additionally has the `UF_HIDDEN` flag
/// (what `chflags hidden` sets, and how Finder hides `~/Library`).
#[cfg(not(windows))]
fn attributes(name: &str, meta: &std::fs::Metadata) -> Attributes {
    #[cfg(target_os = "macos")]
    let flag_hidden = {
        use std::os::macos::fs::MetadataExt;
        const UF_HIDDEN: u32 = 0x8000;
        meta.st_flags() & UF_HIDDEN != 0
    };
    #[cfg(not(target_os = "macos"))]
    let flag_hidden = false;

    Attributes {
        hidden: name.starts_with('.') || flag_hidden,
        readonly: meta.permissions().readonly(),
        reparse: meta.file_type().is_symlink(),
        ..Default::default()
    }
}

/// Converts to epoch millis, yielding `0` for unknown — the contract's "unknown" value.
pub(crate) fn system_time_millis(t: Option<std::time::SystemTime>) -> i64 {
    t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fx_core::{CollectSink, Sort};
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("zdir")).unwrap();
        fs::write(d.path().join("a.txt"), b"hello").unwrap();
        fs::write(d.path().join("b.rs"), b"fn main() {}").unwrap();
        let hidden = d.path().join(".hidden");
        fs::write(&hidden, b"x").unwrap();
        mark_hidden(&hidden);
        d
    }

    /// Unix hides dotfiles by name, so `.hidden` already is. Windows hides by attribute.
    #[cfg(unix)]
    fn mark_hidden(_path: &Path) {}

    #[cfg(windows)]
    fn mark_hidden(path: &Path) {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN};
        // SAFETY: the HSTRING outlives the call and the attribute constant is a plain flag.
        #[allow(unsafe_code)]
        unsafe { SetFileAttributesW(&HSTRING::from(path.as_os_str()), FILE_ATTRIBUTE_HIDDEN) }
            .unwrap();
    }

    fn names(entries: &[FileEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    fn list(dir: &Path, opts: &ListOptions) -> (ListSummary, Vec<FileEntry>) {
        let sink = CollectSink::new();
        let summary = list_dir(dir.to_str().unwrap(), opts, &sink).unwrap();
        (summary, sink.into_inner())
    }

    fn by_name(desc: bool, dirs_first: bool) -> ListOptions {
        ListOptions {
            sort: Sort {
                key: SortKey::Name,
                desc,
                dirs_first,
            },
            ..Default::default()
        }
    }

    #[test]
    fn lists_visible_entries_and_hides_dotfiles_by_default() {
        let d = fixture();
        let (summary, entries) = list(d.path(), &ListOptions::default());
        assert_eq!(summary.total, 3, "dotfile should be excluded");
        assert!(names(&entries).contains(&"a.txt"));
        assert!(!names(&entries).contains(&".hidden"));
    }

    #[test]
    fn show_hidden_includes_dotfiles() {
        let d = fixture();
        let opts = ListOptions {
            show_hidden: true,
            ..Default::default()
        };
        assert_eq!(list(d.path(), &opts).0.total, 4);
    }

    #[test]
    fn directories_sort_before_files() {
        let d = fixture();
        let (_, entries) = list(d.path(), &ListOptions::default());
        // "zdir" sorts last alphabetically but must still come first.
        assert_eq!(entries[0].name, "zdir");
        assert_eq!(entries[0].kind, EntryKind::Dir);
    }

    #[test]
    fn dirs_first_can_be_disabled() {
        let d = fixture();
        let (_, entries) = list(d.path(), &by_name(false, false));
        assert_eq!(entries[0].name, "a.txt");
    }

    #[test]
    fn descending_keeps_directories_grouped_first() {
        let d = fixture();
        let (_, entries) = list(d.path(), &by_name(true, true));
        assert_eq!(names(&entries), ["zdir", "b.rs", "a.txt"]);
    }

    #[test]
    fn extension_is_precomputed_lowercase_and_absent_for_dirs() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("Shout.TXT"), b"x").unwrap();
        fs::create_dir(d.path().join("plain.d")).unwrap();

        for e in list(d.path(), &ListOptions::default()).1 {
            match e.kind {
                EntryKind::Dir => assert_eq!(e.ext, None),
                _ => assert_eq!(e.ext.as_deref(), Some("txt")),
            }
        }
    }

    #[test]
    fn stat_reports_size_and_name() {
        let d = fixture();
        let e = stat(d.path().join("a.txt").to_str().unwrap()).unwrap();
        assert_eq!(e.name, "a.txt");
        assert_eq!(e.size, 5);
        assert_eq!(e.kind, EntryKind::File);
    }

    #[test]
    fn stat_of_missing_path_is_not_found() {
        assert!(matches!(
            stat("/definitely/not/here.txt"),
            Err(FxError::NotFound(_))
        ));
    }

    #[test]
    fn missing_directory_is_a_not_found_error() {
        let sink = CollectSink::new();
        let err = list_dir("/definitely/not/here", &ListOptions::default(), &sink).unwrap_err();
        assert!(matches!(err, FxError::NotFound(_)), "got {err:?}");
    }

    #[test]
    fn cancelling_sink_stops_the_listing() {
        struct Refuse;
        impl BatchSink<FileEntry> for Refuse {
            fn push(&self, _b: Vec<FileEntry>) -> bool {
                false
            }
        }
        let d = fixture();
        let err =
            list_dir(d.path().to_str().unwrap(), &ListOptions::default(), &Refuse).unwrap_err();
        assert!(matches!(err, FxError::Cancelled));
    }

    #[test]
    fn rows_have_distinct_ids_under_a_common_parent() {
        let d = fixture();
        let (_, entries) = list(d.path(), &ListOptions::default());
        let mut ids: Vec<_> = entries.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            entries.len(),
            "ids key UI rows, so must be unique"
        );
        assert!(entries.windows(2).all(|w| w[0].parent == w[1].parent));
    }

    #[test]
    fn large_directories_stream_in_batches_with_a_total_hint() {
        use std::sync::Mutex;
        #[derive(Default)]
        struct Recording {
            hint: Mutex<Option<u64>>,
            batches: Mutex<Vec<usize>>,
        }
        impl BatchSink<FileEntry> for Recording {
            fn push(&self, b: Vec<FileEntry>) -> bool {
                self.batches.lock().unwrap().push(b.len());
                true
            }
            fn hint_total(&self, total: u64) {
                *self.hint.lock().unwrap() = Some(total);
            }
        }

        let d = tempfile::tempdir().unwrap();
        let n = Query::BATCH_SIZE * 2 + 7 + PARALLEL_THRESHOLD;
        for i in 0..n {
            fs::write(d.path().join(format!("f{i}")), b"").unwrap();
        }
        let sink = Recording::default();
        let summary = list_dir(d.path().to_str().unwrap(), &ListOptions::default(), &sink).unwrap();
        assert_eq!(summary.total, n as u64);
        assert_eq!(*sink.hint.lock().unwrap(), Some(n as u64));
        let batches = sink.batches.lock().unwrap();
        assert!(batches.iter().all(|&b| b <= Query::BATCH_SIZE));
        assert_eq!(batches.iter().sum::<usize>(), n);
    }

    #[test]
    fn parallel_path_sorts_naturally_too() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..PARALLEL_THRESHOLD + 10 {
            fs::write(d.path().join(format!("file{i}")), b"").unwrap();
        }
        let (_, entries) = list(d.path(), &ListOptions::default());
        assert_eq!(entries[0].name, "file0");
        assert_eq!(entries[1].name, "file1");
        assert_eq!(entries[2].name, "file2");
        assert_eq!(entries[10].name, "file10");
    }

    // ---- natural sort ------------------------------------------------------------------

    fn sorted(mut v: Vec<&str>) -> Vec<&str> {
        v.sort_by(|a, b| natural_cmp(a, b));
        v
    }

    #[test]
    fn natural_sort_orders_numbers_by_value() {
        assert_eq!(
            sorted(vec!["file10", "file2", "file1", "file20", "file3"]),
            ["file1", "file2", "file3", "file10", "file20"]
        );
        assert_eq!(
            sorted(vec!["v1.10.0", "v1.2.0", "v1.9.3"]),
            ["v1.2.0", "v1.9.3", "v1.10.0"]
        );
    }

    #[test]
    fn natural_sort_is_case_insensitive_with_a_deterministic_tiebreak() {
        assert_eq!(natural_cmp("apple", "Banana"), Ordering::Less);
        assert_eq!(natural_cmp("Zebra", "apple"), Ordering::Greater);
        // Equal ignoring case must still be a total order, never Equal for distinct names.
        assert_ne!(natural_cmp("Readme", "readme"), Ordering::Equal);
        assert_eq!(
            natural_cmp("Readme", "readme"),
            natural_cmp("Readme", "readme")
        );
        assert_eq!(natural_cmp("same", "same"), Ordering::Equal);
    }

    #[test]
    fn natural_sort_handles_leading_zeros_and_huge_numbers() {
        // Same value: fewer zeros first, but they group together ahead of 2.
        assert_eq!(
            sorted(vec!["img2", "img001", "img1"]),
            ["img1", "img001", "img2"]
        );
        // Longer than u64::MAX — must not overflow or fall back to lexicographic.
        let big = "n99999999999999999999999999999";
        let bigger = "n100000000000000000000000000000";
        assert_eq!(natural_cmp(big, bigger), Ordering::Less);
    }

    #[test]
    fn natural_sort_prefixes_and_unicode() {
        assert_eq!(natural_cmp("file", "file1"), Ordering::Less);
        assert_eq!(natural_cmp("a", ""), Ordering::Greater);
        assert_eq!(natural_cmp("Äpfel2", "äpfel10"), Ordering::Less);
        assert_eq!(sorted(vec!["été", "Ete", "zz"]), ["Ete", "été", "zz"]);
    }

    #[test]
    fn sort_by_size_ties_break_by_name() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("b10"), b"xx").unwrap();
        fs::write(d.path().join("b9"), b"xx").unwrap();
        fs::write(d.path().join("a"), b"xxxxx").unwrap();
        let opts = ListOptions {
            sort: Sort {
                key: SortKey::Size,
                desc: false,
                dirs_first: true,
            },
            ..Default::default()
        };
        assert_eq!(names(&list(d.path(), &opts).1), ["b9", "b10", "a"]);
    }

    // ---- directory sizes ---------------------------------------------------------------

    fn sized_fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("big/nested")).unwrap();
        fs::write(d.path().join("big/one"), vec![0u8; 100]).unwrap();
        fs::write(d.path().join("big/nested/two"), vec![0u8; 50]).unwrap();
        fs::create_dir(d.path().join("small")).unwrap();
        fs::write(d.path().join("small/x"), vec![0u8; 10]).unwrap();
        fs::write(d.path().join("file"), vec![0u8; 1]).unwrap();
        d
    }

    #[test]
    fn dir_sizes_arrive_as_updates_after_the_listing() {
        let d = sized_fixture();
        let opts = ListOptions {
            compute_dir_sizes: true,
            ..Default::default()
        };
        let (summary, rows) = list(d.path(), &opts);
        // Three real entries, then two directory updates.
        assert_eq!(summary.total, 3);
        assert_eq!(rows.len(), 5);
        assert!(rows[..3]
            .iter()
            .filter(|e| e.kind == EntryKind::Dir)
            .all(|e| e.size == 0));
        let updates = &rows[3..];
        let big = updates.iter().find(|e| e.name == "big").unwrap();
        let first_big = rows.iter().find(|e| e.name == "big").unwrap();
        assert_eq!(big.size, 150);
        assert_eq!(
            big.id, first_big.id,
            "update must be keyed like the original"
        );
        assert_eq!(updates.iter().find(|e| e.name == "small").unwrap().size, 10);
    }

    #[test]
    fn sorting_by_size_computes_dir_sizes_first() {
        let d = sized_fixture();
        let opts = ListOptions {
            compute_dir_sizes: true,
            sort: Sort {
                key: SortKey::Size,
                desc: true,
                dirs_first: true,
            },
            ..Default::default()
        };
        let (_, rows) = list(d.path(), &opts);
        assert_eq!(
            names(&rows),
            ["big", "small", "file"],
            "no trailing updates"
        );
        assert_eq!(rows[0].size, 150);
    }

    #[cfg(unix)]
    #[test]
    fn subtree_size_does_not_follow_symlinks() {
        let d = sized_fixture();
        std::os::unix::fs::symlink(d.path().join("big"), d.path().join("small/loop")).unwrap();
        let cancelled = AtomicBool::new(false);
        // A followed link would add big's 150 bytes.
        assert_eq!(subtree_size(&d.path().join("small"), &cancelled), 10);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_reported_as_symlinks() {
        let d = fixture();
        std::os::unix::fs::symlink(d.path().join("zdir"), d.path().join("link")).unwrap();
        let (_, entries) = list(d.path(), &ListOptions::default());
        let link = entries.iter().find(|e| e.name == "link").unwrap();
        assert_eq!(link.kind, EntryKind::Symlink);
        assert!(link.attrs.reparse);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_entries_never_fail_the_listing() {
        use std::os::unix::fs::PermissionsExt;
        let d = fixture();
        let locked = d.path().join("zdir");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // The parent is still listable; the locked dir is just a row.
        let (summary, _) = list(d.path(), &ListOptions::default());
        assert_eq!(summary.total, 3);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn visibility_rules_match_explorer() {
        let hidden = Attributes {
            hidden: true,
            ..Default::default()
        };
        let protected = Attributes {
            hidden: true,
            system: true,
            ..Default::default()
        };
        let system_only = Attributes {
            system: true,
            ..Default::default()
        };
        let show = |h, s| ListOptions {
            show_hidden: h,
            show_system: s,
            ..Default::default()
        };
        assert!(!is_visible(&hidden, &show(false, false)));
        assert!(is_visible(&hidden, &show(true, false)));
        // Protected OS files need both toggles.
        assert!(!is_visible(&protected, &show(true, false)));
        assert!(is_visible(&protected, &show(true, true)));
        // System-but-not-hidden files are visible in Explorer by default.
        assert!(is_visible(&system_only, &show(false, false)));
    }
}
