use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sled::IVec;

use crate::{filters::path_excluded, Options};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathSnapshot {
    pub path: PathBuf,
    pub mtime: u64,
    pub size: u64,
    pub dev: u64,
    pub ino: u64,
}

#[derive(Default, Debug)]
pub struct DeltaSet {
    pub added: u64,
    pub removed: u64,
    pub modified: u64,
}

fn mtime_secs(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn encode_key(p: &Path) -> Vec<u8> {
    p.to_string_lossy().as_bytes().to_vec()
}

pub fn open_db(path: &Path) -> Result<sled::Db> {
    Ok(sled::open(path)?)
}

#[cfg(unix)]
fn dev_ino(md: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (md.dev(), md.ino())
}

#[cfg(windows)]
fn dev_ino(_md: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

pub fn snapshot_walk_and_update(db: &sled::Db, root: &Path, opt: &Options) -> Result<()> {
    fn walk(db: &sled::Db, dir: &Path, depth: u32, opt: &Options) {
        if opt.max_depth > 0 && depth > opt.max_depth {
            return;
        }
        let rd = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return,
        };
        for ent in rd {
            let Ok(ent) = ent else { continue };
            let p = ent.path();
            if path_excluded(&p, opt) {
                continue;
            }
            let Ok(md) = ent.metadata() else { continue };
            if md.is_dir() {
                walk(db, &p, depth + 1, opt);
                continue;
            }
            if md.is_file() {
                let (dev, ino) = dev_ino(&md);
                let snap = PathSnapshot {
                    path: p.clone(),
                    mtime: mtime_secs(&md),
                    size: md.len(),
                    dev,
                    ino,
                };
                let _ = db.insert(
                    encode_key(&p),
                    IVec::from(serde_json::to_vec(&snap).unwrap()),
                );
            }
        }
    }
    walk(db, root, 0, opt);
    db.flush()?;
    Ok(())
}

pub fn compute_delta(db: &sled::Db, root: &Path, opt: &Options) -> Result<DeltaSet> {
    let mut delta = DeltaSet::default();
    // Mark current paths as seen, and compare with DB
    let mut seen: ahash::AHashSet<Vec<u8>> = ahash::AHashSet::with_capacity(1024);
    fn walk(
        db: &sled::Db,
        seen: &mut ahash::AHashSet<Vec<u8>>,
        dir: &Path,
        depth: u32,
        opt: &Options,
        delta: &mut DeltaSet,
    ) {
        if opt.max_depth > 0 && depth > opt.max_depth {
            return;
        }
        let rd = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return,
        };
        for ent in rd {
            let Ok(ent) = ent else { continue };
            let p = ent.path();
            if path_excluded(&p, opt) {
                continue;
            }
            let Ok(md) = ent.metadata() else { continue };
            if md.is_dir() {
                walk(db, seen, &p, depth + 1, opt, delta);
                continue;
            }
            if md.is_file() {
                let key = encode_key(&p);
                seen.insert(key.clone());
                let cur_m = mtime_secs(&md);
                let cur_s = md.len();
                if let Some(v) = db.get(&key).ok().flatten() {
                    if let Ok(prev) = serde_json::from_slice::<PathSnapshot>(&v) {
                        if prev.mtime != cur_m || prev.size != cur_s {
                            delta.modified += 1;
                        }
                    } else {
                        delta.modified += 1;
                    }
                } else {
                    delta.added += 1;
                }
            }
        }
    }
    walk(db, &mut seen, root, 0, opt, &mut delta);
    // Removed: iterate DB prefix under root and count keys not in seen
    let prefix = root.to_string_lossy().as_bytes().to_vec();
    for kv in db.scan_prefix(prefix) {
        if let Ok((k, _)) = kv {
            if !seen.contains(&k.to_vec()) {
                delta.removed += 1;
            }
        }
    }
    Ok(delta)
}

pub fn snapshot_prune_removed(db: &sled::Db, root: &Path) -> Result<u64> {
    let mut removed = 0u64;
    let prefix = root.to_string_lossy().as_bytes().to_vec();
    let keys: Vec<Vec<u8>> = db
        .scan_prefix(prefix)
        .filter_map(|kv| kv.ok())
        .map(|(k, _)| k.to_vec())
        .collect();
    for k in keys {
        if let Ok(s) = std::str::from_utf8(&k) {
            let p = PathBuf::from(s);
            if !p.exists() {
                let _ = db.remove(&k);
                removed += 1;
            }
        }
    }
    db.flush()?;
    Ok(removed)
}

pub fn watch(
    root: &Path,
    on_event: impl Fn(&str, &Path) + Send + 'static,
) -> notify::Result<notify::RecommendedWatcher> {
    use notify::{Event, EventKind, RecommendedWatcher, Watcher};
    let mut w: RecommendedWatcher = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let kind = match &event.kind {
                    EventKind::Create(_) => "create",
                    EventKind::Modify(_) => "modify",
                    EventKind::Remove(_) => "remove",
                    _ => "event",
                };
                for p in event.paths.iter() {
                    on_event(kind, p.as_path());
                }
            }
        },
        notify::Config::default(),
    )?;
    w.watch(root, notify::RecursiveMode::Recursive)?;
    Ok(w)
}
