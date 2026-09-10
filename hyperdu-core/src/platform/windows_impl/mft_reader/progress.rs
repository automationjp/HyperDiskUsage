//! Bounded record iteration control, independent of platform UI options.
use super::{Entry, MftReader, VolumeSource};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReadProgress {
    /// Records examined, including inactive slots; not a percentage.
    pub(crate) records: u64,
    /// Valid file entries seen, before hardlink/parent accounting.
    pub(crate) files: u64,
    /// The walk reached its end with no required record failures.
    pub(crate) finished: bool,
}

impl<S: VolumeSource> MftReader<S> {
    /// Report before walking, at most every 256 records, and at successful end.
    /// Returning false aborts and drops the partial result. Extension I/O stays
    /// synchronous inside its base record; cancellation is checked between batches.
    pub(crate) fn entries_with_control(
        &mut self,
        mut keep_going: impl FnMut(ReadProgress) -> bool,
    ) -> Option<Vec<Entry>> {
        const FIRST_USER_RECORD: u64 = 16;
        let mut progress = ReadProgress::default();
        if !keep_going(progress) {
            self.complete = false;
            return None;
        }
        let mut out = Vec::new();
        for number in FIRST_USER_RECORD..self.record_count() {
            if let Some(entry) = self.entry(number) {
                progress.files += u64::from(!entry.is_directory);
                out.push(entry);
            }
            progress.records += 1;
            if progress.records % 256 == 0 && !keep_going(progress) {
                self.complete = false;
                return None;
            }
        }
        if self.complete {
            progress.finished = true;
            if !keep_going(progress) {
                self.complete = false;
                return None;
            }
        }
        Some(out)
    }
}
