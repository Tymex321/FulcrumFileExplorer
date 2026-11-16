//! Compact in-memory index storage.
//!
//! The layout is what decides whether a 5M-file volume costs ~250 MB or well over a
//! gigabyte, so it is deliberately columnar:
//!
//! ```text
//!   names:        "srcmain.rsCargo.toml…"   one contiguous arena, no per-name allocation
//!   names_lower:  "srcmain.rscargo.toml…"   case-folded mirror, *same offsets*
//!   records:      [ name_off: u32, name_len: u16, flags: u16, parent: u32,
//!                   size: u64, modified: i64, created: i64 ]          40 bytes
//!   ids:          [ EntryId ]                                           8 bytes
//!   by_number:    record number -> RecordIx                             4 bytes per id slot
//! ```
//!
//! Points that matter:
//!
//! - `parent` is an index into `records`, not an MFT frn, so it stays 32-bit even on huge
//!   volumes. `by_number` maps the low 48 bits of an id (the MFT record number, or the
//!   synthetic sequence number on other platforms) back to a record. Both id sources are
//!   dense, so a flat `Vec` beats a hash map by ~4x on memory and needs no hashing.
//! - Lookups compare the **full** id, sequence number included. NTFS reuses record numbers
//!   after deletion; a stale parent reference must not resolve to whatever file now
//!   occupies the slot.
//! - Deletions mark a tombstone rather than compacting, because compaction would
//!   invalidate every `parent` reference. Renames append the new name and orphan the old
//!   bytes. Both are reclaimed only by a rescan, which rebuilds the store from scratch.
//! - The lowercased mirror uses a *length-preserving* fold (see [`fold_lower`]), so one
//!   `(offset, len)` addresses both arenas and match offsets found in the folded copy are
//!   valid byte offsets into the original name.

use std::collections::HashMap;
use std::io::{self, Read, Write};

use fx_core::{Attributes, EntryId, EntryKind, FileEntry, Millis, VolumeId};

/// Index into [`EntryStore::records`]. Distinct from [`EntryId`], which is the
/// filesystem's own identifier.
pub type RecordIx = u32;

/// Sentinel for "no record": a root's parent, an empty `by_number` slot.
pub const NO_RECORD: RecordIx = RecordIx::MAX;

/// Low 48 bits of an [`EntryId`]. On NTFS this is the MFT record number; the high 16 bits
/// are the sequence number that changes each time the record is reused.
pub const ID_NUMBER_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// Records per scan chunk. Queries parallelise over chunks of this size, and the store
/// tracks per chunk whether its names lie back to back in the arena — the condition for
/// the query engine's whole-chunk SIMD scan (see [`EntryStore::chunk_is_contiguous`]).
pub const CHUNK: usize = 1 << 14;

/// Packed per-entry record. `Copy` and free of heap pointers, so the whole column is one
/// allocation the CPU can stream through.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Record {
    pub name_off: u32,
    pub name_len: u16,
    /// Packed attribute bits plus the entry kind; see [`flags`]. Expanded into
    /// [`fx_core::Attributes`] only when a row is returned.
    pub flags: u16,
    pub parent: RecordIx,
    pub size: u64,
    pub modified: i64,
    pub created: i64,
}

/// Bit layout of [`Record::flags`].
pub mod flags {
    /// Low three bits hold the [`fx_core::EntryKind`].
    pub const KIND_MASK: u16 = 0b111;
    pub const HIDDEN: u16 = 1 << 3;
    pub const SYSTEM: u16 = 1 << 4;
    pub const READONLY: u16 = 1 << 5;
    pub const ARCHIVE: u16 = 1 << 6;
    pub const COMPRESSED: u16 = 1 << 7;
    pub const ENCRYPTED: u16 = 1 << 8;
    pub const SPARSE: u16 = 1 << 9;
    pub const REPARSE: u16 = 1 << 10;
    /// Deleted. The slot stays so parent indices remain valid; queries skip it.
    pub const TOMBSTONE: u16 = 1 << 11;
    /// The parent reference could not be validated (stale sequence number, or a parent
    /// the scan never saw). Still searchable; its path is best-effort.
    pub const ORPHAN: u16 = 1 << 12;
}

impl Record {
    pub fn kind(&self) -> EntryKind {
        kind_from_bits(self.flags & flags::KIND_MASK)
    }

    pub fn is_tombstone(&self) -> bool {
        self.flags & flags::TOMBSTONE != 0
    }

    pub fn is_container(&self) -> bool {
        self.kind().is_container()
    }

    pub fn attributes(&self) -> Attributes {
        let f = self.flags;
        Attributes {
            hidden: f & flags::HIDDEN != 0,
            system: f & flags::SYSTEM != 0,
            readonly: f & flags::READONLY != 0,
            archive: f & flags::ARCHIVE != 0,
            compressed: f & flags::COMPRESSED != 0,
            encrypted: f & flags::ENCRYPTED != 0,
            sparse: f & flags::SPARSE != 0,
            reparse: f & flags::REPARSE != 0,
        }
    }
}

pub fn kind_bits(kind: EntryKind) -> u16 {
    match kind {
        EntryKind::File => 0,
        EntryKind::Dir => 1,
        EntryKind::Symlink => 2,
        EntryKind::Volume => 3,
        EntryKind::Unknown => 4,
    }
}

pub fn kind_from_bits(bits: u16) -> EntryKind {
    match bits {
        0 => EntryKind::File,
        1 => EntryKind::Dir,
        2 => EntryKind::Symlink,
        3 => EntryKind::Volume,
        _ => EntryKind::Unknown,
    }
}

/// Packs the IPC attribute struct plus a kind into [`Record::flags`] bits.
pub fn pack_flags(kind: EntryKind, a: &Attributes) -> u16 {
    let mut f = kind_bits(kind);
    for (on, bit) in [
        (a.hidden, flags::HIDDEN),
        (a.system, flags::SYSTEM),
        (a.readonly, flags::READONLY),
        (a.archive, flags::ARCHIVE),
        (a.compressed, flags::COMPRESSED),
        (a.encrypted, flags::ENCRYPTED),
        (a.sparse, flags::SPARSE),
        (a.reparse, flags::REPARSE),
    ] {
        if on {
            f |= bit;
        }
    }
    f
}

