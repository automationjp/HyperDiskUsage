//! Bounded asynchronous statx with ownership lasting through every original CQE.
//!
//! A submit error does not prove that the kernel ignored the SQEs. If completion
//! cannot be established, keep the ring, descriptor, names and outputs alive and
//! disable this backend for the process. This rare bounded retention is preferable
//! to freeing memory the kernel may still reference.

use std::{
    cell::UnsafeCell,
    ffi::CStr,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::atomic::{AtomicBool, Ordering},
};

use io_uring::{opcode, types, IoUring, Probe};

pub(super) const BATCH_SIZE: usize = 64;
const NAME_BYTES: usize = 256;
static BROKEN: AtomicBool = AtomicBool::new(false);

struct Completions {
    expected: usize,
    received: u64,
    results: [i32; BATCH_SIZE],
}

impl Default for Completions {
    fn default() -> Self {
        Self {
            expected: 0,
            received: 0,
            results: [0; BATCH_SIZE],
        }
    }
}

impl Completions {
    fn remaining(&self) -> usize {
        self.expected - self.received.count_ones() as usize
    }

    fn observe(&mut self, id: u64, result: i32) -> io::Result<()> {
        if id >= self.expected as u64 {
            return Err(protocol_error("unknown statx completion"));
        }
        let bit = 1u64 << id;
        if self.received & bit != 0 {
            return Err(protocol_error("duplicate statx completion"));
        }
        self.results[id as usize] = result;
        self.received |= bit;
        Ok(())
    }
}

/// Retains all referenced resources on unwind and on unrecoverable queue errors.
/// The generic owner also lets fault tests observe actual resource destruction.
struct InFlight<'a, T> {
    resources: Option<T>,
    completions: Completions,
    poisoned: &'a AtomicBool,
}

impl<T> Drop for InFlight<'_, T> {
    fn drop(&mut self) {
        if self.completions.remaining() != 0 {
            self.poisoned.store(true, Ordering::Release);
            if let Some(resources) = self.resources.take() {
                std::mem::forget(resources);
            }
        }
    }
}

struct Resources {
    ring: IoUring,
    directory: OwnedFd,
    names: Box<[[u8; NAME_BYTES]; BATCH_SIZE]>,
    outputs: Box<[UnsafeCell<libc::statx>]>,
}

pub(super) struct StatxBatcher {
    flight: InFlight<'static, Resources>,
}

