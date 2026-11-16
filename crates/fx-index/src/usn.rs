//! USN change journal reader. **Windows only.**
//!
//! This is what keeps the index live without rescanning: NTFS records every mutation into
//! a per-volume journal, and replaying it turns "stay current" into an incremental update
//! costing microseconds per change.
//!
//! This module is deliberately thin. It opens the journal, reads raw records and converts
//! them to [`JournalRecord`]; everything that *decides* something — coalescing a burst,
//! pairing renames, collapsing create+delete — is in [`crate::ntfs::apply_journal`], where
//! the macOS test suite covers it. The watch loop itself lives in [`crate::mft`].
//!
//! ## Failure classification
//!
//! The one thing this module must get right is telling the reasons a read can fail apart,
//! because each needs a different response:
//!
//! - **Desynced** — the saved position was overwritten (the journal wrapped), or the
//!   journal was deleted and recreated with a new id. Deltas are lost; the index for the
//!   volume must be rebuilt. Silently continuing would leave it permanently wrong.
//! - **Unavailable** — the volume has no journal at all (disabled by an administrator, or
//!   a volume type that never had one). The index is still correct right after a scan, it
//!   just cannot be kept live; the caller falls back to periodic rescans.
//! - **Failed** — anything else, including missing elevation, reported as an
//!   [`fx_core::FxError`].

use fx_core::{FxError, VolumeId};
use ntfs_reader::{
    HistorySize, Journal, JournalOptions, JournalPosition, NextUsn, NtfsReaderError, Reason,
    UsnRecord, Volume,
};

use crate::mft::{device_path_for, map_ntfs_error};
use crate::ntfs::{reason, Checkpoint, JournalRecord};

pub use crate::COALESCE_WINDOW_MS;

/// Bytes per journal read. Large enough that a burst drains in a few calls; each call is a
/// synchronous kernel round trip.
const READ_BUFFER: usize = 256 * 1024;

/// Upper bound on reads per [`UsnJournal::read_available`] call, so one enormous burst
/// cannot starve the coalescing loop of a chance to publish what it has.
const MAX_READS_PER_DRAIN: usize = 64;

/// Why the journal could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalError {
    /// Records between the checkpoint and now are gone: rescan the volume.
    Desynced,
    /// The volume has no active journal: changes cannot be followed.
    Unavailable(String),
    Failed(FxError),
}

impl From<NtfsReaderError> for JournalError {
    fn from(e: NtfsReaderError) -> Self {
        match e {
            NtfsReaderError::JournalEntryDeleted | NtfsReaderError::JournalIdMismatch => {
                JournalError::Desynced
            }
            NtfsReaderError::JournalNotActive | NtfsReaderError::JournalDeleteInProgress => {
                JournalError::Unavailable(e.to_string())
            }
            other => JournalError::Failed(map_ntfs_error(other, "usn journal")),
        }
    }
}

/// One volume's change journal, positioned at a [`Checkpoint`].
#[derive(Debug)]
pub struct UsnJournal {
    volume: VolumeId,
    journal: Journal,
}

impl UsnJournal {
    /// Opens the journal, resuming at `from` or, without one, at the journal's current end
    /// (only changes made from now on).
    ///
    /// A checkpoint older than the oldest record still in the journal is reported as
    /// [`JournalError::Desynced`] here rather than on the first read, so the caller can
    /// rescan before it starts serving queries from a stale snapshot.
    pub fn open(volume: &VolumeId, from: Option<Checkpoint>) -> Result<Self, JournalError> {
        let v = Volume::new(device_path_for(volume))?;
        let next_usn = match from {
            Some(cp) => NextUsn::Custom(JournalPosition {
                journal_id: cp.journal_id,
                usn: cp.next_usn,
            }),
            None => NextUsn::Next,
        };
        let journal = Journal::new(
            v,
            JournalOptions {
                reason_mask: Reason::from_bits(reason::INTERESTING),
                next_usn,
                // Renames are paired through index state, not journal history (see
                // `apply_journal`), so keeping old-name records would only cost memory.
                max_history_size: HistorySize::Limited(0),
            },
        )?;
        if let Some(cp) = from {
            if cp.next_usn < journal.first_usn() {
                return Err(JournalError::Desynced);
            }
        }
        Ok(Self {
            volume: volume.clone(),
            journal,
        })
    }

    pub fn volume(&self) -> &str {
        &self.volume
    }

    /// The position everything read so far brings the index up to.
    pub fn checkpoint(&self) -> Checkpoint {
        let p = self.journal.position();
        Checkpoint {
            journal_id: p.journal_id,
            next_usn: p.usn,
        }
    }

    /// Reads every record available right now (up to a bounded number of reads), without
    /// blocking for new ones. An empty result means nothing changed since the last call.
    pub fn read_available(&mut self) -> Result<Vec<JournalRecord>, JournalError> {
        let mut out = Vec::new();
        for _ in 0..MAX_READS_PER_DRAIN {
            let res = self.journal.read_sized(READ_BUFFER)?;
            out.extend(res.records.iter().map(convert));
            if res.caught_up {
                break;
            }
        }
        Ok(out)
    }
}

fn convert(r: &UsnRecord) -> JournalRecord {
    JournalRecord {
        usn: r.usn,
        // NTFS ids always fit in 64 bits; a value that does not (a ReFS 128-bit id) cannot
        // name anything in an MFT-built store, so 0 simply never matches.
        id: r.file_id.as_reference().unwrap_or(0),
        parent: r.parent_id.as_reference().unwrap_or(0),
        reason: r.reason.bits(),
        attrs: r.file_attributes,
        name: r.name.to_string_lossy().into_owned(),
        time: (r.timestamp.unix_timestamp_nanos() / 1_000_000) as i64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_and_recreation_are_desyncs_absence_is_unavailable() {
        assert_eq!(
            JournalError::from(NtfsReaderError::JournalEntryDeleted),
            JournalError::Desynced
        );
        assert_eq!(
            JournalError::from(NtfsReaderError::JournalIdMismatch),
            JournalError::Desynced
        );
        assert!(matches!(
            JournalError::from(NtfsReaderError::JournalNotActive),
            JournalError::Unavailable(_)
        ));
        assert!(matches!(
            JournalError::from(NtfsReaderError::AccessDenied),
            JournalError::Failed(FxError::NotElevated(_))
        ));
    }
}
