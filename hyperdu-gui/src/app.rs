//! The egui application.
//!
//! Everything drawn here is derived from [`scan::Model`] at draw time. The
//! previous version built a `Node` tree of the entire result up front -- on the
//! UI thread, so the window went "Not Responding" for the duration -- and then
//! walked that whole tree on every repaint to find the selected node. Both are
//! gone: the model answers "children of this directory" from an index in about
//! 11 us, and nothing is materialised for a row that is not on screen.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Instant,
};

use egui::{Align, Layout, RichText};
use egui_extras::TableBuilder;
use humansize::{format_size, BINARY};

use crate::{fonts, scan};

/// Messages drained per frame. Each one folds a subtree into the model, which
/// re-sorts the affected parents, so an unbounded drain would let a burst of
/// small subtrees stall a frame.
const MSGS_PER_FRAME: usize = 32;

/// How often to wake while a scan is running, so rows appear as they land.
const REFRESH_MS: u64 = 250;

/// A filesystem this deep means a link loop; recursing without a bound would
/// blow the stack.
const MAX_TREE_DEPTH: usize = 64;

pub struct App {
    root: Option<PathBuf>,
    exclude: String,
    min_file: u64,
    max_depth: u32,
    follow: bool,

    scan: Option<scan::Handle>,
    model: scan::Model,
    /// Directory whose children the table is showing.
    current: PathBuf,
    /// Directories the user has opened in the tree.
    expanded: HashSet<PathBuf>,