/// Everything about an entry except its name and position in the tree. What a scanner or
/// change feed hands the store.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Meta {
    pub kind_and_attrs: u16,
    pub size: u64,
    pub modified: Millis,
    pub created: Millis,
}

impl Meta {
    pub fn new(
        kind: EntryKind,
        attrs: &Attributes,
        size: u64,
        modified: Millis,
        created: Millis,
    ) -> Self {
        Self {
            kind_and_attrs: pack_flags(kind, attrs),
            size,
            modified,
            created,
        }
    }

    pub fn dir() -> Self {
        Self::new(EntryKind::Dir, &Attributes::default(), 0, 0, 0)
    }

    pub fn file(size: u64) -> Self {
        Self::new(EntryKind::File, &Attributes::default(), size, 0, 0)
    }
}

/// Case-folds a name for case-insensitive matching **without changing its byte length**.
///
/// Each char is replaced by its lowercase form only when that form is a single char of the
/// same UTF-8 width; otherwise it is kept as is. That covers ASCII and the overwhelming
/// majority of real-world names, and it is what lets the folded arena share offsets with
/// the original — a fold that changed lengths (`'İ'` lowercases to two chars) would need a
/// second offset column and would break match-range reporting. The query needle goes
/// through the same function, so both sides always agree.
pub fn fold_lower(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let mut lower = c.to_lowercase();
        match (lower.next(), lower.next()) {
            (Some(l), None) if l.len_utf8() == c.len_utf8() => out.push(l),
            _ => out.push(c),
        }
    }
    out
}

/// Columnar store for one volume (or, for the walk backend, one indexed root).
#[derive(Debug, Default, Clone)]
pub struct EntryStore {
    pub volume: VolumeId,
    /// Absolute path of the root record, e.g. `C:\` or `/Users/me`. Paths are rebuilt by
    /// appending names to this.
    pub mount: String,
    /// Path separator used when rebuilding paths.
    pub sep: char,
    pub records: Vec<Record>,
    /// Filesystem id per record, parallel to `records`.
    pub ids: Vec<EntryId>,
    /// Concatenated names, addressed by `(name_off, name_len)`.
    pub names: Vec<u8>,
    /// Folded mirror of `names`, same offsets, for case-insensitive matching.
    pub names_lower: Vec<u8>,
    /// Low 48 bits of an id -> record. [`NO_RECORD`] for empty slots.
    by_number: Vec<RecordIx>,
    root: Option<RecordIx>,
    /// `(parent, name hash) -> child`, enabled only by backends that receive change events
    /// keyed by path (the walk backend). The MFT backend receives ids and never needs it.
    children: Option<HashMap<(RecordIx, u64), RecordIx>>,
    live: usize,
    live_dirs: usize,
    /// Arena bytes no longer referenced by any live name, reclaimed on rescan.
    garbage_bytes: usize,
    /// Bit per [`CHUNK`]: set when the chunk's names are *not* laid out back to back in
    /// the arena (a rename appended a new name elsewhere). Clear bits let a query run one
    /// SIMD search over the chunk's slice of the arena instead of one call per name.
    scattered: Vec<u64>,
}

impl EntryStore {
    pub fn new(volume: VolumeId) -> Self {
        Self {
            volume,
            sep: std::path::MAIN_SEPARATOR,
            ..Default::default()
        }
    }

    /// A store whose root is `mount`, with paths joined by `sep`.
    pub fn with_mount(volume: VolumeId, mount: impl Into<String>, sep: char) -> Self {
        Self {
            volume,
            mount: mount.into(),
            sep,
            ..Default::default()
        }
    }

    /// Enables the `(parent, name) -> child` index needed for path lookups. Costs roughly
    /// 20 bytes per entry, so only backends that receive path-keyed events turn it on.
    pub fn enable_child_index(&mut self) {
        if self.children.is_some() {
            return;
        }
        let mut map = HashMap::with_capacity(self.records.len());
        for (ix, r) in self.records.iter().enumerate() {
            if !r.is_tombstone() && r.parent != NO_RECORD {
                map.insert((r.parent, name_hash(self.name_bytes(r))), ix as RecordIx);
            }
        }
        self.children = Some(map);
    }

    /// Number of record slots, tombstones included. Use [`EntryStore::live_count`] for
    /// what a user would call "entries".
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn live_count(&self) -> usize {
        self.live
    }

    pub fn live_dirs(&self) -> usize {
        self.live_dirs
    }

    pub fn garbage_bytes(&self) -> usize {
        self.garbage_bytes
    }

    pub fn root(&self) -> Option<RecordIx> {
        self.root
    }

    /// Approximate resident size, for [`fx_core::IndexStats::memory_bytes`].
    pub fn memory_bytes(&self) -> u64 {
        let children = self.children.as_ref().map_or(0, |m| {
            // hashbrown: key + value + one control byte per bucket, ~8/7 load factor.
            m.capacity() * (std::mem::size_of::<((RecordIx, u64), RecordIx)>() + 1)
        });
        (self.records.capacity() * std::mem::size_of::<Record>()
            + self.ids.capacity() * std::mem::size_of::<EntryId>()
            + self.by_number.capacity() * std::mem::size_of::<RecordIx>()
            + self.names.capacity()
            + self.names_lower.capacity()
            + self.mount.capacity()
            + children) as u64
    }

    #[inline]
    pub fn record(&self, ix: RecordIx) -> &Record {
        &self.records[ix as usize]
    }

    #[inline]
    pub fn name_bytes(&self, r: &Record) -> &[u8] {
        let a = r.name_off as usize;
        &self.names[a..a + r.name_len as usize]
    }

