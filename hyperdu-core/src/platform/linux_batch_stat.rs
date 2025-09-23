// Batch stat operations for Linux
use std::sync::atomic::{AtomicU64, Ordering};
use std::ffi::CString;

const BATCH_SIZE: usize = 32;

pub struct BatchStatCollector {
    fd: i32,
    batch: Vec<(Vec<u8>, u8, bool)>, // (name, d_type, is_dir)
    results: Vec<Option<(u64, u64)>>, // (logical, physical) sizes
}

impl BatchStatCollector {
    pub fn new(fd: i32) -> Self {
        Self {
            fd,
            batch: Vec::with_capacity(BATCH_SIZE),
            results: Vec::with_capacity(BATCH_SIZE),
        }
    }

    pub fn add(&mut self, name: Vec<u8>, d_type: u8, is_dir: bool) {
        self.batch.push((name, d_type, is_dir));
    }

    pub fn is_full(&self) -> bool {
        self.batch.len() >= BATCH_SIZE
    }

    // Process batch using parallel threads or io_uring if available
    pub fn process(&mut self, follow_links: bool, compute_physical: bool) {
        self.results.clear();

        // Try io_uring first if available
        #[cfg(feature = "uring")]
        if self.process_iouring(follow_links, compute_physical) {
            return;
        }

        // Fallback to threaded approach
        self.process_threaded(follow_links, compute_physical);
    }

    fn process_threaded(&mut self, follow_links: bool, compute_physical: bool) {
        use std::thread;
        use std::sync::Arc;
        use crossbeam_channel::{bounded, Sender};

        let (tx, rx) = bounded(BATCH_SIZE);
        let batch = Arc::new(self.batch.clone());
        let fd = self.fd;

        // Spawn worker threads for parallel statx
        let handles: Vec<_> = (0..4.min(self.batch.len())).map(|i| {
            let batch = batch.clone();
            let tx = tx.clone();
            thread::spawn(move || {
                for j in (i..batch.len()).step_by(4) {
                    let (name, _dtype, _is_dir) = &batch[j];
                    let result = stat_single(fd, name, follow_links, compute_physical);
                    tx.send((j, result)).ok();
                }
            })
        }).collect();

        drop(tx);

        // Collect results
        self.results.resize(self.batch.len(), None);
        while let Ok((idx, result)) = rx.recv() {
            self.results[idx] = result;
        }

        for h in handles {
            h.join().ok();
        }
    }

    #[cfg(feature = "uring")]
    fn process_iouring(&mut self, follow_links: bool, compute_physical: bool) -> bool {
        use io_uring::{opcode, types, IoUring};

        let ring = match IoUring::new(64) {
            Ok(r) => r,
            Err(_) => return false,
        };

        // Submit batch statx operations
        let mut statx_bufs = Vec::with_capacity(self.batch.len());
        for (name, _, _) in &self.batch {
            statx_bufs.push(Box::pin(unsafe { std::mem::zeroed::<libc::statx>() }));
        }

        {
            let mut sq = ring.submission();
            for (i, ((name, _, _), statx_buf)) in self.batch.iter().zip(statx_bufs.iter()).enumerate() {
                if let Ok(c_name) = CString::new(name.as_slice()) {
                    let flags = if follow_links { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
                    let statx_e = opcode::Statx::new(
                        types::Fd(self.fd),
                        c_name.as_ptr(),
                        flags,
                        libc::STATX_SIZE | libc::STATX_BLOCKS,
                        statx_buf.as_ref().get_ref() as *const _ as *mut _,
                    )
                    .build()
                    .user_data(i as u64);

                    unsafe { sq.push(&statx_e).ok(); }
                }
            }
        }

        // Wait for completions
        if ring.submit_and_wait(self.batch.len()).is_ok() {
            let cq = ring.completion();
            for cqe in cq {
                let idx = cqe.user_data() as usize;
                if cqe.result() >= 0 && idx < statx_bufs.len() {
                    let stx = &*statx_bufs[idx];
                    let logical = stx.stx_size as u64;
                    let physical = if compute_physical {
                        let raw = (stx.stx_blocks as u64) * 512;
                        if raw == 0 { logical } else { raw }
                    } else {
                        logical
                    };
                    self.results.push(Some((logical, physical)));
                }
            }
            true
        } else {
            false
        }
    }

    pub fn take_results(&mut self) -> Vec<Option<(u64, u64)>> {
        std::mem::take(&mut self.results)
    }

    pub fn clear(&mut self) {
        self.batch.clear();
        self.results.clear();
    }
}

fn stat_single(fd: i32, name: &[u8], follow_links: bool, compute_physical: bool) -> Option<(u64, u64)> {
    let c_name = CString::new(name).ok()?;
    let flags = if follow_links { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
    let mut stx: libc::statx = unsafe { std::mem::zeroed() };

    let rc = unsafe {
        libc::statx(
            fd,
            c_name.as_ptr(),
            flags,
            libc::STATX_SIZE | libc::STATX_BLOCKS,
            &mut stx,
        )
    };

    if rc == 0 {
        let logical = stx.stx_size as u64;
        let physical = if compute_physical {
            let raw = (stx.stx_blocks as u64) * 512;
            if raw == 0 { logical } else { raw }
        } else {
            logical
        };
        Some((logical, physical))
    } else {
        None
    }
}