//! Export completed snapshots without copying, sorting or writing on the UI thread.
use std::{
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TryRecvError},
        Arc,
    },
};

use crate::scan::ExportSnapshot;

#[derive(Clone, Copy)]
pub(super) enum Format {
    Json,
    Csv,
}
#[derive(Debug)]
pub(super) enum Outcome {
    Saved(PathBuf),
    Cancelled,
    Failed(String),
    /// The save thread ended without reporting; worded by the UI.
    Crashed,
}
pub(super) struct Task {
    receiver: mpsc::Receiver<Outcome>,
    cancel: Arc<AtomicBool>,
}
impl Task {
    /// Move the immutable snapshot to a worker, including the native save dialog.
    pub(super) fn start(
        snapshot: ExportSnapshot,
        format: Format,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<Self> {
        Self::start_job(wake, move |cancel| {
            let extension = match format {
                Format::Json => "json",
                Format::Csv => "csv",
            };
            let Some(path) = rfd::FileDialog::new()
                .add_filter(extension, &[extension])
                .set_file_name(format!("hyperdu-report.{extension}"))
                .save_file()
            else {
                return Outcome::Cancelled;
            };
            save(snapshot, path, format, &cancel)
        })
    }
    /// No join or blocking receive is performed by the UI or by Drop.
    pub(super) fn poll(&self) -> Option<Outcome> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Outcome::Crashed),
        }
    }
    /// Cooperative cancellation; an active OS call or sort finishes first.
    pub(super) fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    fn start_job(
        wake: Arc<dyn Fn() + Send + Sync>,
        job: impl FnOnce(Arc<AtomicBool>) -> Outcome + Send + 'static,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("hyperdu-gui-export".into())
            .spawn(move || {
                let outcome = job(worker_cancel);
                let _ = sender.send(outcome);
                wake();
            })?;
        Ok(Self { receiver, cancel })
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn save(snapshot: ExportSnapshot, path: PathBuf, format: Format, cancel: &AtomicBool) -> Outcome {
    if cancel.load(Ordering::Relaxed) {
        return Outcome::Cancelled;
    }
    // Both the O(N) copy and O(N log N) sort happen here, on the export worker.
    let rows = snapshot.rows();
    match atomic_write(&path, cancel, |writer| match format {
        Format::Json => hyperdu_core::report::write_json(writer, &rows),
        Format::Csv => hyperdu_core::report::write_csv(writer, &rows),
    }) {
        Ok(true) => Outcome::Saved(path),
        Ok(false) => Outcome::Cancelled,
        Err(error) => Outcome::Failed(error.to_string()),
    }
}

/// Leave the previous destination untouched on write failure or observed cancellation.
/// The last rename is the commit point; cancellation is not rollback after that point.
fn atomic_write(
    path: &Path,
    cancel: &AtomicBool,
    write: impl FnOnce(&mut dyn Write) -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(false);
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        write(&mut writer)?;
        writer.flush()?;
    }
    temporary.as_file().sync_all()?;
    if cancel.load(Ordering::Relaxed) {
        return Ok(false);
    }
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::scan::{Model, Update};

    fn wait(task: &Task) -> Outcome {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = task.poll() {
                return result;
            }
            assert!(Instant::now() < deadline, "export completion timed out");
            std::thread::yield_now();
        }
    }

    #[test]
    fn ui_poll_never_waits_for_a_blocked_worker() {
        let (ready, started) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let ui_thread = std::thread::current().id();
        let task = Task::start_job(Arc::new(|| {}), move |_| {
            assert_ne!(std::thread::current().id(), ui_thread);
            ready.send(()).unwrap();
            gate.recv_timeout(Duration::from_secs(5)).unwrap();
            Outcome::Cancelled
        })
        .unwrap();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        for _ in 0..100 {
            assert!(task.poll().is_none());
        }
        release.send(()).unwrap();
        assert!(matches!(wait(&task), Outcome::Cancelled));
    }

    #[test]
    fn failed_or_cancelled_write_preserves_destination_and_removes_tempfile() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.json");
        fs::write(&path, b"original").unwrap();
        let cancel = AtomicBool::new(false);
        let result = atomic_write(&path, &cancel, |writer| {
            writer.write_all(b"partial")?;
            anyhow::bail!("simulated disk failure")
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert!(!atomic_write(&path, &cancel, |writer| {
            writer.write_all(b"cancelled")?;
            cancel.store(true, Ordering::Relaxed);
            Ok(())
        })
        .unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn completed_snapshot_is_independent_and_sorted_in_both_formats() {
        let mut model = Model::new(PathBuf::from("root"));
        model.apply(Update::Nodes(vec![
            (
                0,
                PathBuf::from("root/z"),
                hyperdu_core::Stat {
                    files: 1,
                    logical: 7,
                    physical: 8,
                },
                false,
            ),
            (
                1,
                PathBuf::from("root/a"),
                hyperdu_core::Stat::default(),
                false,
            ),
        ]));
        let json_snapshot = model.export_snapshot();
        let csv_snapshot = model.export_snapshot();
        model.apply(Update::Reset);
        let directory = tempfile::tempdir().unwrap();
        let json = directory.path().join("report.json");
        let csv = directory.path().join("report.csv");
        assert!(matches!(
            save(
                json_snapshot,
                json.clone(),
                Format::Json,
                &AtomicBool::new(false)
            ),
            Outcome::Saved(_)
        ));
        assert!(matches!(
            save(
                csv_snapshot,
                csv.clone(),
                Format::Csv,
                &AtomicBool::new(false)
            ),
            Outcome::Saved(_)
        ));
        let json = fs::read_to_string(json).unwrap();
        assert!(json.find("root/a").unwrap() < json.find("root/z").unwrap());
        assert!(json.contains("\"logical\": 7"));
        let csv = fs::read_to_string(csv).unwrap();
        assert!(csv.starts_with("path,logical,physical,files"));
        assert!(csv.find("root/a").unwrap() < csv.find("root/z").unwrap());
    }

    #[test]
    fn worker_failure_and_drop_do_not_hang_or_publish_a_false_success() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let task = Task {
            receiver,
            cancel: Arc::clone(&cancel),
        };
        drop(sender);
        assert!(matches!(task.poll(), Some(Outcome::Crashed)));
        drop(task);
        assert!(cancel.load(Ordering::Relaxed));
    }
}