impl StatxBatcher {
    /// None means the kernel/security policy cannot provide the statx opcode.
    pub(super) fn new(directory: RawFd) -> io::Result<Option<Self>> {
        if BROKEN.load(Ordering::Acquire) {
            return Ok(None);
        }
        let ring = match IoUring::new(BATCH_SIZE as u32) {
            Ok(ring) => ring,
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENOSYS | libc::EPERM | libc::EACCES | libc::EINVAL)
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let mut probe = Probe::new();
        match ring.submitter().register_probe(&mut probe) {
            Ok(()) if probe.is_supported(opcode::Statx::CODE) => {}
            Ok(()) => return Ok(None),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::EINVAL | libc::ENOSYS | libc::EPERM | libc::EACCES)
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
        // SAFETY: fcntl creates a distinct descriptor; it does not take ownership
        // of the caller's descriptor or change the shared directory offset.
        let duplicate = unsafe { libc::fcntl(directory, libc::F_DUPFD_CLOEXEC, 0) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: this descriptor was just returned by fcntl and has one owner.
        let directory = unsafe { OwnedFd::from_raw_fd(duplicate) };
        let outputs = (0..BATCH_SIZE)
            .map(|_| {
                // SAFETY: statx is an integer-only C output structure.
                UnsafeCell::new(unsafe { std::mem::zeroed() })
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(Some(Self {
            flight: InFlight {
                resources: Some(Resources {
                    ring,
                    directory,
                    names: Box::new([[0; NAME_BYTES]; BATCH_SIZE]),
                    outputs,
                }),
                completions: Completions::default(),
                poisoned: &BROKEN,
            },
        }))
    }

    /// Results retain input order. Inspect masks or perform synchronous fallback
    /// only after this returns: every original request has then completed.
    pub(super) fn query(
        &mut self,
        names: &[&CStr],
        flags: i32,
        mask: u32,
        cancel: &AtomicBool,
    ) -> io::Result<Vec<io::Result<libc::statx>>> {
        if self.flight.completions.remaining() != 0 {
            return Err(protocol_error("previous statx batch is still active"));
        }
        if names.len() > BATCH_SIZE
            || names
                .iter()
                .any(|name| name.to_bytes_with_nul().len() > NAME_BYTES)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "statx batch/name too large",
            ));
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let resources = self
            .flight
            .resources
            .as_mut()
            .ok_or_else(|| protocol_error("statx resources unavailable"))?;
        for (index, name) in names.iter().enumerate() {
            let bytes = name.to_bytes_with_nul();
            resources.names[index][..bytes.len()].copy_from_slice(bytes);
            // SAFETY: no request from the previous batch remains; no output is
            // borrowed while this prepares a new batch.
            unsafe { *resources.outputs[index].get() = std::mem::zeroed() };
        }
        self.flight.completions = Completions {
            expected: names.len(),
            ..Completions::default()
        };
        {
            let mut queue = resources.ring.submission();
            for index in 0..names.len() {
                let entry = opcode::Statx::new(
                    types::Fd(resources.directory.as_raw_fd()),
                    resources.names[index].as_ptr().cast(),
                    resources.outputs[index].get().cast(),
                )
                .flags(flags)
                .mask(mask)
                .build()
                .user_data(index as u64);
                // SAFETY: all pointer targets and the duplicated fd belong to
                // InFlight. They are not changed/released until all original
                // CQEs arrive, even on submit failure, cancellation or unwind.
                unsafe { queue.push(&entry) }
                    .map_err(|_| protocol_error("statx submission queue unexpectedly full"))?;
            }
        }
        drain(&mut resources.ring, &mut self.flight.completions)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let results = (0..names.len())
            .map(|index| match self.flight.completions.results[index] {
                0 => {
                    // SAFETY: drain received this original CQE; kernel output
                    // writes are finished and the queue provided acquire ordering.
                    Ok(unsafe { *resources.outputs[index].get() })
                }
                result if result < 0 => Err(io::Error::from_raw_os_error(
                    result.checked_neg().unwrap_or(libc::EIO),
                )),
                _ => Err(protocol_error("invalid statx completion result")),
            })
            .collect();
        Ok(results)
    }
}

trait CompletionSource {
    fn reap(&mut self, completions: &mut Completions) -> io::Result<()>;
    fn submit_wait(&mut self) -> io::Result<usize>;
}

impl CompletionSource for IoUring {
    fn reap(&mut self, completions: &mut Completions) -> io::Result<()> {
        let mut queue = self.completion();
        if queue.overflow() != 0 {
            return Err(protocol_error("statx completion queue overflow"));
        }
        for entry in &mut queue {
            completions.observe(entry.user_data(), entry.result())?;
        }
        Ok(())
    }

