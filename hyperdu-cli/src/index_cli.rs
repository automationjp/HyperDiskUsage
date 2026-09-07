use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Explicit Linux directory snapshots (no background monitoring)
    #[command(subcommand)]
    Index(IndexCommand),
}

#[derive(Debug, Subcommand)]
pub(crate) enum IndexCommand {
    /// Perform an exact scan and atomically replace a saved snapshot
    Refresh {
        root: PathBuf,
        /// Snapshot file outside ROOT; its parent directory must already exist
        #[arg(long, value_name = "FILE")]
        database: PathBuf,
    },
    /// Read saved totals without walking ROOT; freshness is always stale
    Show {
        root: PathBuf,
        #[arg(long, value_name = "FILE")]
        database: PathBuf,
    },
}

pub(crate) fn run(command: Command) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::run(command)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = command;
        anyhow::bail!("index commands are currently supported on Linux only")
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        fs,
        os::unix::fs::MetadataExt,
        path::Path,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };

    use anyhow::{bail, Context, Result};
    use hyperdu_core::index::Index;

    use super::{Command, IndexCommand};

    pub(super) fn run(command: Command) -> Result<()> {
        let Command::Index(command) = command;
        let (root, database, refresh) = match command {
            IndexCommand::Refresh { root, database } => (root, database, true),
            IndexCommand::Show { root, database } => (root, database, false),
        };
        let root = root.canonicalize().context("resolve snapshot root")?;
        let meta = fs::metadata(&root)?;
        if !meta.is_dir() {
            bail!("snapshot root must be a directory");
        }
        let key = (meta.dev(), meta.ino());
        let parent = database
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()
            .context("resolve database parent (create it first)")?;
        let database = parent.join(database.file_name().context("database must name a file")?);
        if database.starts_with(&root) {
            bail!("database must be outside the scanned root");
        }
        if let Ok(metadata) = fs::symlink_metadata(&database) {
            if !metadata.file_type().is_file() {
                bail!("database must be a regular file, not a symlink");
            }
        }
        let index = if refresh {
            let cancel = Arc::new(AtomicBool::new(false));
            let signal = cancel.clone();
            ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
            let (scanned_key, index) = Index::scan_snapshot(&root, cancel.clone())?;
            if scanned_key != key || cancel.load(Ordering::Relaxed) {
                bail!("scan interrupted or root replaced; old snapshot preserved");
            }
            index.save(&database).context("save snapshot")?;
            index
        } else {
            Index::load(&database).context("read snapshot; run index refresh to rebuild")?
        };
        // A subtree of a different root is not the requested database identity.
        if index.parent_of(key).is_some() {
            bail!("database belongs to a different root");
        }
        let (bytes, files, _) = index
            .subtree_total(key)
            .context("database root identity mismatch; rebuild explicitly")?;
        let output = serde_json::json!({
            "root": root, "database": database, "device": key.0, "inode": key.1,
            "physical_bytes": bytes, "files": files, "directory_count": index.len(),
            "freshness": "stale", "monitoring": false,
        });
        println!("{}", serde_json::to_string(&output)?);
        Ok(())
    }
}
