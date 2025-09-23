use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc,
    },
    time::Instant,
};

use egui::{self, Align, FontData, FontDefinitions, FontFamily, Layout, RichText};
use egui_extras::TableBuilder;
use humansize::{format_size, BINARY};
use hyperdu_core as core;
use hyperdu_core::{Stat, StatMap};

fn puffin_frame() {}

#[derive(Default)]
pub struct App {
    root: Option<PathBuf>,
    exclude: String,
    min_file: u64,
    max_depth: u32,
    follow: bool,
    scanning: bool,
    selected: Option<PathBuf>,
    tree: Option<Node>,
    rx: Option<mpsc::Receiver<Vec<(PathBuf, Stat)>>>,
    // Live metrics
    files_processed: Option<Arc<AtomicU64>>,
    start_at: Option<Instant>,
    last_count: u64,
    last_at: Option<Instant>,
    yield_current: Option<Arc<AtomicUsize>>,
    uring_batch: Option<Arc<AtomicUsize>>,
    uring_depth: Option<Arc<AtomicUsize>>,
    uring_fail: Option<Arc<std::sync::atomic::AtomicU64>>,
    uring_wait_ns: Option<Arc<std::sync::atomic::AtomicU64>>,
    uring_enq: Option<Arc<std::sync::atomic::AtomicU64>>,
    uring_cqe: Option<Arc<std::sync::atomic::AtomicU64>>,
    uring_err: Option<Arc<std::sync::atomic::AtomicU64>>,
}

// Default is derived above

#[derive(Clone)]
struct Node {
    path: PathBuf,
    name: String,
    stat: Stat,
    children: Vec<Node>,
}

