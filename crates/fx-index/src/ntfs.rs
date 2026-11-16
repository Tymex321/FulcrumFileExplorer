//! The portable half of the MFT and USN backends.
//!
//! Raw volume access only exists on Windows, but the *decisions* made about what the MFT
//! and the change journal report — which name of a hard-linked file to index, which records
//! are metafiles, how a burst of journal records collapses into index updates — are plain
//! logic. They live here, free of `ntfs-reader` types, so the macOS test suite exercises
//! them. `crate::mft` and `crate::usn` (Windows-only) are thin adapters that translate `ntfs-reader`
//! values into [`RawRecord`] / [`JournalRecord`] and call in.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use fx_core::{ChangeEvent, EntryId, EntryKind, Millis, VolumeId};

use crate::store::{flags, EntryStore, Meta, ID_NUMBER_MASK};
use crate::walk::attributes_from_win32;

/// The volume root directory's MFT record number.
pub const ROOT_RECORD: u64 = 5;

/// Records below this are NTFS metafiles (`$MFT`, `$LogFile`, `$Extend`, …), except the
/// root. Never user content, never search results.
pub const FIRST_NORMAL_RECORD: u64 = 24;

/// `FILE_ATTRIBUTE_DIRECTORY`.
pub const ATTR_DIRECTORY: u32 = 0x10;

/// The record number part of a file reference.
#[inline]
pub fn record_number(id: u64) -> u64 {
    id & ID_NUMBER_MASK
}

/// Whether a reference names a metafile (or something only a metafile can contain).
#[inline]
pub fn is_metafile(id: u64) -> bool {
    let n = record_number(id);
    n < FIRST_NORMAL_RECORD && n != ROOT_RECORD
}

/// `$FILE_NAME` namespace, as stored on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    Posix,
    Win32,
    Dos,
    Win32AndDos,
}

/// One `$FILE_NAME` attribute of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameCandidate {
    pub namespace: Namespace,
    pub parent: u64,
    pub name: String,
}

/// Chooses the one name a file is indexed under.
///
/// A file can carry several `$FILE_NAME` attributes: a DOS 8.3 alias alongside its long
/// name, and one per hard link. Indexing all of them would list most files twice
/// (`PROGRA~1` next to `Program Files`). The Win32 name wins; a POSIX name (case-sensitive
/// names created by WSL, and some hard links) is the fallback; a DOS-only name is used only
/// if it is all there is, so the file is still findable.
pub fn pick_name(names: impl IntoIterator<Item = NameCandidate>) -> Option<NameCandidate> {
    let mut posix = None;
    let mut dos = None;
    for n in names {
        match n.namespace {
            Namespace::Win32 | Namespace::Win32AndDos => return Some(n),
            Namespace::Posix => {
                posix.get_or_insert(n);
            }
            Namespace::Dos => {
                dos.get_or_insert(n);
            }
        }
    }
    posix.or(dos)
}

/// One MFT file record, reduced to what the index keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRecord {
    /// Full file reference: sequence number in the high 16 bits.
    pub id: u64,
    /// Full parent reference from the chosen `$FILE_NAME`. Carries the sequence number the
    /// parent had when the name was written, which is what makes a stale link detectable.
    pub parent: u64,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Raw `FILE_ATTRIBUTE_*` bits from `$STANDARD_INFORMATION`.
    pub attrs: u32,
    pub modified: Millis,
    pub created: Millis,
}

impl RawRecord {
    fn meta(&self) -> Meta {
        let kind = if self.is_dir {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let size = if self.is_dir { 0 } else { self.size };
        Meta::new(
            kind,
            &attributes_from_win32(self.attrs),
            size,
            self.modified,
            self.created,
        )
    }
}

/// Builds a volume's store from MFT records arriving in record order.
///
/// Parents are resolved only in [`VolumeBuilder::finish`], because in record order a
/// child frequently precedes its directory.
#[derive(Debug)]
pub struct VolumeBuilder {
    store: EntryStore,
    parents: Vec<u64>,
    root_id: Option<u64>,
    skipped: u64,
}

impl VolumeBuilder {
    /// `mount` is the path the root rebuilds to, e.g. `C:\`.
    pub fn new(volume: VolumeId, mount: impl Into<String>) -> Self {
        Self {
            store: EntryStore::with_mount(volume, mount, '\\'),
            parents: Vec::new(),
            root_id: None,
            skipped: 0,
        }
    }

