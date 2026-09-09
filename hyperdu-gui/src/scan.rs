//! GUI transport and directory index. All filesystem policy lives in hyperdu-core.
use hyperdu_core::{self as core, ScanEvent, ScanMode, Stat, StatMap};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc,
    },
};

pub enum Msg {
    Core(ScanEvent),
    Failed(String),
}
pub struct Handle {
    pub rx: mpsc::Receiver<Msg>,
    pub files_seen: Arc<AtomicU64>,
    pub errors: Arc<AtomicU64>,
    pub error_details: Arc<std::sync::Mutex<Vec<String>>>,
    cancel: Arc<AtomicBool>,
}
impl Handle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Clone)]
pub struct Params {
    pub mode: ScanMode,
    pub exclude: String,
    pub glob: String,
    pub regex: String,
    pub min_size: String,
    pub max_depth: u32,
    pub follow_links: bool,
    pub count_hardlinks: bool,
    pub one_file_system: bool,
    pub threads: usize,
    pub size_mode: u8,
    pub io_profile: core::IoProfile,
    pub prefetch: u8,
    pub dir_yield: usize,
    pub use_mft: bool,
}
impl Default for Params {
    fn default() -> Self {
        Self {
            mode: ScanMode::Interactive,
            exclude: String::new(),
            glob: String::new(),
            regex: String::new(),
            min_size: "0".into(),
            max_depth: 0,
            follow_links: false,
            count_hardlinks: false,
            one_file_system: false,
            threads: 0,
            size_mode: 0,
            io_profile: core::IoProfile::Balanced,
            prefetch: 0,
            dir_yield: 0,
            use_mft: false,
        }
    }
}
fn patterns(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .collect()
}
fn size(value: &str) -> anyhow::Result<u64> {
    let text = value.trim().to_ascii_lowercase().replace(' ', "");
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let number = text[..split].parse::<u64>()?;
    let multiplier = match &text[split..] {
        "" | "b" => 1,
        "k" | "kb" => 1000,
        "kib" => 1024,
        "m" | "mb" => 1000000,
        "mib" => 1048576,
        "g" | "gb" => 1000000000,
        "gib" => 1073741824,
        _ => anyhow::bail!("サイズは整数と B / KB / KiB / MB / MiB / GB / GiB で入力してください"),
    };
    number
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow::anyhow!("サイズが大きすぎます"))
}
impl Params {
    pub fn to_options(&self) -> anyhow::Result<core::Options> {
        let opt = core::Options {
            exclude_contains: patterns(&self.exclude),
            exclude_glob: patterns(&self.glob),
            exclude_regex: patterns(&self.regex),
            min_file_size: size(&self.min_size)?,
            max_depth: self.max_depth,
            follow_links: self.follow_links,
            count_hardlinks: self.count_hardlinks,
            one_file_system: self.one_file_system,
            threads: if self.threads == 0 {
                core::default_threads()
            } else {
                self.threads
            },
            compute_physical: self.size_mode == 0,
            approximate_sizes: self.size_mode == 2,
            io_profile: self.io_profile,
            prefetch: match self.prefetch {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            },
            dir_yield_every: Arc::new(AtomicUsize::new(self.dir_yield)),
            use_mft: self.use_mft,
            ..core::Options::default()
        };
        core::validate_options(&opt)?;
        Ok(opt)
    }
}
pub fn start(root: PathBuf, params: Params) -> anyhow::Result<Handle> {
    let mut opt = params.to_options()?;
    // Bound queued subtree maps while the UI folds previous results.
    // Dropping the receiver unblocks a sender when the window closes.
    let (tx, rx) = mpsc::sync_channel(2);
    let files_seen = Arc::new(AtomicU64::new(0));
    let counter = files_seen.clone();
    opt.progress_every = 256;
    opt.progress_callback = Some(Arc::new(move |n| {
        counter.fetch_max(n, Ordering::Relaxed);
    }));
    let cancel = opt.cancel.clone();
    let worker_cancel = cancel.clone();
    let errors = opt.error_count.clone();
    let error_details = Arc::new(std::sync::Mutex::new(Vec::new()));
    let details = error_details.clone();
    opt.error_report = Some(Arc::new(move |message| {
        let mut messages = details.lock().unwrap_or_else(|e| e.into_inner());
        if messages.len() < 20 {
            messages.push(message.to_string());
        }
    }));
    std::thread::Builder::new()
        .name("hyperdu-gui-scan".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                core::scan_directory_mode(&root, &opt, params.mode, |event| {
                    if tx.send(Msg::Core(event)).is_err() {
                        worker_cancel.store(true, Ordering::Relaxed);
                    }
                })
            }));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let _ = tx.send(Msg::Failed(e.to_string()));
                }
                Err(_) => {
                    let _ = tx.send(Msg::Failed("走査スレッドが異常終了しました".into()));
                }
            }
        })?;
    Ok(Handle {
        rx,
        files_seen,
        errors,
        error_details,
        cancel,
    })
}