impl Default for Node {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            name: String::new(),
            stat: Stat::default(),
            children: vec![],
        }
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_fonts(&cc.egui_ctx);
        Self::default()
    }
    pub fn start_scan(&mut self, root: PathBuf) {
        self.scanning = true;
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let exclude = self.exclude.clone();
        let min_file = self.min_file;
        let max_depth = self.max_depth;
        let follow = self.follow;
        let files_counter = Arc::new(AtomicU64::new(0));
        self.files_processed = Some(files_counter.clone());
        self.start_at = Some(Instant::now());
        self.last_count = 0;
        self.last_at = self.start_at;
        let dir_yield = Arc::new(AtomicUsize::new(0));
        self.yield_current = Some(dir_yield.clone());
        let uring_batch = Arc::new(AtomicUsize::new(128));
        let uring_depth = Arc::new(AtomicUsize::new(256));
        let uring_fail = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let uring_wait_ns = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let uring_enq = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let uring_cqe = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let uring_err = Arc::new(std::sync::atomic::AtomicU64::new(0));
        self.uring_batch = Some(uring_batch.clone());
        self.uring_depth = Some(uring_depth.clone());
        self.uring_fail = Some(uring_fail.clone());
        self.uring_wait_ns = Some(uring_wait_ns.clone());
        self.uring_enq = Some(uring_enq.clone());
        self.uring_cqe = Some(uring_cqe.clone());
        self.uring_err = Some(uring_err.clone());
        std::thread::spawn(move || {
            let mut opt = core::Options {
                exclude_contains: exclude
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                max_depth,
                min_file_size: min_file,
                follow_links: follow,
                threads: std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4),
                progress_every: 8192,
                progress_callback: None,
                progress_path_callback: None,
                compute_physical: true,
                approximate_sizes: false,
                dir_yield_every: dir_yield.clone(),
                uring_batch,
                uring_sq_depth: uring_depth,
                uring_sqe_fail: uring_fail,
                uring_submit_wait_ns: uring_wait_ns,
                uring_sqe_enq: uring_enq,
                uring_cqe_comp: uring_cqe,
                uring_cqe_err: uring_err,
                ..core::Options::default()
            };
            // Install progress callback with live tuning (quiet)
            let t0 = Instant::now();
            let last = Arc::new(std::sync::Mutex::new((0u64, t0)));
            let lc = last.clone();
            let y_atomic = opt.dir_yield_every.clone();
            let yield_candidates: [usize; 5] = [8192, 16384, 32768, 65536, 131072];
            let tuner = Arc::new(std::sync::Mutex::new((2usize, 1isize, 0.0f64))); // idx, dir, last_rate
            let tuner_cl = tuner.clone();
            opt.progress_callback = Some(Arc::new(move |n| {
                files_counter.store(n, Ordering::Relaxed);
                let now = Instant::now();
                let (prev_n, prev_t) = *lc.lock().unwrap();
                let dt = now.duration_since(prev_t).as_secs_f64().max(1e-6);
                let dn = n.saturating_sub(prev_n) as f64;
                let recent = dn / dt;
                *lc.lock().unwrap() = (n, now);
                // tune with 5% threshold
                let mut st = tuner_cl.lock().unwrap();
                let (ref mut idx, ref mut dir, ref mut last_rate) = *st;
                if *last_rate == 0.0 {
                    *last_rate = recent;
                }
                let degrade = recent < *last_rate * 0.95;
                let improve = recent > *last_rate * 1.05;
                if degrade {
                    *dir = -*dir;
                }
                if degrade || improve {
                    let new_idx = (*idx as isize + *dir)
                        .clamp(0, (yield_candidates.len() - 1) as isize)
                        as usize;
                    if new_idx != *idx {
                        *idx = new_idx;
                        let new_y = yield_candidates[*idx];
                        y_atomic.store(new_y, Ordering::Relaxed);
                    }
                }
                *last_rate = recent;
            }));
            let res = core::scan_directory(&root, &opt).unwrap_or_default();
            let mut v: Vec<_> = res.into_iter().collect();
            v.sort_unstable_by_key(|(_, s)| std::cmp::Reverse(s.physical));
            let _ = tx.send(v);
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        puffin_frame();
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("フォルダ…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        self.root = Some(p);
                    }
                }
                if ui.button("スキャン").clicked() {
                    if let Some(root) = self.root.clone() {
                        self.start_scan(root);
                    }
                }
                ui.separator();
                ui.label("除外");
                ui.text_edit_singleline(&mut self.exclude);
                ui.label("最小サイズ(B)");
                ui.add(egui::DragValue::new(&mut self.min_file).speed(1.0));
                ui.label("深さ上限(0=無制限)");
                ui.add(egui::DragValue::new(&mut self.max_depth).range(0..=u32::MAX));
                ui.checkbox(&mut self.follow, "リンク追従");
                if let Some(start) = self.start_at {
                    if let Some(counter) = &self.files_processed {
                        let n = counter.load(Ordering::Relaxed);
                        let dt = start.elapsed().as_secs_f64().max(1e-6);
                        let total_rate = (n as f64) / dt;
                        let recent = if let Some(last_at) = self.last_at {
                            let dtn = last_at.elapsed().as_secs_f64().max(1e-6);
                            let dn = n.saturating_sub(self.last_count) as f64;
                            dn / dtn
                        } else {
                            0.0
                        };
                        self.last_count = n;
                        self.last_at = Some(Instant::now());
                        let y = self
                            .yield_current
                            .as_ref()
                            .map(|a| a.load(Ordering::Relaxed))
                            .unwrap_or(0);
                        ui.separator();
                        ui.monospace(format!(
                            "files/s: {total_rate:.0} (recent {recent:.0})  yield: {y}"
                        ));
                        if let (Some(b), Some(d)) = (&self.uring_batch, &self.uring_depth) {
                            let depth = d.load(Ordering::Relaxed);
                            let batch = b.load(Ordering::Relaxed);
                            ui.monospace(format!("uring: depth={depth} batch={batch}"));
                        }
                        if let (Some(f), Some(w), Some(e), Some(c), Some(er)) = (
                            &self.uring_fail,
                            &self.uring_wait_ns,
                            &self.uring_enq,
                            &self.uring_cqe,
                            &self.uring_err,
                        ) {
                            let fail = f.load(Ordering::Relaxed);
                            let wait_ms = (w.load(Ordering::Relaxed) as f64) / 1.0e6;
                            let enq = e.load(Ordering::Relaxed);
                            let cqe = c.load(Ordering::Relaxed);
                            let err = er.load(Ordering::Relaxed);
                            ui.monospace(format!(
                                "uring-metrics: fail={fail} wait={wait_ms:.2}ms enq={enq} cqe={cqe} err={err}"
                            ));
                        }
                    }
                }
                if let Some(root) = &self.root {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(root.display().to_string()).monospace());
                    });
                }
            });
        });

        // Receive scan result
        if let Some(rx) = &self.rx {
            if let Ok(v) = rx.try_recv() {
                self.scanning = false;
                let map: StatMap = v.into_iter().collect();
                if let Some(root) = &self.root {
                    self.tree = Some(build_tree(root, &map));
                    self.selected = Some(root.clone());
                }
                self.rx = None;
            } else if self.scanning {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }

        egui::SidePanel::left("left")
            .resizable(true)
            .show(ctx, |ui| {
                ui.heading("ディレクトリツリー");
                if let Some(tree) = &mut self.tree {
                    show_tree(ui, tree, &mut self.selected);
                } else {
                    ui.label("スキャン結果なし");
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("内容");
            if let (Some(sel), Some(tree)) = (&self.selected, &self.tree) {
                if let Some(node) = find_node(tree, sel) {
                    show_children_table(ui, node);
                } else {
                    ui.label("選択ノードが見つかりません");
                }
            } else {
                ui.label("左からディレクトリを選択してください");
            }
        });
    }
}

