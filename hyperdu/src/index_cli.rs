use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use hyperdu_core::index::{
    journal::JournalKind,
    v2::{observe, EntryKind, Freshness, IndexWatcher, PersistentIndex, WatchUpdate},
};

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Start the MCP server over stdio. Use ./mcp to scan a directory named mcp.
    Mcp,
    /// Save, query or monitor a directory index on one filesystem
    #[command(subcommand)]
    Index(IndexCommand),
}

#[derive(Debug, Subcommand)]
pub(crate) enum IndexCommand {
    /// Scan ROOT and atomically replace a version 2 snapshot
    Refresh {
        root: PathBuf,
        /// Snapshot outside ROOT; its parent directory must already exist
        #[arg(long, value_name = "FILE")]
        database: PathBuf,
    },
    /// Read saved totals without walking ROOT; saved data is always stale
    Show {
        root: PathBuf,
        #[arg(long, value_name = "FILE")]
        database: PathBuf,
    },
    /// Replay native changes in the foreground; Ctrl-C stops monitoring
    Watch {
        root: PathBuf,
        #[arg(long, value_name = "FILE")]
        database: PathBuf,
        /// Poll native changes at this interval
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u64).range(10..=60000))]
        poll_ms: u64,
        /// Reconcile advisory notifications with a full scan at this interval
        #[arg(long, default_value_t = 900, value_parser = clap::value_parser!(u64).range(1..=86400))]
        reconcile_seconds: u64,
        /// Persist a complete checkpoint at most this often after initial catch-up
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=3600))]
        checkpoint_seconds: u64,
        /// Catch up once and save, then exit (30-second catch-up limit)
        #[arg(long)]
        once: bool,
    },
}

pub(crate) fn run(command: IndexCommand) -> Result<()> {
    let (requested_root, requested_database) = match &command {
        IndexCommand::Refresh { root, database }
        | IndexCommand::Show { root, database }
        | IndexCommand::Watch { root, database, .. } => (root, database),
    };
    let root = requested_root
        .canonicalize()
        .context("resolve index root")?;
    let root_entry = observe(&root).context("observe index root")?;
    if root_entry.kind != EntryKind::Directory {
        bail!("index root must be a directory");
    }
    let database = database_path(&root, requested_database)?;
    if matches!(&command, IndexCommand::Show { .. }) {
        let index = PersistentIndex::load(&database)
            .context("read version 2 snapshot; use index refresh to migrate or rebuild")?;
        if index.root() != root_entry.id {
            bail!("database root identity mismatch; refresh explicitly");
        }
        return output(
            &root,
            &database,
            &index,
            false,
            false,
            WatchUpdate::default(),
        );
    }
    // Advisory Unix lock / exclusive Windows open lives through every save.
    // The stable lock file is retained: unlinking it would split writer locks.
    let _writer = lock_database(&database).context("acquire database writer lock")?;
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    match command {
        IndexCommand::Refresh { .. } => {
            let index = PersistentIndex::scan_snapshot(&root, &cancel)?;
            if index.root() != root_entry.id || cancel.load(Ordering::Relaxed) {
                bail!("scan interrupted or root replaced; previous checkpoint retained");
            }
            index.save(&database).context("save index snapshot")?;
            output(
                &root,
                &database,
                &index,
                false,
                true,
                WatchUpdate::default(),
            )
        }
        IndexCommand::Watch {
            poll_ms,
            reconcile_seconds,
            checkpoint_seconds,
            once,
            ..
        } => {
            let saved = match PersistentIndex::load(&database) {
                Ok(index) => Some(index),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(error).context("read checkpoint; use index refresh to rebuild")
                }
            };
            let mut watcher = IndexWatcher::start(&root, saved, &cancel)?;
            watcher.set_reconcile_interval(Duration::from_secs(reconcile_seconds))?;
            let started = Instant::now();
            let mut last_checkpoint = started;
            let mut first = true;
            let mut dirty = false;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    if watcher
                        .index()
                        .total(watcher.index().root())
                        .is_some_and(|(_, state)| state == Freshness::Observed)
                    {
                        watcher
                            .index()
                            .save(&database)
                            .context("save final checkpoint")?;
                    }
                    return Ok(());
                }
                let update = match watcher.poll(&cancel) {
                    Ok(update) => update,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(()),
                    Err(error) => {
                        return Err(error).context("monitor index; previous checkpoint retained")
                    }
                };
                let changed = update.notifications > 0
                    || update.rebuilt
                    || update.stats.observed_entries > 0
                    || update.stats.directories_read > 0;
                dirty |= changed;
                if update.caught_up {
                    let checkpoint = first
                        || once
                        || (dirty
                            && last_checkpoint.elapsed()
                                >= Duration::from_secs(checkpoint_seconds));
                    if checkpoint {
                        watcher
                            .index()
                            .save(&database)
                            .context("save journal checkpoint")?;
                        dirty = false;
                        last_checkpoint = Instant::now();
                    }
                    if first || changed || checkpoint {
                        output(&root, &database, watcher.index(), !once, checkpoint, update)?;
                    }
                    first = false;
                    if once {
                        return Ok(());
                    }
                } else if once && started.elapsed() >= Duration::from_secs(30) {
                    bail!(
                        "journal did not catch up within 30 seconds; previous checkpoint retained"
                    );
                }
                thread::sleep(Duration::from_millis(poll_ms));
            }
        }
        IndexCommand::Show { .. } => unreachable!(),
    }
}

