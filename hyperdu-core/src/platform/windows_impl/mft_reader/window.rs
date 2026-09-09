//! A bounded raw-byte window. Fixups always operate on a copied record, so a
//! second read (including an extension lookup) never observes patched tails.
use super::{MftReader, VolumeSource};

pub(super) struct ReadWindow {
    start: u64,
    bytes: Vec<u8>,
    pub(super) limit: usize,
}

impl Default for ReadWindow {
    fn default() -> Self {
        Self {
            start: 0,
            bytes: Vec::new(),
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
    pub(super) fn raw_record(&mut self, number: u64, prefetch: bool) -> Option<Vec<u8>> {
        let size = self.geometry.record_size as usize;
        let mut logical = number.checked_mul(size as u64)?;
        let cluster = self.geometry.cluster_size() as u64;
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
            let mut run_start = 0u64;
            let mut mapped = None;
            for run in &self.runs {
                let length = run.length.checked_mul(cluster)?;
                let end = run_start.checked_add(length)?;
                if logical < end {
                    let within = logical.checked_sub(run_start)?;
                    let physical = run.lcn?.checked_mul(cluster)?.checked_add(within)?;
                    mapped = Some((physical, end - logical));
                    break;
                }
                run_start = end;
            }
            let (physical, remaining) = mapped?;
            let take = (size - filled).min(usize::try_from(remaining).unwrap_or(usize::MAX));
            // Random extension misses bypass the sequential window. The range
            // is capped at this physical extent, even when a record straddles it.
            if prefetch && self.window.limit > 0 {
                let length = remaining.min(self.window.limit as u64) as usize;
                self.window.clear();
                self.window.bytes.resize(length, 0);
                if self.source.read_at(physical, &mut self.window.bytes) {
                    self.window.start = logical;
                    continue;
                }
                // Read-ahead may reach an unreadable later record. Retry only
                // the required segment and never expose a partially filled cache.
                self.window.clear();
            }
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