#[derive(Default)]
pub struct Model {
    pub revision: u64,
    pub root: PathBuf,
    entries: Vec<(PathBuf, Stat)>,
    paths: HashMap<PathBuf, u32>,
    children: HashMap<PathBuf, Vec<u32>>,
    pending: HashSet<PathBuf>,
    pub direct_files: Stat,
}
impl Model {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            ..Self::default()
        }
    }
    pub fn expect(&mut self, dirs: Vec<PathBuf>) {
        self.pending = dirs.iter().cloned().collect();
        self.insert_all(dirs.into_iter().map(|p| (p, Stat::default())));
    }
    pub fn is_pending(&self, path: &Path) -> bool {
        self.pending.contains(path)
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
    pub fn absorb(&mut self, root: &Path, map: StatMap) {
        self.pending.remove(root);
        self.insert_all(map.into_iter());
    }
    pub fn replace_batch(&mut self, mut map: StatMap) {
        let total = map.remove(&self.root).unwrap_or_default();
        let children = map
            .iter()
            .filter(|(p, _)| p.parent() == Some(self.root.as_path()))
            .fold(Stat::default(), |mut a, (_, s)| {
                a.logical += s.logical;
                a.physical += s.physical;
                a.files += s.files;
                a
            });
        self.entries.clear();
        self.paths.clear();
        self.children.clear();
        self.pending.clear();
        self.direct_files = Stat {
            logical: total.logical.saturating_sub(children.logical),
            physical: total.physical.saturating_sub(children.physical),
            files: total.files.saturating_sub(children.files),
        };
        self.insert_all(map.into_iter());
    }
    fn insert_all(&mut self, items: impl Iterator<Item = (PathBuf, Stat)>) {
        self.revision += 1;
        let mut touched = HashSet::new();
        for (path, stat) in items {
            let Some(parent) = path.parent().map(Path::to_path_buf) else {
                continue;
            };
            if let Some(&index) = self.paths.get(&path) {
                self.entries[index as usize].1 = stat;
            } else {
                let index = self.entries.len() as u32;
                self.paths.insert(path.clone(), index);
                self.entries.push((path, stat));
                self.children.entry(parent.clone()).or_default().push(index);
            }
            touched.insert(parent);
        }
        for parent in touched {
            if let Some(indices) = self.children.get_mut(&parent) {
                indices.sort_unstable_by(|a, b| {
                    self.entries[*b as usize]
                        .1
                        .physical
                        .cmp(&self.entries[*a as usize].1.physical)
                        .then_with(|| {
                            self.entries[*a as usize]
                                .0
                                .cmp(&self.entries[*b as usize].0)
                        })
                });
            }
        }
    }
    pub fn children_of(&self, dir: &Path) -> &[u32] {
        self.children.get(dir).map(Vec::as_slice).unwrap_or(&[])
    }
    pub fn entry(&self, index: u32) -> (&Path, &Stat) {
        let (p, s) = &self.entries[index as usize];
        (p, s)
    }
    pub fn has_children(&self, dir: &Path) -> bool {
        !self.children_of(dir).is_empty()
    }
    pub fn total_of(&self, dir: &Path) -> Stat {
        if dir == self.root {
            return self
                .children_of(dir)
                .iter()
                .fold(self.direct_files, |mut a, &i| {
                    let s = self.entry(i).1;
                    a.logical += s.logical;
                    a.physical += s.physical;
                    a.files += s.files;
                    a
                });
        }
        self.paths
            .get(dir)
            .map(|&i| self.entries[i as usize].1)
            .unwrap_or_default()
    }
    pub fn direct_files_of(&self, dir: &Path) -> Stat {
        if dir == self.root {
            return self.direct_files;
        }
        let mut total = self.total_of(dir);
        for &index in self.children_of(dir) {
            let s = self.entry(index).1;
            total.logical = total.logical.saturating_sub(s.logical);
            total.physical = total.physical.saturating_sub(s.physical);
            total.files = total.files.saturating_sub(s.files);
        }
        total
    }
    pub fn rows(&self) -> Vec<(PathBuf, Stat)> {
        let mut rows = self.entries.clone();
        rows.push((self.root.clone(), self.total_of(&self.root)));
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn options_and_invalid_patterns() {
        let p = Params {
            min_size: "2 MiB".into(),
            regex: "[".into(),
            ..Params::default()
        };
        assert!(p.to_options().is_err());
        let p = Params {
            regex: "ignored$".into(),
            threads: 3,
            max_depth: 2,
            follow_links: true,
            count_hardlinks: true,
            one_file_system: true,
            size_mode: 1,
            io_profile: core::IoProfile::Gentle,
            prefetch: 2,
            ..p
        };
        let o = p.to_options().unwrap();
        assert_eq!(o.min_file_size, 2097152);
        assert_eq!(o.threads, 3);
        assert_eq!(o.max_depth, 2);
        assert!(o.follow_links && o.count_hardlinks && o.one_file_system);
        assert!(!o.compute_physical);
        assert_eq!(o.prefetch, Some(false));
    }
    #[test]
    fn placeholders_are_replaced_and_direct_files_have_no_fake_path() {
        let root = PathBuf::from("root");
        let child = root.join("a");
        let mut m = Model::new(root.clone());
        let pending = root.join("pending");
        m.expect(vec![child.clone(), pending]);
        m.direct_files = Stat {
            logical: 7,
            physical: 8,
            files: 1,
        };
        let mut map = StatMap::default();
        map.insert(
            child.clone(),
            Stat {
                logical: 3,
                physical: 4,
                files: 1,
            },
        );
        m.absorb(&child, map);
        assert_eq!(m.entry_count(), 2);
        assert_eq!(m.pending_count(), 1);
        assert_eq!(m.total_of(&root).logical, 10);
        assert_eq!(m.rows().len(), 3);
    }
    #[test]
    fn both_gui_modes_match_core_total_and_exports() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("a")).unwrap();
        std::fs::write(temp.path().join("root"), [1; 17]).unwrap();
        std::fs::write(temp.path().join("a/data"), [2; 9]).unwrap();
        std::fs::create_dir(temp.path().join("empty")).unwrap();
        std::fs::create_dir(temp.path().join("b")).unwrap();
        std::fs::hard_link(temp.path().join("a/data"), temp.path().join("b/link")).unwrap();
        std::fs::create_dir(temp.path().join("a/deep")).unwrap();
        std::fs::write(temp.path().join("a/deep/ignored"), [3; 25]).unwrap();
        for mode in [ScanMode::Interactive, ScanMode::Batch] {
            for filtered in [false, true] {
                let params = Params {
                    mode,
                    min_size: if filtered { "10" } else { "0" }.into(),
                    max_depth: if filtered { 1 } else { 0 },
                    regex: if filtered { "ignored$" } else { "" }.into(),
                    follow_links: true,
                    ..Params::default()
                };
                let expected =
                    core::scan_directory(temp.path(), &params.to_options().unwrap()).unwrap();
                let handle = start(temp.path().to_path_buf(), params).unwrap();
                let mut model = Model::new(temp.path().to_path_buf());
                loop {
                    match handle
                        .rx
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .unwrap()
                    {
                        Msg::Core(ScanEvent::RootListed {
                            directories,
                            direct_files,
                            ..
                        }) => {
                            model.expect(directories);
                            model.direct_files = direct_files;
                        }
                        Msg::Core(ScanEvent::ChildCompleted { root, map }) => {
                            model.absorb(&root, map)
                        }
                        Msg::Core(ScanEvent::BatchCompleted { map }) => model.replace_batch(map),
                        Msg::Core(ScanEvent::Finished) => break,
                        _ => panic!("unexpected scan event"),
                    }
                }
                let total = model.total_of(temp.path());
                let expected = expected.get(temp.path()).unwrap();
                assert_eq!(
                    (total.logical, total.physical, total.files),
                    (expected.logical, expected.physical, expected.files)
                );
                let mut json = Vec::new();
                core::report::write_json(&mut json, &model.rows()).unwrap();
                assert!(!json.is_empty());
            }
        }
    }
}
