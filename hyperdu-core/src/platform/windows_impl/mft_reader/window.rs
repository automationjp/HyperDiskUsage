//! One current raw window and one independently owned pending read. Fixups only
//! touch copied records. Random extension reads never consume the pending slot.
use super::{MftReader, VolumeSource};

pub(super) struct ReadWindow {
    start: u64,
    bytes: Vec<u8>,
    pending: Option<(u64, usize)>,
    pub(super) limit: usize,
}
impl Default for ReadWindow {
    fn default() -> Self {
        Self {
            start: 0,
            bytes: Vec::new(),
            pending: None,
            limit: 1024 * 1024,
        }
    }
}
impl ReadWindow {
    #[cfg(test)]
    pub(super) fn capacity(&self) -> usize {
        self.bytes.capacity()
    }
    pub(super) fn clear(&mut self) {
        self.bytes.clear();
    }
}
impl<S: VolumeSource> MftReader<S> {
    pub(super) fn drain_prefetch(&mut self) {
        self.source.cancel_prefetch();
        self.window.pending = None;
    }

    fn physical_range(&self, logical: u64) -> Option<(u64, u64)> {
        let cluster = self.geometry.cluster_size() as u64;
        let mut start = 0u64;
        for run in &self.runs {
            let end = start.checked_add(run.length.checked_mul(cluster)?)?;
            if logical < end {
                return Some((
                    run.lcn?
                        .checked_mul(cluster)?
                        .checked_add(logical.checked_sub(start)?)?,
                    end - logical,
                ));
            }
            start = end;
        }
        None
    }

    fn queue_after_window(&mut self) {
        self.drain_prefetch();
        let Some(next) = self
            .window
            .start
            .checked_add(self.window.bytes.len() as u64)
        else {
            return;
        };
        let Some((physical, remaining)) = self.physical_range(next) else {
            return;
        };
        let length = remaining.min(self.window.limit.min(1024 * 1024) as u64) as usize;
        if length != 0 && self.source.begin_prefetch(physical, length) {
            self.window.pending = Some((next, length));
        }
    }

    pub(super) fn raw_record(&mut self, number: u64, prefetch: bool) -> Option<Vec<u8>> {
        let size = self.geometry.record_size as usize;
        let mut logical = number.checked_mul(size as u64)?;
        let mut buf = vec![0; size];
        let mut filled = 0;
        while filled < size {
            if let Some(at) = logical.checked_sub(self.window.start) {
                if at < self.window.bytes.len() as u64 {
                    let at = at as usize;
                    let take = (size - filled).min(self.window.bytes.len() - at);
                    buf[filled..filled + take].copy_from_slice(&self.window.bytes[at..at + take]);
                    logical = logical.checked_add(take as u64)?;
                    filled += take;
                    continue;
                }
            }
            let (physical, remaining) = self.physical_range(logical)?;
            let take = (size - filled).min(usize::try_from(remaining).unwrap_or(usize::MAX));
            if prefetch && self.window.limit > 0 {
                let length = remaining.min(self.window.limit.min(1024 * 1024) as u64) as usize;
                let pending_matches = self.window.pending == Some((logical, length));
                if !pending_matches {
                    self.drain_prefetch();
                }
                self.window.clear();
                self.window.bytes.resize(length, 0);
                let read = if pending_matches {
                    self.window.pending = None;
                    self.source.finish_prefetch(&mut self.window.bytes)
                } else {
                    self.source.read_at(physical, &mut self.window.bytes)
                };
                if read {
                    self.window.start = logical;
                    // Issue next extent/window before the current record is
                    // returned to its parser. Only one operation may be pending.
                    self.queue_after_window();
                    continue;
                }
                self.drain_prefetch();
                self.window.clear();
            }
            // Read-ahead failure is retried only for the required segment. A
            // short/failed required read keeps the existing incomplete signal.
            if !self
                .source
                .read_at(physical, &mut buf[filled..filled + take])
            {
                return None;
            }
            logical = logical.checked_add(take as u64)?;
            filled += take;
        }
        Some(buf)
    }
}