    /// Adds one record. Metafiles are dropped here; anything beneath one (the contents of
    /// `$Extend`) is dropped in [`VolumeBuilder::finish`], once parents are known.
    pub fn push(&mut self, r: RawRecord) {
        let number = record_number(r.id);
        if is_metafile(r.id) {
            self.skipped += 1;
            return;
        }
        if number == ROOT_RECORD {
            self.root_id = Some(r.id);
        }
        self.store.push_unlinked(r.id, &r.name, r.meta());
        self.parents.push(r.parent);
    }

    /// Records pushed so far, metafiles excluded.
    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Metafile records dropped so far.
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Links every record to its parent — validating the parent's full reference,
    /// sequence number included, so a reused MFT slot never adopts another file's
    /// children — and removes everything that lives under a metafile.
    pub fn finish(mut self) -> EntryStore {
        self.store.link_parents(&self.parents, self.root_id);

        // `$Extend\$RmMetadata\$Txf\...` and friends. Direct children of a metafile are
        // orphans (their parent was never pushed); deeper ones linked fine and only become
        // removable once their parent is tombstoned, so sweep until nothing changes. In a
        // finished builder the only tombstones are these removals, so "parent is a
        // tombstone" means exactly "parent was under a metafile". One pass per level of
        // `$Extend` nesting — a handful — each a cheap scan with no hashing.
        loop {
            let mut changed = false;
            for ix in 0..self.store.len() {
                let r = self.store.records[ix];
                if r.is_tombstone() {
                    continue;
                }
                let under_dead = if r.parent == crate::store::NO_RECORD {
                    r.flags & flags::ORPHAN != 0 && is_metafile(self.parents[ix])
                } else {
                    self.store.records[r.parent as usize].is_tombstone()
                };
                if under_dead {
                    let id = self.store.ids[ix];
                    self.store.remove(id);
                    self.skipped += 1;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        self.store.shrink_to_fit();
        self.store
    }
}

// ---- USN journal application ----

/// USN reason bits used by the coalescer (values from `winioctl.h`).
pub mod reason {
    pub const DATA_OVERWRITE: u32 = 0x0000_0001;
    pub const DATA_EXTEND: u32 = 0x0000_0002;
    pub const DATA_TRUNCATION: u32 = 0x0000_0004;
    pub const FILE_CREATE: u32 = 0x0000_0100;
    pub const FILE_DELETE: u32 = 0x0000_0200;
    pub const RENAME_OLD_NAME: u32 = 0x0000_1000;
    pub const RENAME_NEW_NAME: u32 = 0x0000_2000;
    pub const BASIC_INFO_CHANGE: u32 = 0x0000_8000;
    pub const HARD_LINK_CHANGE: u32 = 0x0001_0000;
    pub const REPARSE_POINT_CHANGE: u32 = 0x0010_0000;
    pub const CLOSE: u32 = 0x8000_0000;

    /// Everything that can change what the index shows. Security, EA, object-id and
    /// stream changes are filtered out by the driver, before they ever reach us.
    pub const INTERESTING: u32 = DATA_OVERWRITE
        | DATA_EXTEND
        | DATA_TRUNCATION
        | FILE_CREATE
        | FILE_DELETE
        | RENAME_OLD_NAME
        | RENAME_NEW_NAME
        | BASIC_INFO_CHANGE
        | HARD_LINK_CHANGE
        | REPARSE_POINT_CHANGE
        | CLOSE;
}

/// One change-journal record, reduced to what the coalescer needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRecord {
    pub usn: i64,
    pub id: u64,
    pub parent: u64,
    pub reason: u32,
    /// `FILE_ATTRIBUTE_*` at the time of the change.
    pub attrs: u32,
    pub name: String,
    pub time: Millis,
}

#[derive(Debug)]
struct Net<'a> {
    last: Option<&'a JournalRecord>,
    deleted: bool,
    created_here: bool,
}

/// Applies a batch of journal records — everything read in one coalescing window — and
/// returns one event per file that visibly changed.
///
/// The burst from one operation collapses per file id: a file created, written ten times
/// and closed becomes one `Created`; a temp file created and deleted inside the window
/// produces nothing at all.
///
/// Renames are paired through the store rather than through journal history: the store
/// still holds the old name until the `RENAME_NEW_NAME` record is applied, so the event
/// reports both names even when the `RENAME_OLD_NAME` half fell into an earlier window.
/// `RENAME_OLD_NAME` records are therefore ignored outright.
///
/// The journal carries names and attributes but not sizes, so `stat` is asked for the
/// current metadata of each surviving file, by id and resolved path. When it cannot answer
/// (the file is already gone again, is unreadable, or the caller is deferring stats until
/// it has released its lock) the journal's attributes and timestamp are used and the last
/// known size kept.
///
/// Applying the same batch twice yields the same store, which is what lets a scan start
/// from a journal position taken *before* the MFT read and replay over it safely.
pub fn apply_journal(
    store: &mut EntryStore,
    records: &[JournalRecord],
    stat: &mut dyn FnMut(EntryId, &str) -> Option<Meta>,
) -> Vec<ChangeEvent> {
    let mut order: Vec<u64> = Vec::new();
    let mut net: HashMap<u64, Net<'_>> = HashMap::new();
    for r in records {
        // Metafile churn ($MFT, $LogFile, $Extend\...) is constant and never user-visible.
        if is_metafile(r.id) || is_metafile(r.parent) {
            continue;
        }
        let n = net.entry(r.id).or_insert_with(|| {
            order.push(r.id);
            Net {
                last: None,
                deleted: false,
                created_here: false,
            }
        });
        if r.reason & reason::FILE_DELETE != 0 {
            n.deleted = true;
        } else if r.reason & reason::RENAME_OLD_NAME != 0 && r.reason & reason::RENAME_NEW_NAME == 0
        {
            // The old half of a rename: the new half carries everything we need.
        } else {
            n.deleted = false;
            n.last = Some(r);
            if r.reason & reason::FILE_CREATE != 0 {
                n.created_here = true;
            }
        }
    }

    let volume = store.volume.clone();
    let mut events = Vec::new();
    for id in order {
        let Some(n) = net.remove(&id) else { continue };
        if n.deleted {
            // Created and deleted within one window: never shown, nothing to report —
            // unless a replay finds it already in the store from the scan.
            if store.remove(id) {
                events.push(ChangeEvent::Deleted {
                    volume: volume.clone(),
                    id,
                });
            }
            continue;
        }
        let Some(r) = n.last else { continue };

        let path = store.find(r.parent).map(|p| {
            let mut s = store.path_of(p);
            if !s.ends_with('\\') {
                s.push('\\');
            }
            s.push_str(&r.name);
            s
        });
        let existing = store.find(id);
        let meta = path
            .as_deref()
            .and_then(|p| stat(id, p))
            .unwrap_or_else(|| fallback_meta(store, existing, r));

        match existing {
            None => {
                store.upsert(id, r.parent, &r.name, meta);
                events.push(ChangeEvent::Created {
                    volume: volume.clone(),
                    id,
                });
            }
            Some(ix) => {
                let rec = *store.record(ix);
                let parent_id = if rec.parent == crate::store::NO_RECORD {
                    0
                } else {
                    store.id(rec.parent)
                };
                let moved = parent_id != r.parent || store.name(ix) != r.name;
                let meta_changed = rec.flags & !(flags::TOMBSTONE | flags::ORPHAN)
                    != meta.kind_and_attrs
                    || rec.size != meta.size
                    || rec.modified != meta.modified
                    || rec.created != meta.created;
                if moved {
                    let old_name = store.rename(id, r.parent, &r.name).unwrap_or_default();
                    store.set_meta(id, meta);
                    events.push(ChangeEvent::Renamed {
                        volume: volume.clone(),
                        id,
                        old_name,
                        new_name: r.name.clone(),
                    });
                } else if meta_changed {
                    store.set_meta(id, meta);
                    events.push(ChangeEvent::Modified {
                        volume: volume.clone(),
                        id,
                    });
                } else if n.created_here {
                    // A replayed create of something the scan already saw: nothing new.
                }
            }
        }
    }
    events
}

fn fallback_meta(store: &EntryStore, existing: Option<u32>, r: &JournalRecord) -> Meta {
    let kind = if r.attrs & ATTR_DIRECTORY != 0 {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    let (size, created) = existing
        .map(|ix| {
            let rec = store.record(ix);
            (rec.size, rec.created)
        })
        .unwrap_or((0, r.time));
    Meta::new(kind, &attributes_from_win32(r.attrs), size, r.time, created)
}

/// Converts a Windows FILETIME (100 ns ticks since 1601-01-01 UTC) to Unix milliseconds.
/// `0` stays `0`, the contract's "unknown".
pub fn filetime_to_millis(ft: i64) -> Millis {
    const EPOCH_DIFF_MS: i64 = 11_644_473_600_000;
    if ft <= 0 {
        return 0;
    }
    ft / 10_000 - EPOCH_DIFF_MS
}

// ---- persistence ----

/// Where to resume the change journal: the journal's id (a recreated journal gets a new
/// one, invalidating every saved position) and the next USN to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    pub journal_id: u64,
    pub next_usn: i64,
}

const CHECKPOINT_MAGIC: &[u8; 8] = b"FXCKPT01";

/// Saves a store together with the journal position it is current to.
///
/// Both go in one file, written to a temporary name and renamed into place, so a crash
/// mid-write leaves the previous checkpoint intact — and the snapshot and the USN can never
/// disagree, which would silently skip or double-apply changes on restart.
pub fn save_checkpoint(path: &Path, store: &EntryStore, cp: Option<Checkpoint>) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut w = BufWriter::new(File::create(&tmp)?);
        w.write_all(CHECKPOINT_MAGIC)?;
        match cp {
            Some(c) => {
                w.write_all(&[1])?;
                w.write_all(&c.journal_id.to_le_bytes())?;
                w.write_all(&c.next_usn.to_le_bytes())?;
            }
            None => w.write_all(&[0; 17])?,
        }
        store.write_snapshot(&mut w)?;
        w.flush()?;
        w.get_ref().sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Inverse of [`save_checkpoint`]. Any damage is an error, which callers treat as "no
/// checkpoint" and rescan.
pub fn load_checkpoint(path: &Path) -> io::Result<(EntryStore, Option<Checkpoint>)> {
    let mut r = BufReader::new(File::open(path)?);
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)?;
    if &magic != CHECKPOINT_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a checkpoint",
        ));
    }
    let mut head = [0u8; 17];
    r.read_exact(&mut head)?;
    let cp = (head[0] == 1).then(|| Checkpoint {
        journal_id: u64::from_le_bytes(head[1..9].try_into().unwrap_or_default()),
        next_usn: i64::from_le_bytes(head[9..17].try_into().unwrap_or_default()),
    });
    let store = EntryStore::read_snapshot(&mut r)?;
    Ok((store, cp))
}