    #[inline]
    pub fn name_lower_bytes(&self, r: &Record) -> &[u8] {
        let a = r.name_off as usize;
        &self.names_lower[a..a + r.name_len as usize]
    }

    pub fn name(&self, ix: RecordIx) -> &str {
        // Names are pushed as UTF-8 and offsets always land on char boundaries.
        std::str::from_utf8(self.name_bytes(self.record(ix))).unwrap_or("")
    }

    pub fn id(&self, ix: RecordIx) -> EntryId {
        self.ids[ix as usize]
    }

    /// Appends a name to both arenas, returning its `(offset, len)`.
    ///
    /// Names longer than `u16::MAX` bytes cannot occur on any real filesystem (NTFS caps a
    /// component at 255 UTF-16 units, ≤ 765 UTF-8 bytes); one is truncated at a char
    /// boundary rather than corrupting the length field.
    pub fn intern(&mut self, name: &str) -> (u32, u16) {
        let name = truncate_to_boundary(name, u16::MAX as usize);
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name.as_bytes());
        let lower = fold_lower(name);
        debug_assert_eq!(lower.len(), name.len());
        self.names_lower.extend_from_slice(lower.as_bytes());
        (off, name.len() as u16)
    }

    /// Maps a filesystem id to its live record, validating the full id (sequence number
    /// included) so a reused MFT slot never resolves to the wrong file.
    pub fn find(&self, id: EntryId) -> Option<RecordIx> {
        let ix = self.slot(id)?;
        (self.ids[ix as usize] == id && !self.records[ix as usize].is_tombstone()).then_some(ix)
    }

    /// Whatever record currently occupies `id`'s number slot, regardless of sequence.
    fn slot(&self, id: EntryId) -> Option<RecordIx> {
        let n = (id & ID_NUMBER_MASK) as usize;
        match self.by_number.get(n) {
            Some(&ix) if ix != NO_RECORD => Some(ix),
            _ => None,
        }
    }

    /// Appends a record without resolving its parent. The bulk-load path: a scanner pushes
    /// everything first, then calls [`EntryStore::link_parents`] once, which is what lets
    /// the MFT scan run in record order even though parents often come after children.
    pub fn push_unlinked(&mut self, id: EntryId, name: &str, meta: Meta) -> RecordIx {
        let ix = self.records.len() as RecordIx;
        let (name_off, name_len) = self.intern(name);
        if !(ix as usize).is_multiple_of(CHUNK) {
            let prev = self.records[ix as usize - 1];
            if prev.name_off + prev.name_len as u32 != name_off {
                self.mark_scattered(ix);
            }
        }
        self.records.push(Record {
            name_off,
            name_len,
            flags: meta.kind_and_attrs,
            parent: NO_RECORD,
            size: meta.size,
            modified: meta.modified,
            created: meta.created,
        });
        self.ids.push(id);
        if let Some(old) = self.set_slot(id, ix) {
            // Duplicate id within one scan: keep the newest, tombstone the earlier one.
            self.tombstone_one(old);
        }
        self.live += 1;
        if kind_from_bits(meta.kind_and_attrs & flags::KIND_MASK).is_container() {
            self.live_dirs += 1;
        }
        ix
    }

    /// Resolves the parent ids recorded alongside a bulk load (`parent_ids[i]` belongs to
    /// record `i`). Parents are validated by full id; a stale or missing one marks the
    /// child [`flags::ORPHAN`]. `root_id` becomes the store root.
    pub fn link_parents(&mut self, parent_ids: &[EntryId], root_id: Option<EntryId>) {
        self.root = root_id.and_then(|id| self.find(id));
        for (ix, &pid) in parent_ids.iter().enumerate().take(self.records.len()) {
            if Some(ix as RecordIx) == self.root {
                self.records[ix].parent = NO_RECORD;
                continue;
            }
            match self.find(pid) {
                Some(p) if p as usize != ix && self.records[p as usize].is_container() => {
                    self.records[ix].parent = p;
                }
                _ => {
                    self.records[ix].parent = NO_RECORD;
                    self.records[ix].flags |= flags::ORPHAN;
                }
            }
        }
        if self.children.is_some() {
            self.children = None;
            self.enable_child_index();
        }
    }

    /// Declares the root record (the mount point itself). Excluded from search results.
    pub fn set_root(&mut self, ix: RecordIx) {
        self.root = Some(ix);
        self.records[ix as usize].parent = NO_RECORD;
        self.records[ix as usize].flags &= !flags::ORPHAN;
    }

    /// Adds a record under an already-resolved parent. Used by the walk scanner, which
    /// always sees a directory before its children.
    pub fn push(&mut self, id: EntryId, parent: RecordIx, name: &str, meta: Meta) -> RecordIx {
        let ix = self.push_unlinked(id, name, meta);
        self.records[ix as usize].parent = parent;
        if let Some(map) = self.children.as_mut() {
            let r = &self.records[ix as usize];
            let a = r.name_off as usize;
            let h = name_hash(&self.names[a..a + r.name_len as usize]);
            map.insert((parent, h), ix);
        }
        ix
    }

    /// Inserts or updates an entry from a change feed. Idempotent: replaying the same
    /// change twice leaves the store as replaying it once, which is what makes it safe to
    /// replay a journal from a checkpoint taken *before* a scan finished.
    ///
    /// Returns the record and whether it was newly created.
    pub fn upsert(
        &mut self,
        id: EntryId,
        parent_id: EntryId,
        name: &str,
        meta: Meta,
    ) -> (RecordIx, bool) {
        let parent = self
            .find(parent_id)
            .filter(|&p| self.records[p as usize].is_container());
        if let Some(ix) = self.find(id) {
            self.update_in_place(ix, parent, Some(name), Some(meta));
            return (ix, false);
        }
        let ix = self.push_unlinked(id, name, meta);
        self.attach(ix, parent);
        (ix, true)
    }

    /// Renames and/or moves an existing entry. Returns the previous name, or `None` when
    /// `id` is unknown.
    pub fn rename(
        &mut self,
        id: EntryId,
        new_parent_id: EntryId,
        new_name: &str,
    ) -> Option<String> {
        let ix = self.find(id)?;
        let old = self.name(ix).to_owned();
        let parent = self
            .find(new_parent_id)
            .filter(|&p| self.records[p as usize].is_container());
        self.update_in_place(ix, parent, Some(new_name), None);
        Some(old)
    }

    /// Replaces an entry's metadata, keeping its name and position.
    pub fn set_meta(&mut self, id: EntryId, meta: Meta) -> bool {
        match self.find(id) {
            Some(ix) => {
                let parent = self.records[ix as usize].parent;
                let parent = (parent != NO_RECORD).then_some(parent);
                self.update_in_place(ix, parent, None, Some(meta));
                true
            }
            None => false,
        }
    }

    fn update_in_place(
        &mut self,
        ix: RecordIx,
        parent: Option<RecordIx>,
        name: Option<&str>,
        meta: Option<Meta>,
    ) {
        self.unindex_child(ix);
        if let Some(name) = name {
            if self.name(ix) != name {
                let r = self.records[ix as usize];
                self.garbage_bytes += r.name_len as usize;
                let (off, len) = self.intern(name);
                self.mark_scattered(ix);
                let r = &mut self.records[ix as usize];
                r.name_off = off;
                r.name_len = len;
            }
        }
        if let Some(meta) = meta {
            let r = &mut self.records[ix as usize];
            let was_dir = r.is_container();
            let keep = r.flags & (flags::TOMBSTONE | flags::ORPHAN);
            r.flags = meta.kind_and_attrs | keep;
            r.size = meta.size;
            r.modified = meta.modified;
            r.created = meta.created;
            match (was_dir, r.is_container()) {
                (false, true) => self.live_dirs += 1,
                (true, false) => self.live_dirs -= 1,
                _ => {}
            }
        }
        if Some(ix) != self.root {
            self.attach(ix, parent);
        }
    }

    fn attach(&mut self, ix: RecordIx, parent: Option<RecordIx>) {
        let r = &mut self.records[ix as usize];
        match parent {
            Some(p) if p != ix => {
                r.parent = p;
                r.flags &= !flags::ORPHAN;
            }
            _ => {
                r.parent = NO_RECORD;
                r.flags |= flags::ORPHAN;
            }
        }
        self.index_child(ix);
    }

    fn index_child(&mut self, ix: RecordIx) {
        let r = self.records[ix as usize];
        if r.parent == NO_RECORD || r.is_tombstone() {
            return;
        }
        let h = name_hash(self.name_bytes(&r));
        if let Some(map) = self.children.as_mut() {
            map.insert((r.parent, h), ix);
        }
    }

    fn unindex_child(&mut self, ix: RecordIx) {
        let r = self.records[ix as usize];
        if r.parent == NO_RECORD {
            return;
        }
        let h = name_hash(self.name_bytes(&r));
        if let Some(map) = self.children.as_mut() {
            if map.get(&(r.parent, h)) == Some(&ix) {
                map.remove(&(r.parent, h));
            }
        }
    }

    fn set_slot(&mut self, id: EntryId, ix: RecordIx) -> Option<RecordIx> {
        let n = (id & ID_NUMBER_MASK) as usize;
        if n >= self.by_number.len() {
            // Grow geometrically; MFT record numbers are dense, so the slack is bounded.
            let want = (n + 1).max(self.by_number.len() * 3 / 2);
            self.by_number.resize(want, NO_RECORD);
        }
        let prev = std::mem::replace(&mut self.by_number[n], ix);
        (prev != NO_RECORD && prev != ix && !self.records[prev as usize].is_tombstone())
            .then_some(prev)
    }

    fn tombstone_one(&mut self, ix: RecordIx) {
        let r = self.records[ix as usize];
        if r.is_tombstone() {
            return;
        }
        self.unindex_child(ix);
        let r = &mut self.records[ix as usize];
        r.flags |= flags::TOMBSTONE;
        self.live -= 1;
        if r.is_container() {
            self.live_dirs -= 1;
        }
        self.garbage_bytes += r.name_len as usize;
        let id = self.ids[ix as usize];
        let n = (id & ID_NUMBER_MASK) as usize;
        if self.by_number.get(n) == Some(&ix) {
            self.by_number[n] = NO_RECORD;
        }
    }

    /// Deletes an entry. Returns `false` when it was not present.
    ///
    /// Only the one record is tombstoned: the USN journal reports every deleted child
    /// individually, so the MFT backend never needs a subtree sweep.
    pub fn remove(&mut self, id: EntryId) -> bool {
        match self.find(id) {
            Some(ix) => {
                self.tombstone_one(ix);
                true
            }
            None => false,
        }
    }

    /// Deletes an entry and everything beneath it. Needed by change feeds that report a
    /// removed directory as a single event (filesystem notifications on macOS and Linux).
    /// Returns the number of records removed.
    ///
    /// O(n) over the store, which is acceptable because it only runs for directory
    /// deletions on the fallback backend.
    pub fn remove_subtree(&mut self, id: EntryId) -> usize {
        let Some(top) = self.find(id) else { return 0 };
        let mut dead = vec![false; self.records.len()];
        dead[top as usize] = true;
        let mut removed = 0;
        // Children usually follow their parent, so one forward pass catches nearly all of
        // them; loop until a pass finds nothing for records that were moved under a
        // directory created later.
        loop {
            let mut changed = false;
            for ix in 0..self.records.len() {
                let r = self.records[ix];
                if !dead[ix]
                    && !r.is_tombstone()
                    && r.parent != NO_RECORD
                    && dead[r.parent as usize]
                {
                    dead[ix] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for (ix, &d) in dead.iter().enumerate() {
            if d && !self.records[ix].is_tombstone() {
                self.tombstone_one(ix as RecordIx);
                removed += 1;
            }
        }
        removed
    }

    /// Finds a live child by exact name. Requires [`EntryStore::enable_child_index`].
    pub fn child(&self, parent: RecordIx, name: &str) -> Option<RecordIx> {
        let map = self.children.as_ref()?;
        let &ix = map.get(&(parent, name_hash(name.as_bytes())))?;
        let r = &self.records[ix as usize];
        // Verify: the key is a hash, and a 64-bit collision between siblings, while
        // astronomically unlikely, must not return the wrong file.
        (!r.is_tombstone() && r.parent == parent && self.name_bytes(r) == name.as_bytes())
            .then_some(ix)
    }

    /// Resolves a path relative to the mount by walking the child index from the root.
    /// `components` are the path segments below the mount.
    pub fn lookup<'a>(&self, components: impl IntoIterator<Item = &'a str>) -> Option<RecordIx> {
        let mut cur = self.root?;
        for c in components {
            cur = self.child(cur, c)?;
        }
        Some(cur)
    }

    /// Reconstructs a record's absolute path by walking the parent chain.
    pub fn path_of(&self, ix: RecordIx) -> String {
        let mut out = String::with_capacity(64);
        self.write_path(ix, false, &mut out);
        out
    }

    /// Writes `ix`'s path into `out` (cleared first). `lower` selects the folded arena,
    /// for case-insensitive full-path matching.
    pub fn write_path(&self, ix: RecordIx, lower: bool, out: &mut String) {
        out.clear();
        if Some(ix) == self.root {
            push_folded(out, &self.mount, lower);
            return;
        }
        // Collect the chain bottom-up, then emit top-down. Bounded by record count so a
        // corrupt parent cycle cannot spin forever.
        let mut chain: [RecordIx; 64] = [NO_RECORD; 64];
        let mut overflow: Vec<RecordIx> = Vec::new();
        let mut n = 0usize;
        let mut cur = ix;
        let mut steps = 0usize;
        while cur != NO_RECORD && Some(cur) != self.root && steps <= self.records.len() {
            if n < chain.len() {
                chain[n] = cur;
            } else {
                overflow.push(cur);
            }
            n += 1;
            steps += 1;
            cur = self.records[cur as usize].parent;
        }
        push_folded(out, &self.mount, lower);
        let total = n;
        for i in (0..total).rev() {
            let r = if i < chain.len() {
                chain[i]
            } else {
                overflow[i - chain.len()]
            };
            if !out.is_empty() && !out.ends_with(self.sep) {
                out.push(self.sep);
            }
            let rec = &self.records[r as usize];
            let bytes = if lower {
                self.name_lower_bytes(rec)
            } else {
                self.name_bytes(rec)
            };
            out.push_str(std::str::from_utf8(bytes).unwrap_or(""));
        }
    }

    /// Whether `ix` lies strictly beneath `ancestor`.
    pub fn is_descendant(&self, ix: RecordIx, ancestor: RecordIx) -> bool {
        let mut cur = self.records[ix as usize].parent;
        let mut steps = 0usize;
        while cur != NO_RECORD && steps <= self.records.len() {
            if cur == ancestor {
                return true;
            }
            cur = self.records[cur as usize].parent;
            steps += 1;
        }
        false
    }

    /// Materialises the IPC row for one record.
    pub fn to_entry(&self, ix: RecordIx) -> FileEntry {
        let r = self.record(ix);
        let name = self.name(ix).to_owned();
        let kind = r.kind();
        let ext = if kind.is_container() {
            None
        } else {
            FileEntry::split_ext(&name).1
        };
        let parent = if r.parent == NO_RECORD {
            0
        } else {
            self.ids[r.parent as usize]
        };
        FileEntry {
            id: self.ids[ix as usize],
            parent,
            name,
            ext,
            size: r.size,
            modified: r.modified,
            created: r.created,
            kind,
            attrs: r.attributes(),
        }
    }

    /// Shrinks every column to fit. Called once after a bulk scan, where `Vec` growth
    /// would otherwise leave up to half of each column as slack.
    /// Whether chunk `c` (records `c * CHUNK ..`) has its names back to back in the arena,
    /// so one search over `names[first.name_off .. last end]` sees exactly those names.
    #[inline]
    pub fn chunk_is_contiguous(&self, c: usize) -> bool {
        self.scattered
            .get(c / 64)
            .is_none_or(|w| w & (1u64 << (c % 64)) == 0)
    }

    fn mark_scattered(&mut self, ix: RecordIx) {
        let c = ix as usize / CHUNK;
        if c / 64 >= self.scattered.len() {
            self.scattered.resize(c / 64 + 1, 0);
        }
        self.scattered[c / 64] |= 1u64 << (c % 64);
    }

    pub fn shrink_to_fit(&mut self) {
        self.records.shrink_to_fit();
        self.ids.shrink_to_fit();
        self.names.shrink_to_fit();
        self.names_lower.shrink_to_fit();
        self.by_number.shrink_to_fit();
    }

    /// Serialises the store so a restart can resume from a journal checkpoint instead of
    /// rescanning. Plain little-endian columns behind a magic/version header: the format is
    /// private to one machine, and a mismatch simply triggers a rescan.
    pub fn write_snapshot(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(SNAPSHOT_MAGIC)?;
        w.write_all(&SNAPSHOT_VERSION.to_le_bytes())?;
        write_str(w, &self.volume)?;
        write_str(w, &self.mount)?;
        w.write_all(&(self.sep as u32).to_le_bytes())?;
        w.write_all(&self.root.unwrap_or(NO_RECORD).to_le_bytes())?;
        w.write_all(&(self.records.len() as u64).to_le_bytes())?;
        for r in &self.records {
            w.write_all(&r.name_off.to_le_bytes())?;
            w.write_all(&r.name_len.to_le_bytes())?;
            w.write_all(&r.flags.to_le_bytes())?;
            w.write_all(&r.parent.to_le_bytes())?;
            w.write_all(&r.size.to_le_bytes())?;
            w.write_all(&r.modified.to_le_bytes())?;
            w.write_all(&r.created.to_le_bytes())?;
        }
        for id in &self.ids {
            w.write_all(&id.to_le_bytes())?;
        }
        write_bytes(w, &self.names)?;
        Ok(())
    }

    /// Inverse of [`EntryStore::write_snapshot`]. Validates every offset so a truncated or
    /// tampered file is rejected rather than producing out-of-bounds reads later.
    pub fn read_snapshot(r: &mut impl Read) -> io::Result<Self> {
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != SNAPSHOT_MAGIC || read_u32(r)? != SNAPSHOT_VERSION {
            return Err(invalid("not an index snapshot of this version"));
        }
        let volume = read_str(r)?;
        let mount = read_str(r)?;
        let sep = char::from_u32(read_u32(r)?).ok_or_else(|| invalid("bad separator"))?;
        let root = read_u32(r)?;
        let n = read_u64(r)? as usize;
        // Bound before allocating: a corrupt count must not request terabytes.
        if n > (1 << 32) {
            return Err(invalid("record count out of range"));
        }
        let mut records = Vec::with_capacity(n.min(1 << 24));
        for _ in 0..n {
            records.push(Record {
                name_off: read_u32(r)?,
                name_len: read_u16(r)?,
                flags: read_u16(r)?,
                parent: read_u32(r)?,
                size: read_u64(r)?,
                modified: read_u64(r)? as i64,
                created: read_u64(r)? as i64,
            });
        }
        let mut ids = Vec::with_capacity(n.min(1 << 24));
        for _ in 0..n {
            ids.push(read_u64(r)?);
        }
        let names = read_bytes(r)?;
        let names_str = std::str::from_utf8(&names).map_err(|_| invalid("names not UTF-8"))?;
        for rec in &records {
            let end = rec.name_off as usize + rec.name_len as usize;
            if end > names.len()
                || !names_str.is_char_boundary(rec.name_off as usize)
                || !names_str.is_char_boundary(end)
                || (rec.parent != NO_RECORD && rec.parent as usize >= n)
            {
                return Err(invalid("record out of bounds"));
            }
        }
        if root != NO_RECORD && root as usize >= n {
            return Err(invalid("root out of bounds"));
        }

        // The folded arena and the id map are derived data: rebuilding them is cheaper
        // than storing them, and guarantees they match this build's fold rules.
        let names_lower = fold_lower(names_str).into_bytes();
        let mut store = EntryStore {
            volume,
            mount,
            sep,
            records,
            ids,
            names,
            names_lower,
            root: (root != NO_RECORD).then_some(root),
            ..Default::default()
        };
        if store.names_lower.len() != store.names.len() {
            return Err(invalid("fold changed arena length"));
        }
        store.rebuild_derived();
        Ok(store)
    }

    fn rebuild_derived(&mut self) {
        self.by_number.clear();
        self.live = 0;
        self.live_dirs = 0;
        let mut referenced = 0usize;
        for ix in 0..self.records.len() {
            let r = self.records[ix];
            if r.is_tombstone() {
                continue;
            }
            self.live += 1;
            referenced += r.name_len as usize;
            if r.is_container() {
                self.live_dirs += 1;
            }
            let n = (self.ids[ix] & ID_NUMBER_MASK) as usize;
            if n >= self.by_number.len() {
                self.by_number.resize(n + 1, NO_RECORD);
            }
            self.by_number[n] = ix as RecordIx;
        }
        self.garbage_bytes = self.names.len().saturating_sub(referenced);
        self.scattered.clear();
        for ix in 1..self.records.len() {
            let (prev, r) = (self.records[ix - 1], self.records[ix]);
            if !ix.is_multiple_of(CHUNK) && prev.name_off + prev.name_len as u32 != r.name_off {
                self.mark_scattered(ix as RecordIx);
            }
        }
    }
}

const SNAPSHOT_MAGIC: &[u8; 8] = b"FXIDXSNP";
const SNAPSHOT_VERSION: u32 = 1;

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_owned())
}