fn build_tree(root: &Path, map: &StatMap) -> Node {
    // Build parent -> children index
    let mut idx: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for p in map.keys() {
        if let Some(parent) = p.parent() {
            idx.entry(parent.to_path_buf()).or_default().push(p.clone());
        }
    }
    // Sort children by physical desc
    for v in idx.values_mut() {
        v.sort_by_key(|p| std::cmp::Reverse(map.get(p).map(|s| s.physical).unwrap_or(0)));
    }
    fn make_node(p: &Path, idx: &HashMap<PathBuf, Vec<PathBuf>>, map: &StatMap) -> Node {
        let name = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(p.as_os_str().to_string_lossy().as_ref())
            .to_string();
        let mut node = Node {
            path: p.to_path_buf(),
            name,
            stat: *map.get(p).unwrap_or(&Stat::default()),
            children: vec![],
        };
        if let Some(children) = idx.get(p) {
            for c in children {
                node.children.push(make_node(c, idx, map));
            }
        }
        node
    }
    make_node(root, &idx, map)
}

fn show_tree(ui: &mut egui::Ui, node: &mut Node, selected: &mut Option<PathBuf>) {
    let label = format!(
        "{}  ({} / {})",
        node.name,
        format_size(node.stat.physical, BINARY),
        format_size(node.stat.logical, BINARY)
    );
    let resp = egui::CollapsingHeader::new(label).show(ui, |ui| {
        for child in &mut node.children {
            show_tree(ui, child, selected);
        }
    });
    if resp.header_response.clicked() {
        *selected = Some(node.path.clone());
    }
}

fn find_node<'a>(node: &'a Node, p: &Path) -> Option<&'a Node> {
    if node.path == p {
        return Some(node);
    }
    for c in &node.children {
        if let Some(n) = find_node(c, p) {
            return Some(n);
        }
    }
    None
}

fn show_children_table(ui: &mut egui::Ui, parent: &Node) {
    let total = parent.stat.physical.max(1);
    let rows = parent.children.len();
    let table = TableBuilder::new(ui)
        .striped(true)
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(egui_extras::Column::auto())
        .column(egui_extras::Column::remainder());
    table
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.label(egui::RichText::new("名前").strong());
            });
            header.col(|ui| {
                ui.label(egui::RichText::new("サイズ(物理/論理)").strong());
            });
        })
        .body(|body| {
            body.rows(22.0, rows, |mut row| {
                let i = row.index();
                let child = &parent.children[i];
                row.col(|ui| {
                    ui.label(&child.name);
                });
                row.col(|ui| {
                    let frac = (child.stat.physical as f64 / total as f64) as f32;
                    ui.add(egui::ProgressBar::new(frac).show_percentage().text(format!(
                        "{} / {}",
                        format_size(child.stat.physical, BINARY),
                        format_size(child.stat.logical, BINARY)
                    )));
                });
            });
        });
}

fn configure_fonts(ctx: &egui::Context) {
    // Start from egui defaults and add UTF-8 capable system fallbacks (CJK, Emoji).
    let mut fonts = FontDefinitions::default();

    // Helper to add a font file if present
    let mut add_font_file = |key: &str, path: &std::path::Path| -> bool {
        match std::fs::read(path) {
            Ok(bytes) => {
                fonts
                    .font_data
                    .insert(key.to_string(), FontData::from_owned(bytes).into());
                true
            }
            Err(_) => false,
        }
    };

    // Collect candidate font files per platform
    let (dirs, cjk_candidates, emoji_candidates, ui_candidates, mono_candidates) =
        platform_font_candidates();

    // Find first matches
    let find_first = |names: &[&str]| find_font_in_dirs(&dirs, names);

    if let Some(p) = find_first(&cjk_candidates) {
        if add_font_file("cjk", &p) {
            // Append CJK fallback
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("cjk".to_string());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push("cjk".to_string());
        }
    }
    if let Some(p) = find_first(&emoji_candidates) {
        if add_font_file("emoji", &p) {
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("emoji".to_string());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push("emoji".to_string());
        }
    }
    if let Some(p) = find_first(&ui_candidates) {
        if add_font_file("ui", &p) {
            // Prefer UI font first for proportional
            let fam = fonts.families.entry(FontFamily::Proportional).or_default();
            fam.insert(0, "ui".to_string());
        }
    }
    if let Some(p) = find_first(&mono_candidates) {
        if add_font_file("mono", &p) {
            let fam = fonts.families.entry(FontFamily::Monospace).or_default();
            fam.insert(0, "mono".to_string());
            // Also add as fallback to proportional for code snippets
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("mono".to_string());
        }
    }

    ctx.set_fonts(fonts);
}

