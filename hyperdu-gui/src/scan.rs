//! GUI transport and directory index. All filesystem policy lives in hyperdu-core.
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc,
    },
};

use hyperdu_core::{self as core, ScanEvent, ScanMode, Stat, StatMap};

pub const CHUNK_NODES: usize = 256;
type Orders = [Arc<[u32]>; 4];
type Node = (u32, PathBuf, Stat, bool);
pub enum Update {
    Nodes(Vec<Node>),
    Parent(PathBuf, Orders),
    Direct(Stat),
    Resolved(PathBuf),
    Reset,
}
impl Update {
    pub fn work(&self) -> usize {
        match self {
            Self::Nodes(nodes) => nodes.len(),
            _ => 1,
        }
    }
}
struct Prepared {
    nodes: Vec<Node>,
    parents: Vec<(PathBuf, Orders)>,
}
pub enum Msg {
    Update(Update),
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
pub fn start(
    root: PathBuf,
    params: Params,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> anyhow::Result<Handle> {
    let mut opt = params.to_options()?;
    // Queue only bounded node chunks and already-sorted parent indices.
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
            let mut model = Model::new(root.clone());
            model.cancel = Some(worker_cancel.clone());
            let send = |message| {
                let sent = tx.send(message).is_ok();
                if sent {
                    wake();
                } else {
                    worker_cancel.store(true, Ordering::Relaxed);
                }
                sent
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                core::scan_directory_mode(&root, &opt, params.mode, |event| {
                    let (prepared, resolved) = match event {
                        ScanEvent::RootListed {
                            directories,
                            direct_files,
                            ..
                        } => {
                            model.direct_files = direct_files;
                            (model.expect(directories), None)
                        }
                        ScanEvent::ChildCompleted { root, map } => {
                            (model.absorb(&root, map), Some(root))
                        }
                        ScanEvent::BatchCompleted { map } => {
                            let prepared = model.replace_batch(map);
                            if !send(Msg::Update(Update::Reset)) {
                                return;
                            }
                            (prepared, None)
                        }
                        event => {
                            send(Msg::Core(event));
                            return;
                        }
                    };
                    if !send(Msg::Update(Update::Direct(model.direct_files))) {
                        return;
                    }
                    let mut nodes = prepared.nodes.into_iter();
                    loop {
                        if worker_cancel.load(Ordering::Relaxed) {
                            return;
                        }
                        let chunk: Vec<_> = nodes.by_ref().take(CHUNK_NODES).collect();
                        if chunk.is_empty() {
                            break;
                        }
                        if !send(Msg::Update(Update::Nodes(chunk))) {
                            return;
                        }
                    }
                    // FIFO guarantees all referenced IDs exist before publication.
                    for (parent, orders) in prepared.parents {
                        if worker_cancel.load(Ordering::Relaxed)
                            || !send(Msg::Update(Update::Parent(parent, orders)))
                        {
                            return;
                        }
                    }
                    if let Some(root) = resolved {
                        send(Msg::Update(Update::Resolved(root)));
                    }
                })
            }));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    send(Msg::Failed(e.to_string()));
                }
                Err(_) => {
                    send(Msg::Failed("走査スレッドが異常終了しました".into()));
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
    pub root: PathBuf,
    cancel: Option<Arc<AtomicBool>>,
    entries: Vec<(PathBuf, Stat)>,
    paths: HashMap<PathBuf, u32>,
    children: HashMap<PathBuf, Orders>,
    child_sums: HashMap<PathBuf, Stat>,
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
    fn expect(&mut self, dirs: Vec<PathBuf>) -> Prepared {
        self.pending = dirs.iter().cloned().collect();
        self.insert_all(dirs.into_iter().map(|p| (p, Stat::default())))
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
    fn absorb(&mut self, root: &Path, map: StatMap) -> Prepared {
        self.pending.remove(root);
        self.insert_all(map.into_iter())
    }
    fn replace_batch(&mut self, mut map: StatMap) -> Prepared {
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
        self.child_sums.clear();
        self.pending.clear();
        self.direct_files = Stat {
            logical: total.logical.saturating_sub(children.logical),
            physical: total.physical.saturating_sub(children.physical),
            files: total.files.saturating_sub(children.files),
        };
        self.insert_all(map.into_iter())
    }
    fn insert_all(&mut self, items: impl Iterator<Item = (PathBuf, Stat)>) -> Prepared {
        let mut touched: HashMap<PathBuf, Vec<u32>> = HashMap::new();
        let mut nodes = Vec::new();
        for (path, stat) in items {
            if nodes.len() % CHUNK_NODES == 0
                && self
                    .cancel
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::Relaxed))
            {
                break;
            }
            let Some(parent) = path.parent().map(Path::to_path_buf) else {
                continue;
            };
            let indices = touched
                .entry(parent)
                .or_insert_with(|| self.children_of(path.parent().unwrap()).to_vec());
            let existing = self.paths.get(&path).copied();
            let index = existing.unwrap_or(self.entries.len() as u32);
            let pending = self.pending.contains(&path);
            self.set_node(index, path.clone(), stat, pending);
            if existing.is_none() {
                indices.push(index);
            }
            nodes.push((index, path, stat, pending));
        }
        let mut parents = Vec::with_capacity(touched.len());
        for (parent, indices) in touched {
            if self
                .cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed))
            {
                break;
            }
            let orders: Orders = std::array::from_fn(|sort| {
                if self
                    .cancel
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::Relaxed))
                {
                    return Arc::from([]);
                }
                let mut sorted = indices.clone();
                sorted.sort_unstable_by(|a, b| {
                    let (pa, sa) = self.entry(*a);
                    let (pb, sb) = self.entry(*b);
                    match sort {
                        1 => sb.logical.cmp(&sa.logical),
                        2 => sb.files.cmp(&sa.files),
                        3 => pa.cmp(pb),
                        _ => sb.physical.cmp(&sa.physical),
                    }
                    .then_with(|| pa.cmp(pb))
                });
                Arc::from(sorted)
            });
            self.children.insert(parent.clone(), orders.clone());
            parents.push((parent, orders));
        }
        Prepared { nodes, parents }
    }
    fn set_node(&mut self, index: u32, path: PathBuf, stat: Stat, pending: bool) {
        let old = if (index as usize) < self.entries.len() {
            std::mem::replace(&mut self.entries[index as usize].1, stat)
        } else {
            assert_eq!(
                index as usize,
                self.entries.len(),
                "ordered display node IDs"
            );
            self.paths.insert(path.clone(), index);
            self.entries.push((path.clone(), stat));
            Stat::default()
        };
        if let Some(parent) = path.parent() {
            let sum = self.child_sums.entry(parent.to_path_buf()).or_default();
            sum.files = sum.files.saturating_sub(old.files) + stat.files;
            sum.logical = sum.logical.saturating_sub(old.logical) + stat.logical;
            sum.physical = sum.physical.saturating_sub(old.physical) + stat.physical;
        }
        if pending {
            self.pending.insert(path);
        } else {
            self.pending.remove(&path);
        }
    }
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Nodes(nodes) => {
                for (index, path, stat, pending) in nodes {
                    self.set_node(index, path, stat, pending);
                }
            }
            Update::Parent(parent, orders) => {
                self.children.insert(parent, orders);
            }
            Update::Direct(stat) => self.direct_files = stat,
            Update::Resolved(path) => {
                self.pending.remove(&path);
            }
            Update::Reset => *self = Self::new(self.root.clone()),
        }
    }
    pub fn children_sorted(&self, dir: &Path, sort: u8) -> &[u32] {
        self.children
            .get(dir)
            .map(|orders| orders[usize::from(sort.min(3))].as_ref())
            .unwrap_or(&[])
    }
    pub fn children_of(&self, dir: &Path) -> &[u32] {
        self.children_sorted(dir, 0)
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
            let mut total = self.child_sums.get(dir).copied().unwrap_or_default();
            total.files += self.direct_files.files;
            total.logical += self.direct_files.logical;
            total.physical += self.direct_files.physical;
            return total;
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
        let children = self.child_sums.get(dir).copied().unwrap_or_default();
        total.logical = total.logical.saturating_sub(children.logical);
        total.physical = total.physical.saturating_sub(children.physical);
        total.files = total.files.saturating_sub(children.files);
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
pub fn test_handle(capacity: usize) -> (mpsc::SyncSender<Msg>, Handle) {
    let (tx, rx) = mpsc::sync_channel(capacity);
    (
        tx,
        Handle {
            rx,
            files_seen: Arc::default(),
            errors: Arc::default(),
            error_details: Arc::default(),
            cancel: Arc::default(),
        },
    )
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
                let wakes = Arc::new(AtomicUsize::new(0));
                let observed = wakes.clone();
                let handle = start(
                    temp.path().to_path_buf(),
                    params,
                    Arc::new(move || {
                        observed.fetch_add(1, Ordering::Relaxed);
                    }),
                )
                .unwrap();
                let mut model = Model::new(temp.path().to_path_buf());
                loop {
                    match handle
                        .rx
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .unwrap()
                    {
                        Msg::Update(update) => model.apply(update),
                        Msg::Core(ScanEvent::Finished) => break,
                        _ => panic!("unexpected scan event"),
                    }
                }
                assert!(
                    wakes.load(Ordering::Relaxed) > 0,
                    "event arrival requests repaint"
                );
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

    #[test]
    fn worker_orders_and_cached_totals_match_after_bounded_updates() {
        let root = PathBuf::from("root");
        let mut worker = Model::new(root.clone());
        let mut ui = Model::new(root.clone());
        let prepared = worker.insert_all(
            [("a", 10, 1, 3), ("b", 3, 9, 1), ("c", 6, 5, 2)]
                .into_iter()
                .map(|(name, logical, physical, files)| {
                    (
                        root.join(name),
                        Stat {
                            logical,
                            physical,
                            files,
                        },
                    )
                }),
        );
        for node in prepared.nodes {
            ui.apply(Update::Nodes(vec![node]));
        }
        for (parent, orders) in prepared.parents {
            ui.apply(Update::Parent(parent, orders));
        }
        for (sort, names) in ["bca", "acb", "acb", "abc"].into_iter().enumerate() {
            let actual: String = ui
                .children_sorted(&root, sort as u8)
                .iter()
                .map(|id| {
                    ui.entry(*id)
                        .0
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            assert_eq!(actual, names);
        }
        assert_eq!(ui.total_of(&root).files, 6);
        assert_eq!(ui.total_of(&root).physical, 15);
        let unchanged = ui.children[&root][0].clone();
        let other = worker.insert_all(std::iter::once((root.join("a/deep"), Stat::default())));
        for node in other.nodes {
            ui.apply(Update::Nodes(vec![node]));
        }
        for (parent, orders) in other.parents {
            ui.apply(Update::Parent(parent, orders));
        }
        assert!(
            Arc::ptr_eq(&unchanged, &ui.children[&root][0]),
            "unrelated parent order remains shared"
        );
    }

    #[test]
    fn disconnect_releases_a_blocked_sender_and_cancels_its_handle() {
        let (tx, handle) = test_handle(0);
        let flag = handle.cancel.clone();
        let (done, rx) = mpsc::channel();
        std::thread::spawn(move || {
            done.send(tx.send(Msg::Core(ScanEvent::Finished)).is_err())
                .unwrap();
        });
        drop(handle);
        assert!(flag.load(Ordering::Relaxed));
        assert!(rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap());
    }
}