    started_at: Option<Instant>,
    finished_in: Option<f64>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            root: None,
            exclude: String::new(),
            min_file: 0,
            max_depth: 0,
            follow: false,
            scan: None,
            model: scan::Model::default(),
            current: PathBuf::new(),
            expanded: HashSet::new(),
            started_at: None,
            finished_in: None,
        }
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        fonts::configure_fonts(&cc.egui_ctx);
        Self::default()
    }

    fn start_scan(&mut self, root: PathBuf) {
        let params = scan::Params {
            exclude: self
                .exclude
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            min_file_size: self.min_file,
            max_depth: self.max_depth,
            follow_links: self.follow,
        };
        self.model = scan::Model::new(root.clone());
        self.current = root.clone();
        self.expanded.clear();
        self.expanded.insert(root.clone());
        self.started_at = Some(Instant::now());
        self.finished_in = None;
        self.scan = Some(scan::start(root, params));
    }

    fn scanning(&self) -> bool {
        self.scan.is_some()
    }

    /// Fold whatever the worker has produced into the model.
    fn drain(&mut self) {
        let Some(handle) = &self.scan else { return };
        let mut done = false;
        for _ in 0..MSGS_PER_FRAME {
            match handle.rx.try_recv() {
                Ok(scan::Msg::Listing { dirs }) => self.model.expect(dirs),
                Ok(scan::Msg::RootFiles(files)) => self.model.absorb_files(files),
                Ok(scan::Msg::Child { path, map }) => self.model.absorb(&path, map),
                Ok(scan::Msg::Done) => {
                    done = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
        if done {
            self.finished_in = self.started_at.map(|t| t.elapsed().as_secs_f64());
            self.scan = None;
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("フォルダ…").clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    self.root = Some(p);
                }
            }
            let can_scan = self.root.is_some() && !self.scanning();
            if ui
                .add_enabled(can_scan, egui::Button::new("スキャン"))
                .clicked()
            {
                if let Some(root) = self.root.clone() {
                    self.start_scan(root);
                }
            }
            if self.scanning() && ui.button("中止").clicked() {
                self.scan = None;
            }
            ui.separator();
            ui.label("除外");
            ui.text_edit_singleline(&mut self.exclude);
            ui.label("最小サイズ(B)");
            ui.add(egui::DragValue::new(&mut self.min_file).speed(1.0));
            ui.label("深さ上限(0=無制限)");
            ui.add(egui::DragValue::new(&mut self.max_depth).range(0..=u32::MAX));
            ui.checkbox(&mut self.follow, "リンク追従");
        });
    }

    fn status(&self, ui: &mut egui::Ui) {
        let Some(start) = self.started_at else { return };
        ui.horizontal(|ui| {
            if let Some(handle) = &self.scan {
                let n = handle.files_seen.load(Ordering::Relaxed);
                let dt = start.elapsed().as_secs_f64().max(1e-6);
                ui.spinner();
                ui.monospace(format!(
                    "{n} files  {:.0} files/s  残り {} ディレクトリ",
                    n as f64 / dt,
                    self.model.pending_count()
                ));
            } else if let Some(secs) = self.finished_in {
                ui.monospace(format!(
                    "完了 {secs:.2} 秒  {} エントリ",
                    self.model.entry_count()
                ));
            }
            if let Some(root) = &self.root {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(root.display().to_string()).monospace());
                });
            }
        });
    }

    /// Breadcrumb from the scan root down to the directory on screen.
    fn breadcrumb(&mut self, ui: &mut egui::Ui) {
        let root = self.model.root.clone();
        if root.as_os_str().is_empty() {
            return;
        }
        let mut jump = None;
        ui.horizontal_wrapped(|ui| {
            if ui.link(root.display().to_string()).clicked() {
                jump = Some(root.clone());
            }
            if let Ok(rel) = self.current.strip_prefix(&root) {
                let mut acc = root.clone();
                for part in rel.iter() {
                    acc.push(part);
                    ui.label("›");
                    if ui.link(part.to_string_lossy()).clicked() {
                        jump = Some(acc.clone());
                    }
                }
            }
        });
        if let Some(p) = jump {
            self.current = p;
        }
    }

    /// The tree, expanded lazily. Only directories the user opened are walked,
    /// so an unopened subtree costs nothing regardless of how big it is.
    fn tree(&mut self, ui: &mut egui::Ui) {
        let root = self.model.root.clone();
        if root.as_os_str().is_empty() {
            ui.label("スキャン結果なし");
            return;
        }
        let mut navigate = None;
        let mut toggles: Vec<PathBuf> = Vec::new();
        self.tree_row(ui, &root, 0, &mut navigate, &mut toggles);
        for p in toggles {
            if !self.expanded.remove(&p) {
                self.expanded.insert(p);
            }
        }
        if let Some(p) = navigate {
            self.current = p;
        }
    }

    fn tree_row(
        &self,
        ui: &mut egui::Ui,
        dir: &Path,
        depth: usize,
        navigate: &mut Option<PathBuf>,
        toggles: &mut Vec<PathBuf>,
    ) {
        if depth > MAX_TREE_DEPTH {
            return;
        }
        let open = self.expanded.contains(dir);
        let expandable = self.model.has_children(dir);
        let name = if depth == 0 {
            dir.display().to_string()
        } else {
            dir.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.display().to_string())
        };

        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 12.0);
            if expandable {
                if ui.small_button(if open { "▾" } else { "▸" }).clicked() {
                    toggles.push(dir.to_path_buf());
                }
            } else {
                ui.add_space(20.0);
            }
            let selected = self.current == dir;
            if ui.selectable_label(selected, name).clicked() {
                *navigate = Some(dir.to_path_buf());
            }
            if self.model.is_pending(dir) {
                ui.spinner();
            }
        });

        if open {
            for &i in self.model.children_of(dir) {
                let (child, _) = self.model.entry(i);
                if self.model.has_children(child) {
                    self.tree_row(ui, child, depth + 1, navigate, toggles);
                }
            }
        }
    }

    /// Children of the current directory, largest first.
    fn table(&mut self, ui: &mut egui::Ui) {
        let current = self.current.clone();
        let indices: Vec<u32> = self.model.children_of(&current).to_vec();
        if indices.is_empty() {
            ui.label(if self.scanning() {
                "スキャン中…"
            } else {
                "表示するものがありません"
            });
            return;
        }
        let total = self.model.total_of(&current).physical.max(1);
        let mut navigate = None;

        TableBuilder::new(ui)
            .striped(true)
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(egui_extras::Column::auto().at_least(200.0))
            .column(egui_extras::Column::auto().at_least(90.0))
            .column(egui_extras::Column::remainder())
            .header(20.0, |mut header| {
                for title in ["名前", "ファイル数", "サイズ(物理 / 論理)"] {
                    header.col(|ui| {
                        ui.label(RichText::new(title).strong());
                    });
                }
            })
            .body(|body| {
                body.rows(22.0, indices.len(), |mut row| {
                    let (path, stat) = self.model.entry(indices[row.index()]);
                    let is_dir = self.model.has_children(path);
                    let name = path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    let (physical, logical, files) = (stat.physical, stat.logical, stat.files);
                    let path = path.to_path_buf();
                    row.col(|ui| {
                        if is_dir {
                            if ui.link(format!("📁 {name}")).clicked() {
                                navigate = Some(path.clone());
                            }
                        } else {
                            ui.label(name);
                        }
                    });
                    row.col(|ui| {
                        ui.monospace(files.to_string());
                    });
                    row.col(|ui| {
                        let frac = (physical as f64 / total as f64) as f32;
                        ui.add(egui::ProgressBar::new(frac).show_percentage().text(format!(
                            "{} / {}",
                            format_size(physical, BINARY),
                            format_size(logical, BINARY)
                        )));
                    });
                });
            });

        if let Some(p) = navigate {
            self.current = p;
            self.expanded.insert(self.current.clone());
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();

        egui::TopBottomPanel::top("controls").show(ctx, |ui| {
            self.controls(ui);
            self.status(ui);
        });

        egui::SidePanel::left("tree")
            .resizable(true)
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.heading("ディレクトリツリー");
                // Without this the panel simply clipped everything past the
                // window height, with no way to reach it.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.tree(ui));
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.breadcrumb(ui);
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| self.table(ui));
        });

        if self.scanning() {
            ctx.request_repaint_after(std::time::Duration::from_millis(REFRESH_MS));
        }
    }
}