    fn submit_wait(&mut self) -> io::Result<usize> {
        self.submit_and_wait(1)
    }
}

fn drain(source: &mut impl CompletionSource, completions: &mut Completions) -> io::Result<()> {
    while completions.remaining() != 0 {
        source.reap(completions)?;
        if completions.remaining() == 0 {
            break;
        }
        // SQEs may be only partly submitted. Submitter reads the shared SQ head
        // and resubmits the still-pending entries; returned submission counts do
        // not retire ownership. Only matching original CQEs do that.
        match source.submit_wait() {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn protocol_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        ffi::CString,
        fs::File,
        os::unix::fs::{symlink, MetadataExt},
        sync::{atomic::AtomicUsize, Arc},
    };

    use super::*;

    struct FakeQueue {
        rounds: VecDeque<(Vec<(u64, i32)>, io::Result<usize>)>,
        ready: Vec<(u64, i32)>,
    }

    impl CompletionSource for FakeQueue {
        fn reap(&mut self, completions: &mut Completions) -> io::Result<()> {
            for (id, result) in self.ready.drain(..) {
                completions.observe(id, result)?;
            }
            Ok(())
        }

        fn submit_wait(&mut self) -> io::Result<usize> {
            let (ready, submitted) = self.rounds.pop_front().expect("unexpected extra wait");
            self.ready = ready;
            submitted
        }
    }

    struct DropWitness(Arc<AtomicUsize>);
    impl Drop for DropWitness {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn partial_submission_interruption_and_out_of_order_completion_retain_input_order() {
        let mut queue = FakeQueue {
            ready: Vec::new(),
            rounds: VecDeque::from([
                (vec![(2, -libc::ENOENT)], Ok(1)),
                (
                    vec![(0, 0)],
                    Err(io::Error::from(io::ErrorKind::Interrupted)),
                ),
                (vec![(1, 0)], Ok(1)),
            ]),
        };
        let poisoned = AtomicBool::new(false);
        let drops = Arc::new(AtomicUsize::new(0));
        let mut flight = InFlight {
            resources: Some(DropWitness(drops.clone())),
            completions: Completions {
                expected: 3,
                ..Completions::default()
            },
            poisoned: &poisoned,
        };
        drain(&mut queue, &mut flight.completions).unwrap();
        assert_eq!(flight.completions.remaining(), 0);
        assert_eq!(&flight.completions.results[..3], &[0, 0, -libc::ENOENT]);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(flight);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(!poisoned.load(Ordering::Acquire));
    }

    #[test]
    fn unrecoverable_submit_error_never_frees_kernel_referenced_resources() {
        let mut queue = FakeQueue {
            ready: Vec::new(),
            rounds: VecDeque::from([(
                vec![(0, 0)],
                Err(io::Error::from_raw_os_error(libc::EBADF)),
            )]),
        };
        let poisoned = AtomicBool::new(false);
        let drops = Arc::new(AtomicUsize::new(0));
        let mut flight = InFlight {
            resources: Some(DropWitness(drops.clone())),
            completions: Completions {
                expected: 2,
                ..Completions::default()
            },
            poisoned: &poisoned,
        };
        assert!(drain(&mut queue, &mut flight.completions).is_err());
        drop(flight);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert!(poisoned.load(Ordering::Acquire));
    }

    #[test]
    fn invalid_or_duplicate_completion_cannot_retire_another_request() {
        let mut completions = Completions {
            expected: 2,
            ..Completions::default()
        };
        assert!(completions.observe(2, 0).is_err());
        assert_eq!(completions.remaining(), 2);
        completions.observe(1, 0).unwrap();
        assert!(completions.observe(1, 0).is_err());
        assert_eq!(completions.remaining(), 1);
    }

    #[test]
    fn unwind_retains_active_resources_but_drops_an_idle_owner() {
        for expected in [0, 1] {
            let poisoned = AtomicBool::new(false);
            let drops = Arc::new(AtomicUsize::new(0));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _flight = InFlight {
                    resources: Some(DropWitness(drops.clone())),
                    completions: Completions {
                        expected,
                        ..Completions::default()
                    },
                    poisoned: &poisoned,
                };
                panic!("injected unwind");
            }));
            assert!(result.is_err());
            assert_eq!(drops.load(Ordering::Relaxed), usize::from(expected == 0));
            assert_eq!(poisoned.load(Ordering::Acquire), expected != 0);
        }
    }

    #[test]
    fn native_statx_batch_matches_sparse_identity_link_policy_and_errno() {
        let root = tempfile::tempdir().unwrap();
        let sparse = root.path().join("sparse");
        File::create(&sparse)
            .unwrap()
            .set_len(8 * 1024 * 1024)
            .unwrap();
        std::fs::hard_link(&sparse, root.path().join("hardlink")).unwrap();
        symlink("sparse", root.path().join("link")).unwrap();
        let dir = File::open(root.path()).unwrap();
        let Some(mut batcher) = StatxBatcher::new(dir.as_raw_fd()).unwrap() else {
            assert!(
                std::env::var_os("HYPERDU_TEST_REQUIRE_URING").is_none(),
                "native io_uring statx required by fixture runner"
            );
            eprintln!("io_uring unavailable: native batch execution not verified");
            return;
        };
        let names: Vec<_> = ["sparse", "hardlink", "link", "absent"]
            .into_iter()
            .map(|name| CString::new(name).unwrap())
            .collect();
        let borrowed: Vec<_> = names.iter().map(|name| name.as_c_str()).collect();
        let cancel = AtomicBool::new(false);
        for follow in [false, true] {
            let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
            let results = batcher
                .query(&borrowed, flags, libc::STATX_BASIC_STATS, &cancel)
                .unwrap();
            for index in 0..3 {
                let expected = if follow {
                    std::fs::metadata(root.path().join(names[index].to_str().unwrap()))
                } else {
                    std::fs::symlink_metadata(root.path().join(names[index].to_str().unwrap()))
                }
                .unwrap();
                let actual = results[index].as_ref().unwrap();
                assert_eq!(actual.stx_size, expected.len());
                assert_eq!(actual.stx_blocks, expected.blocks());
                assert_eq!(actual.stx_ino, expected.ino());
                assert_eq!(actual.stx_nlink as u64, expected.nlink());
                assert_eq!(actual.stx_mode as u32, expected.mode());
            }
            assert_eq!(
                results[3].as_ref().unwrap_err().raw_os_error(),
                Some(libc::ENOENT)
            );
        }
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            batcher
                .query(&borrowed, 0, libc::STATX_BASIC_STATS, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(batcher.flight.completions.remaining(), 0);
    }
}