/// File name a volume's checkpoint is kept under inside the state directory.
pub fn checkpoint_file_name(volume: &str) -> String {
    let safe: String = volume
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("index-{safe}.fxck")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids of every live record.
    fn live_ids(store: &EntryStore) -> Vec<EntryId> {
        (0..store.len() as u32)
            .filter(|&ix| !store.record(ix).is_tombstone())
            .map(|ix| store.id(ix))
            .collect()
    }

    fn seq(seq: u64, number: u64) -> u64 {
        (seq << 48) | number
    }

    fn raw(id: u64, parent: u64, name: &str, is_dir: bool) -> RawRecord {
        RawRecord {
            id,
            parent,
            name: name.into(),
            is_dir,
            size: if is_dir { 0 } else { 10 },
            attrs: if is_dir { ATTR_DIRECTORY } else { 0x20 },
            modified: 1_000,
            created: 500,
        }
    }

    fn cand(ns: Namespace, name: &str) -> NameCandidate {
        NameCandidate {
            namespace: ns,
            parent: 5,
            name: name.into(),
        }
    }

    #[test]
    fn win32_name_wins_over_dos_alias_regardless_of_order() {
        let picked = pick_name([
            cand(Namespace::Dos, "PROGRA~1"),
            cand(Namespace::Win32, "Program Files"),
        ]);
        assert_eq!(picked.unwrap().name, "Program Files");
        let picked = pick_name([cand(Namespace::Win32AndDos, "short.txt")]);
        assert_eq!(picked.unwrap().name, "short.txt");
        // POSIX beats DOS; DOS alone is still indexed so the file stays findable.
        let picked = pick_name([cand(Namespace::Dos, "A~1"), cand(Namespace::Posix, "a")]);
        assert_eq!(picked.unwrap().name, "a");
        assert_eq!(
            pick_name([cand(Namespace::Dos, "A~1")]).unwrap().name,
            "A~1"
        );
        assert!(pick_name([]).is_none());
    }

    /// A small volume in MFT order: children before their directory, a metafile, the
    /// contents of `$Extend`, and a file whose parent slot was reused.
    fn volume() -> EntryStore {
        let mut b = VolumeBuilder::new("C".into(), r"C:\");
        b.push(raw(seq(1, 0), seq(5, 5), "$MFT", false));
        b.push(raw(seq(5, 5), seq(5, 5), ".", true));
        b.push(raw(seq(11, 11), seq(5, 5), "$Extend", true));
        // Child listed before its directory, as happens constantly in record order.
        b.push(raw(seq(1, 40), seq(1, 30), "report.pdf", false));
        b.push(raw(seq(1, 30), seq(5, 5), "Users", true));
        // $Extend\$RmMetadata and something beneath it.
        b.push(raw(seq(1, 50), seq(11, 11), "$RmMetadata", true));
        b.push(raw(seq(1, 51), seq(1, 50), "$Txf", true));
        b.push(raw(seq(1, 52), seq(1, 51), "deep", false));
        // Parent reference to record 30's *previous* incarnation (sequence 0): stale.
        b.push(raw(seq(1, 60), seq(0, 30), "stale.txt", false));
        assert_eq!(b.skipped(), 2, "$MFT and $Extend dropped on push");
        b.finish()
    }

    #[test]
    fn builder_links_out_of_order_parents_and_drops_metafiles() {
        let s = volume();
        let ids = live_ids(&s);
        let report = s.find(seq(1, 40)).unwrap();
        assert_eq!(s.path_of(report), r"C:\Users\report.pdf");
        assert_eq!(s.root(), s.find(seq(5, 5)));
        assert_eq!(s.path_of(s.root().unwrap()), r"C:\");
        for gone in [seq(1, 0), seq(11, 11), seq(1, 50), seq(1, 51), seq(1, 52)] {
            assert!(!ids.contains(&gone), "{gone:#x} should be excluded");
        }
    }

    #[test]
    fn stale_parent_references_do_not_attach_to_the_reused_slot() {
        let s = volume();
        let ix = s.find(seq(1, 60)).unwrap();
        assert!(s.record(ix).flags & flags::ORPHAN != 0);
        assert!(!s.is_descendant(ix, s.find(seq(1, 30)).unwrap()));
    }

    #[test]
    fn raw_records_map_attributes_and_kinds() {
        let mut r = raw(seq(1, 99), seq(5, 5), "x", false);
        r.attrs = 0x2 | 0x4;
        let m = r.meta();
        let rec = crate::store::Record {
            flags: m.kind_and_attrs,
            ..Default::default()
        };
        assert!(rec.attributes().hidden && rec.attributes().system);
        assert_eq!(rec.kind(), EntryKind::File);
        assert_eq!(m.size, 10);
        let d = raw(seq(1, 98), seq(5, 5), "d", true).meta();
        assert_eq!(d.size, 0, "directory sizes are not file sizes");
    }

    fn jr(usn: i64, id: u64, parent: u64, reason: u32, name: &str) -> JournalRecord {
        JournalRecord {
            usn,
            id,
            parent,
            reason,
            attrs: 0x20,
            name: name.into(),
            time: 7_000,
        }
    }

    fn no_stat() -> impl FnMut(EntryId, &str) -> Option<Meta> {
        |_, _| None
    }

    #[test]
    fn a_create_burst_becomes_one_created_event_with_statted_metadata() {
        let mut s = volume();
        let users = seq(1, 30);
        let f = seq(1, 70);
        let recs = vec![
            jr(1, f, users, reason::FILE_CREATE, "new.txt"),
            jr(
                2,
                f,
                users,
                reason::FILE_CREATE | reason::DATA_EXTEND,
                "new.txt",
            ),
            jr(
                3,
                f,
                users,
                reason::FILE_CREATE | reason::DATA_EXTEND | reason::CLOSE,
                "new.txt",
            ),
        ];
        let mut asked = Vec::new();
        let mut stat = |_: EntryId, p: &str| {
            asked.push(p.to_owned());
            Some(Meta::file(1234))
        };
        let ev = apply_journal(&mut s, &recs, &mut stat);
        assert_eq!(
            ev,
            vec![ChangeEvent::Created {
                volume: "C".into(),
                id: f
            }]
        );
        assert_eq!(
            asked,
            vec![r"C:\Users\new.txt".to_string()],
            "one stat per file"
        );
        let ix = s.find(f).unwrap();
        assert_eq!(s.record(ix).size, 1234);
        assert_eq!(s.path_of(ix), r"C:\Users\new.txt");

        // Replaying the same batch (resume from an older checkpoint) changes nothing.
        let ev = apply_journal(&mut s, &recs, &mut |_, _| Some(Meta::file(1234)));
        assert!(ev.is_empty());
    }

    #[test]
    fn create_then_delete_in_one_window_is_invisible() {
        let mut s = volume();
        let before = s.live_count();
        let t = seq(1, 71);
        let recs = vec![
            jr(1, t, seq(1, 30), reason::FILE_CREATE, "~tmp"),
            jr(
                2,
                t,
                seq(1, 30),
                reason::FILE_DELETE | reason::CLOSE,
                "~tmp",
            ),
        ];
        assert!(apply_journal(&mut s, &recs, &mut no_stat()).is_empty());
        assert_eq!(s.live_count(), before);
    }

    #[test]
    fn renames_pair_even_across_windows_and_report_both_names() {
        let mut s = volume();
        let f = seq(1, 40);
        let docs = seq(1, 80);
        apply_journal(
            &mut s,
            &[jr(1, docs, seq(5, 5), reason::FILE_CREATE, "Docs").with_attrs(ATTR_DIRECTORY)],
            &mut no_stat(),
        );
        // Old half in this window...
        let ev = apply_journal(
            &mut s,
            &[jr(2, f, seq(1, 30), reason::RENAME_OLD_NAME, "report.pdf")],
            &mut no_stat(),
        );
        assert!(ev.is_empty());
        // ...new half (moved into Docs, renamed) in the next.
        let ev = apply_journal(
            &mut s,
            &[jr(3, f, docs, reason::RENAME_NEW_NAME, "final.pdf")],
            &mut no_stat(),
        );
        assert_eq!(
            ev,
            vec![ChangeEvent::Renamed {
                volume: "C".into(),
                id: f,
                old_name: "report.pdf".into(),
                new_name: "final.pdf".into(),
            }]
        );
        assert_eq!(s.path_of(s.find(f).unwrap()), r"C:\Docs\final.pdf");
    }

    #[test]
    fn deletes_remove_and_report_once() {
        let mut s = volume();
        let f = seq(1, 40);
        let recs = vec![jr(
            1,
            f,
            seq(1, 30),
            reason::FILE_DELETE | reason::CLOSE,
            "report.pdf",
        )];
        let ev = apply_journal(&mut s, &recs, &mut no_stat());
        assert_eq!(
            ev,
            vec![ChangeEvent::Deleted {
                volume: "C".into(),
                id: f
            }]
        );
        assert!(s.find(f).is_none());
        assert!(apply_journal(&mut s, &recs, &mut no_stat()).is_empty());
    }

    #[test]
    fn modifications_report_only_visible_changes() {
        let mut s = volume();
        let f = seq(1, 40);
        let rec = *s.record(s.find(f).unwrap());
        let same = Meta {
            kind_and_attrs: rec.flags & !(flags::TOMBSTONE | flags::ORPHAN),
            size: rec.size,
            modified: rec.modified,
            created: rec.created,
        };
        let touch = vec![jr(1, f, seq(1, 30), reason::CLOSE, "report.pdf")];
        assert!(apply_journal(&mut s, &touch, &mut |_, _| Some(same)).is_empty());
        let grown = Meta { size: 99, ..same };
        let ev = apply_journal(&mut s, &touch, &mut |_, _| Some(grown));
        assert_eq!(
            ev,
            vec![ChangeEvent::Modified {
                volume: "C".into(),
                id: f
            }]
        );
        assert_eq!(s.record(s.find(f).unwrap()).size, 99);
    }

    #[test]
    fn unstattable_files_fall_back_to_journal_metadata() {
        let mut s = volume();
        let d = seq(1, 90);
        apply_journal(
            &mut s,
            &[jr(1, d, seq(5, 5), reason::FILE_CREATE, "gone-dir")
                .with_attrs(ATTR_DIRECTORY | 0x2)],
            &mut no_stat(),
        );
        let r = s.record(s.find(d).unwrap());
        assert_eq!(r.kind(), EntryKind::Dir);
        assert!(r.attributes().hidden);
        assert_eq!(r.modified, 7_000);
    }

    #[test]
    fn metafile_journal_churn_is_ignored() {
        let mut s = volume();
        let recs = vec![
            jr(1, seq(1, 0), seq(5, 5), reason::DATA_EXTEND, "$MFT"),
            jr(
                2,
                seq(1, 200),
                seq(11, 11),
                reason::FILE_CREATE,
                "$Something",
            ),
        ];
        assert!(apply_journal(&mut s, &recs, &mut no_stat()).is_empty());
    }

    #[test]
    fn filetime_converts_to_unix_millis() {
        assert_eq!(filetime_to_millis(0), 0);
        // 2020-01-01T00:00:00Z
        assert_eq!(
            filetime_to_millis(132_223_104_000_000_000),
            1_577_836_800_000
        );
    }

    #[test]
    fn checkpoints_round_trip_and_reject_damage() {
        let dir = std::env::temp_dir().join(format!("fx-ckpt-{}", std::process::id()));
        let path = dir.join(checkpoint_file_name("C"));
        let s = volume();
        let cp = Checkpoint {
            journal_id: 0xDEAD_BEEF,
            next_usn: 123_456,
        };
        save_checkpoint(&path, &s, Some(cp)).unwrap();
        let (back, got) = load_checkpoint(&path).unwrap();
        assert_eq!(got, Some(cp));
        assert_eq!(back.live_count(), s.live_count());
        assert_eq!(
            back.path_of(back.find(seq(1, 40)).unwrap()),
            r"C:\Users\report.pdf"
        );
        assert!(
            !path.with_extension("tmp").exists(),
            "temp file renamed away"
        );

        save_checkpoint(&path, &s, None).unwrap();
        assert_eq!(load_checkpoint(&path).unwrap().1, None);

        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(load_checkpoint(&path).is_err());
        std::fs::write(&path, b"garbage!garbage!garbage!garbage!").unwrap();
        assert!(load_checkpoint(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(checkpoint_file_name(r"C:\"), "index-C__.fxck");
    }

    impl JournalRecord {
        fn with_attrs(mut self, a: u32) -> Self {
            self.attrs = a;
            self
        }
    }
}