fn read_u16(r: &mut impl Read) -> io::Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn write_bytes(w: &mut impl Write, b: &[u8]) -> io::Result<()> {
    w.write_all(&(b.len() as u64).to_le_bytes())?;
    w.write_all(b)
}

fn write_str(w: &mut impl Write, s: &str) -> io::Result<()> {
    write_bytes(w, s.as_bytes())
}

fn read_bytes(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let n = read_u64(r)?;
    if n > (1 << 34) {
        return Err(invalid("length out of range"));
    }
    let mut out = Vec::new();
    r.take(n).read_to_end(&mut out)?;
    if out.len() as u64 != n {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated snapshot",
        ));
    }
    Ok(out)
}

fn read_str(r: &mut impl Read) -> io::Result<String> {
    String::from_utf8(read_bytes(r)?).map_err(|_| invalid("string not UTF-8"))
}

fn push_folded(out: &mut String, s: &str, lower: bool) {
    if lower {
        out.push_str(&fold_lower(s));
    } else {
        out.push_str(s);
    }
}

fn truncate_to_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// FNV-1a. Names are short, so a simple multiplicative hash beats SipHash here and the
/// child map is not exposed to adversarial keys (names come from the local filesystem).
fn name_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> EntryStore {
        // /root
        //   src/        (id 2)
        //     main.rs   (id 3)
        //   README.md   (id 4)
        let mut s = EntryStore::with_mount("v".into(), "/root", '/');
        let root = s.push(1, NO_RECORD, "root", Meta::dir());
        s.set_root(root);
        let src = s.push(2, root, "src", Meta::dir());
        s.push(3, src, "main.rs", Meta::file(10));
        s.push(4, root, "README.md", Meta::file(20));
        s
    }

    #[test]
    fn intern_round_trips_names_and_tracks_offsets() {
        let mut s = EntryStore::new("C".into());
        let (off_a, len_a) = s.intern("main.rs");
        let (off_b, len_b) = s.intern("Cargo.toml");

        // Second name starts where the first ended: the arena is contiguous.
        assert_eq!(off_a, 0);
        assert_eq!(off_b, len_a as u32);

        s.records.push(Record {
            name_off: off_a,
            name_len: len_a,
            ..Default::default()
        });
        s.records.push(Record {
            name_off: off_b,
            name_len: len_b,
            ..Default::default()
        });
        assert_eq!(s.name(0), "main.rs");
        assert_eq!(s.name(1), "Cargo.toml");
    }

    #[test]
    fn lowercase_mirror_stays_offset_aligned() {
        let mut s = EntryStore::new("C".into());
        s.intern("README.md");
        // Same offsets must address both arenas, or case-insensitive search reads garbage.
        assert_eq!(s.names.len(), s.names_lower.len());
        assert_eq!(&s.names_lower[..9], b"readme.md");
    }

    #[test]
    fn fold_preserves_length_even_for_awkward_unicode() {
        for s in [
            "ÄÖÜ.txt",
            "İstanbul",
            "ΣΊΣΥΦΟΣ",
            "ẞtraße",
            "日本語.TXT",
            "Ⅻ",
        ] {
            let f = fold_lower(s);
            assert_eq!(f.len(), s.len(), "{s:?} folded to {f:?}");
        }
        assert_eq!(fold_lower("ÄÖÜ.TXT"), "äöü.txt");
        // 'İ' lowercases to two chars, so it is kept rather than shifting every offset.
        assert_eq!(fold_lower("İx"), "İx");
    }

    #[test]
    fn record_stays_compact() {
        // Guards the memory budget: 40 bytes/entry keeps a 5M-file volume near 200 MB.
        assert!(
            std::mem::size_of::<Record>() <= 40,
            "Record grew to {} bytes; revisit the memory budget",
            std::mem::size_of::<Record>()
        );
    }

    #[test]
    fn flags_round_trip_every_attribute_and_kind() {
        let attrs = Attributes {
            hidden: true,
            system: false,
            readonly: true,
            archive: false,
            compressed: true,
            encrypted: false,
            sparse: true,
            reparse: true,
        };
        for kind in [
            EntryKind::File,
            EntryKind::Dir,
            EntryKind::Symlink,
            EntryKind::Volume,
            EntryKind::Unknown,
        ] {
            let r = Record {
                flags: pack_flags(kind, &attrs),
                ..Default::default()
            };
            assert_eq!(r.kind(), kind);
            assert_eq!(r.attributes(), attrs);
            assert!(!r.is_tombstone());
        }
    }

    #[test]
    fn paths_are_rebuilt_from_the_parent_chain() {
        let s = tree();
        assert_eq!(s.path_of(s.find(3).unwrap()), "/root/src/main.rs");
        assert_eq!(s.path_of(s.find(4).unwrap()), "/root/README.md");
        assert_eq!(s.path_of(s.root().unwrap()), "/root");

        let mut out = String::new();
        s.write_path(s.find(4).unwrap(), true, &mut out);
        assert_eq!(out, "/root/readme.md");
    }

    #[test]
    fn windows_mount_with_trailing_separator_does_not_double_it() {
        let mut s = EntryStore::with_mount("C".into(), r"C:\", '\\');
        let root = s.push(5, NO_RECORD, ".", Meta::dir());
        s.set_root(root);
        let users = s.push(100, root, "Users", Meta::dir());
        s.push(101, users, "a.txt", Meta::file(1));
        assert_eq!(s.path_of(s.find(101).unwrap()), r"C:\Users\a.txt");
    }

    #[test]
    fn find_validates_the_sequence_number() {
        let mut s = EntryStore::new("C".into());
        let seq1 = (1u64 << 48) | 42;
        let seq2 = (2u64 << 48) | 42;
        s.push_unlinked(seq1, "old.txt", Meta::file(1));
        assert!(s.find(seq1).is_some());
        // Same record number, different sequence: a reused MFT slot. Must not resolve.
        assert!(s.find(seq2).is_none());
    }

    #[test]
    fn link_parents_rejects_stale_and_non_directory_parents() {
        let mut s = EntryStore::with_mount("C".into(), r"C:\", '\\');
        let dir_v1 = (1u64 << 48) | 30;
        let file = 31u64;
        let mut parents = Vec::new();
        s.push_unlinked(5, ".", Meta::dir());
        parents.push(5);
        s.push_unlinked(dir_v1, "dir", Meta::dir());
        parents.push(5);
        // Child of the current incarnation of record 30: valid.
        s.push_unlinked(40, "ok.txt", Meta::file(1));
        parents.push(dir_v1);
        // Child of an older incarnation of record 30: stale, must become an orphan.
        s.push_unlinked(41, "stale.txt", Meta::file(1));
        parents.push(30);
        // "Parent" is a file: corrupt, orphan.
        s.push_unlinked(file, "f", Meta::file(1));
        parents.push(5);
        s.push_unlinked(42, "under-file", Meta::file(1));
        parents.push(file);
        s.link_parents(&parents, Some(5));

        assert_eq!(s.path_of(s.find(40).unwrap()), r"C:\dir\ok.txt");
        let stale = s.record(s.find(41).unwrap());
        assert_eq!(stale.parent, NO_RECORD);
        assert!(stale.flags & flags::ORPHAN != 0);
        assert!(s.record(s.find(42).unwrap()).flags & flags::ORPHAN != 0);
    }

    #[test]
    fn remove_tombstones_without_shifting_indices() {
        let mut s = tree();
        let main_ix = s.find(3).unwrap();
        assert!(s.remove(4));
        assert!(!s.remove(4), "second removal is a no-op");
        assert!(s.find(4).is_none());
        // Indices of survivors are unchanged, so parent links stay valid.
        assert_eq!(s.find(3), Some(main_ix));
        assert_eq!(s.path_of(main_ix), "/root/src/main.rs");
        assert_eq!(s.live_count(), 3);
        assert!(s.garbage_bytes() >= "README.md".len());
    }

    #[test]
    fn remove_subtree_takes_descendants_with_it() {
        let mut s = tree();
        assert_eq!(s.remove_subtree(2), 2);
        assert!(s.find(2).is_none());
        assert!(s.find(3).is_none());
        assert!(s.find(4).is_some());
        assert_eq!(s.live_dirs(), 1);
    }

    #[test]
    fn upsert_is_idempotent_and_updates_in_place() {
        let mut s = tree();
        let (ix, created) = s.upsert(10, 2, "new.rs", Meta::file(5));
        assert!(created);
        let before = s.len();
        let (ix2, created2) = s.upsert(10, 2, "new.rs", Meta::file(5));
        assert!(!created2);
        assert_eq!(ix, ix2);
        assert_eq!(s.len(), before, "replaying a create must not duplicate");
        assert_eq!(s.path_of(ix), "/root/src/new.rs");

        // Same id, new size: updated, not duplicated.
        s.upsert(10, 2, "new.rs", Meta::file(99));
        assert_eq!(s.record(ix).size, 99);
    }

    #[test]
    fn upsert_with_reused_number_replaces_the_stale_record() {
        let mut s = tree();
        let reused = (1u64 << 48) | 3; // record 3, next sequence
        let (ix, created) = s.upsert(reused, 1, "other.txt", Meta::file(1));
        assert!(created);
        assert!(s.find(3).is_none(), "old incarnation must be gone");
        assert_eq!(s.find(reused), Some(ix));
    }

    #[test]
    fn rename_moves_and_reports_the_old_name() {
        let mut s = tree();
        assert_eq!(s.rename(3, 1, "lib.rs").as_deref(), Some("main.rs"));
        let ix = s.find(3).unwrap();
        assert_eq!(s.path_of(ix), "/root/lib.rs");
        assert_eq!(s.rename(999, 1, "x"), None);
    }

    #[test]
    fn child_index_tracks_renames_and_removals() {
        let mut s = tree();
        s.enable_child_index();
        assert_eq!(s.lookup(["src", "main.rs"]), s.find(3));
        s.rename(3, 2, "renamed.rs");
        assert_eq!(s.lookup(["src", "main.rs"]), None);
        assert_eq!(s.lookup(["src", "renamed.rs"]), s.find(3));
        s.remove(3);
        assert_eq!(s.lookup(["src", "renamed.rs"]), None);
        let (ix, _) = s.upsert(50, 2, "fresh", Meta::file(0));
        assert_eq!(s.lookup(["src", "fresh"]), Some(ix));
    }

    #[test]
    fn to_entry_matches_the_ipc_contract() {
        let s = tree();
        let e = s.to_entry(s.find(4).unwrap());
        assert_eq!(e.name, "README.md");
        assert_eq!(e.ext.as_deref(), Some("md"));
        assert_eq!(e.size, 20);
        assert_eq!(e.parent, 1);
        assert_eq!(e.kind, EntryKind::File);
        // Directories never report an extension, even when named like a file.
        let mut s = tree();
        let d = s.push(9, 0, "folder.d", Meta::dir());
        assert_eq!(s.to_entry(d).ext, None);
    }

    #[test]
    fn snapshot_round_trips_and_rejects_corruption() {
        let mut s = tree();
        s.remove(4);
        s.rename(3, 1, "Main.RS");
        let mut buf = Vec::new();
        s.write_snapshot(&mut buf).unwrap();

        let back = EntryStore::read_snapshot(&mut buf.as_slice()).unwrap();
        assert_eq!(back.records, s.records);
        assert_eq!(back.ids, s.ids);
        assert_eq!(back.names_lower, s.names_lower);
        assert_eq!(back.live_count(), s.live_count());
        assert_eq!(back.find(3), s.find(3));
        assert!(back.find(4).is_none());
        assert_eq!(back.path_of(back.find(3).unwrap()), "/root/Main.RS");

        // Truncation anywhere must be an error, never a panic or a half-loaded store.
        for cut in [4, 20, buf.len() / 2, buf.len() - 1] {
            assert!(EntryStore::read_snapshot(&mut &buf[..cut]).is_err());
        }
        // An out-of-range parent must be rejected.
        let mut bad = s.clone();
        bad.records[1].parent = 1_000;
        let mut buf = Vec::new();
        bad.write_snapshot(&mut buf).unwrap();
        assert!(EntryStore::read_snapshot(&mut buf.as_slice()).is_err());
    }

    #[test]
    fn chunk_contiguity_tracks_renames_and_survives_snapshots() {
        let mut s = EntryStore::with_mount("v".into(), "/v", '/');
        let root = s.push(1, NO_RECORD, "v", Meta::dir());
        s.set_root(root);
        for i in 2..(CHUNK as u64 * 2 + 10) {
            s.push(i, root, &format!("n{i}"), Meta::file(0));
        }
        assert!(s.chunk_is_contiguous(0) && s.chunk_is_contiguous(1));
        // A rename appends its name at the arena's end: that chunk is no longer contiguous.
        s.rename(CHUNK as u64 + 5, 1, "renamed");
        assert!(s.chunk_is_contiguous(0));
        assert!(!s.chunk_is_contiguous(1));
        // Appends after the rename follow the orphaned bytes, so the tail chunk is broken
        // too even though no record in it was renamed.
        s.push(9_999_999, root, "late", Meta::file(0));
        assert!(!s.chunk_is_contiguous(2));

        let mut buf = Vec::new();
        s.write_snapshot(&mut buf).unwrap();
        let back = EntryStore::read_snapshot(&mut buf.as_slice()).unwrap();
        for c in 0..3 {
            assert_eq!(
                back.chunk_is_contiguous(c),
                s.chunk_is_contiguous(c),
                "chunk {c}"
            );
        }
    }

    #[test]
    fn overlong_names_are_truncated_at_a_char_boundary() {
        let mut s = EntryStore::new("v".into());
        let long = "é".repeat(40_000); // 80_000 bytes
        let (_, len) = s.intern(&long);
        assert!(len as usize <= u16::MAX as usize);
        assert!(std::str::from_utf8(&s.names).is_ok());
    }
}