fn platform_font_candidates() -> (
    Vec<std::path::PathBuf>,
    Vec<&'static str>,
    Vec<&'static str>,
    Vec<&'static str>,
    Vec<&'static str>,
) {
    #[cfg(target_os = "windows")]
    {
        let dirs = vec![std::path::PathBuf::from(r"C:\\Windows\\Fonts")];
        let cjk = vec![
            "YuGothR.ttc",
            "YuGothM.ttc",
            "meiryo.ttc",
            "MS Gothic.ttf", // JP
            "msyh.ttc",
            "msyh.ttf",
            "Microsoft YaHei.ttf",
            "SimSun.ttc", // SC
            "MingLiU.ttf",
            "PMingLiU.ttf", // TC
            "malgun.ttf",
            "Malgun Gothic.ttf", // KR
        ];
        let emoji = vec!["seguiemj.ttf", "SegoeUIEmoji.ttf"]; // Windows emoji
        let ui = vec!["segoeui.ttf", "YuGothUI.ttc", "meiryo.ttc"];
        let mono = vec![
            "consola.ttf",
            "CascadiaMono.ttf",
            "CascadiaCode.ttf",
            "msmincho.ttc",
        ];
        return (dirs, cjk, emoji, ui, mono);
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let mut dirs = vec![
            std::path::PathBuf::from("/System/Library/Fonts"),
            std::path::PathBuf::from("/Library/Fonts"),
        ];
        if let Some(h) = home {
            dirs.push(h.join("Library/Fonts"));
        }
        let cjk = vec![
            "HiraginoSans-W3.ttc",
            "HiraginoSans-W4.ttc", // JP
            "PingFang.ttc",
            "PingFangSC.ttc",
            "PingFangTC.ttc",       // CN/TW
            "AppleSDGothicNeo.ttc", // KR
        ];
        let emoji = vec!["Apple Color Emoji.ttc", "AppleColorEmoji.ttf"];
        let ui = vec![
            "SFNS.ttf",
            "HelveticaNeueDeskInterface.ttc",
            "HiraginoSans-W3.ttc",
        ];
        let mono = vec!["Menlo.ttc", "SFMono.ttf", "OsakaMono.ttf"];
        return (dirs, cjk, emoji, ui, mono);
    }
    #[cfg(target_os = "linux")]
    {
        let mut dirs = vec![
            std::path::PathBuf::from("/usr/share/fonts"),
            std::path::PathBuf::from("/usr/local/share/fonts"),
        ];
        if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
            dirs.push(home.join(".local/share/fonts"));
            dirs.push(home.join(".fonts"));
        }
        let cjk = vec![
            // Noto CJK families
            "NotoSansCJK-Regular.ttc",
            "NotoSansCJKjp-Regular.otf",
            "NotoSansJP-Regular.otf",
            "NotoSansJP-Regular.ttf",
            "NotoSansSC-Regular.otf",
            "NotoSansTC-Regular.otf",
            "NotoSansKR-Regular.otf",
            // Source Han
            "SourceHanSans-Regular.otf",
            "SourceHanSerif-Regular.otf",
            // Others
            "WenQuanYiMicroHei.ttf",
            "DroidSansFallback.ttf",
        ];
        let emoji = vec![
            "NotoColorEmoji.ttf",
            "EmojiOneColor-SVGinOT.ttf",
            "TwemojiMozilla.ttf",
        ];
        let ui = vec!["DejaVuSans.ttf", "NotoSans-Regular.ttf", "Ubuntu-R.ttf"];
        let mono = vec![
            "DejaVuSansMono.ttf",
            "NotoSansMono-Regular.ttf",
            "UbuntuMono-R.ttf",
        ];
        return (dirs, cjk, emoji, ui, mono);
    }
    #[allow(unreachable_code)]
    {
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new())
    }
}

fn find_font_in_dirs(dirs: &[std::path::PathBuf], names: &[&str]) -> Option<std::path::PathBuf> {
    if dirs.is_empty() || names.is_empty() {
        return None;
    }
    let lower_names = names
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut stack: Vec<std::path::PathBuf> = dirs.to_vec();
    let mut visited = 0usize;
    while let Some(p) = stack.pop() {
        if visited > 50_000 {
            break;
        } // safety cap to avoid long walks
        visited += 1;
        let Ok(rd) = std::fs::read_dir(&p) else {
            continue;
        };
        for ent in rd.flatten() {
            let path = ent.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(file) = path.file_name().and_then(|s| s.to_str()) {
                let lf = file.to_ascii_lowercase();
                if lower_names.iter().any(|n| lf.ends_with(n)) {
                    return Some(path);
                }
            }
        }
    }
    None
}