fn database_path(root: &Path, requested: &Path) -> Result<PathBuf> {
    let parent = requested
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .context("resolve database parent (create it first)")?;
    let database = parent.join(requested.file_name().context("database must name a file")?);
    if database.starts_with(root) {
        bail!("database must be outside the indexed root");
    }
    match fs::symlink_metadata(&database) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            bail!("database must be a regular file, not a symlink");
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    Ok(database)
}

fn lock_database(database: &Path) -> Result<File> {
    let mut name = database
        .file_name()
        .context("database must name a file")?
        .to_os_string();
    name.push(".lock");
    let lock_path = database.with_file_name(name);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0).custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(lock_path)?;
    if !file.metadata()?.file_type().is_file() {
        bail!("writer lock must be a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: a live, ordinary file descriptor; nonblocking lock releases
        // automatically on close, including process termination.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error()).context("database already has a writer");
        }
    }
    #[cfg(not(any(unix, windows)))]
    bail!("database writer locking is unavailable on this platform");
    Ok(file)
}

fn output(
    root: &Path,
    database: &Path,
    index: &PersistentIndex,
    monitoring: bool,
    checkpoint_written: bool,
    update: WatchUpdate,
) -> Result<()> {
    let (totals, freshness) = index.total(index.root()).context("index root missing")?;
    let journal = index.cursor().map(|cursor| serde_json::json!({
        "kind": match cursor.kind { JournalKind::Usn => "usn", JournalKind::Fsevents => "fsevents",
            JournalKind::Fanotify => "fanotify", JournalKind::Inotify => "inotify" },
        "epoch": cursor.epoch.to_string(), "position": cursor.position,
    }));
    let record = serde_json::json!({
        "format": 2, "root": root, "database": database,
        "device": index.root().volume, "object_id": index.root().object.to_string(),
        "logical_bytes": totals.logical, "physical_bytes": totals.physical, "files": totals.files,
        "directory_count": index.directory_count(),
        "freshness": match freshness { Freshness::Unknown => "unknown",
            Freshness::Stale => "stale", Freshness::Observed => "observed" },
        "monitoring": monitoring, "journal": journal, "checkpoint_written": checkpoint_written,
        "rebuilt": update.rebuilt, "notifications": update.notifications, "observed_entries": update.stats.observed_entries,
        "directories_read": update.stats.directories_read,
    });
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &record)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_lock_is_exclusive_and_released_without_deleting_the_lock_file() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("index");
        let first = lock_database(&database).unwrap();
        assert!(lock_database(&database).is_err());
        drop(first);
        assert!(lock_database(&database).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn writer_lock_refuses_symlink_without_following() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("index");
        let target = temp.path().join("target");
        fs::write(&target, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&target, temp.path().join("index.lock")).unwrap();
        assert!(lock_database(&database).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"unchanged");
    }
}
