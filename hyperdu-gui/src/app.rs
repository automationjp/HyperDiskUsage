//! Desktop controls and bounded result rendering over the shared core scan API.

use crate::{fonts, scan};
use egui::{Align, Layout, RichText};
use egui_extras::TableBuilder;
use humansize::{format_size, BINARY};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Instant,
};
/// Messages drained per frame. Each one folds a subtree into the model, which
/// re-sorts the affected parents, so an unbounded drain would let a burst of
/// small subtrees stall a frame.
const MSGS_PER_FRAME: usize = 32;
/// How often to wake while a scan is running, so rows appear as they land.
const REFRESH_MS: u64 = 250;
/// Bound recursive tree rendering. Deeper directories remain accessible in the table.
const MAX_TREE_DEPTH: usize = 64;
pub struct App {
    root_input: String,
    params: scan::Params,
    scan: Option<scan::Handle>,
    model: scan::Model,
    current: PathBuf,
    expanded: HashSet<PathBuf>,
    started_at: Option<Instant>,
    finished_in: Option<f64>,
    state: String,
    complete: bool,
    result_size_mode: u8,
    cancelling: bool,
    errors: u64,
    error_details: Vec<String>,
    export_message: String,
    sort: u8,
    table_sort: u8,
    table_revision: u64,
    table_root: PathBuf,
    table_indices: Vec<u32>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            root_input: String::new(),
            params: scan::Params::default(),
            scan: None,
            model: scan::Model::default(),
            current: PathBuf::new(),
            expanded: HashSet::new(),
            started_at: None,
            finished_in: None,
            state: "フォルダを選んでスキャンを開始してください".into(),
            complete: false,
            result_size_mode: 0,
            cancelling: false,
            errors: 0,
            error_details: Vec::new(),
            export_message: String::new(),
            sort: 0,
            table_sort: 0,
            table_revision: u64::MAX,
            table_root: PathBuf::new(),
            table_indices: Vec::new(),
        }
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        fonts::configure_fonts(&cc.egui_ctx);
        let mut visuals = egui::Visuals::light();
        visuals.selection.bg_fill = egui::Color32::from_rgb(55, 115, 94);
        cc.egui_ctx.set_visuals(visuals);
        Self::default()
    }
    fn start_scan(&mut self, root: PathBuf) {
        if self.scanning() {
            return;
        }
        match scan::start(root.clone(), self.params.clone()) {
            Ok(handle) => {
                self.model = scan::Model::new(root.clone());
                self.current = root.clone();
                self.expanded.clear();
                self.expanded.insert(root);
                self.started_at = Some(Instant::now());
                self.finished_in = None;
                self.scan = Some(handle);
                self.complete = false;
                self.result_size_mode = self.params.size_mode;
                self.cancelling = false;
                self.errors = 0;
                self.error_details.clear();
                self.export_message.clear();
                self.state = "スキャン中".into();
                self.table_revision = u64::MAX;
            }
            Err(error) => {
                self.state = format!("開始できません: {error} — 表示中の結果は前回の走査です");
                self.complete = false;
            }
        }
    }
    fn scanning(&self) -> bool {
        self.scan.is_some()
    }
    fn drain(&mut self) {
        let Some(handle) = &self.scan else { return };
        let mut done = false;
        for _ in 0..MSGS_PER_FRAME {
            match handle.rx.try_recv() {
                Ok(scan::Msg::Core(hyperdu_core::ScanEvent::RootListed {
                    directories,
                    direct_files,
                    ..
                })) => {
                    self.model.direct_files = direct_files;
                    self.model.expect(directories);
                }
                Ok(scan::Msg::Core(hyperdu_core::ScanEvent::ChildCompleted { root, map })) => {
                    self.model.absorb(&root, map)
                }
                Ok(scan::Msg::Core(hyperdu_core::ScanEvent::BatchCompleted { map })) => {
                    self.model.replace_batch(map)
                }
                Ok(scan::Msg::Core(hyperdu_core::ScanEvent::BatchFallback { .. })) => {
                    self.state = "MFT: 一括結果を受信中".into()
                }
                Ok(scan::Msg::Core(hyperdu_core::ScanEvent::Finished)) => {
                    self.errors = handle.errors.load(Ordering::Relaxed);
                    self.complete = self.errors == 0;
                    self.state = if self.complete {
                        "完了".into()
                    } else {
                        format!("一部を読み取れませんでした ({} 件)", self.errors)
                    };
                    done = true;
                    break;
                }
                Ok(scan::Msg::Core(hyperdu_core::ScanEvent::Cancelled)) => {
                    self.state = "中断しました — 表示は完了済みフォルダの部分結果です".into();
                    done = true;
                    break;
                }
                Ok(scan::Msg::Failed(error)) => {
                    self.state = format!("スキャン失敗: {error}");
                    done = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.state = "スキャン失敗: 完了通知なしに接続が終了しました".into();
                    done = true;
                    break;
                }
            }
        }
        self.errors = handle.errors.load(Ordering::Relaxed);
        self.error_details = handle
            .error_details
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if done {
            self.finished_in = self.started_at.map(|t| t.elapsed().as_secs_f64());
            self.scan = None;
            self.cancelling = false;
        }
    }
    fn controls(&mut self, ui: &mut egui::Ui) {
        let running = self.scanning();
        ui.horizontal_wrapped(|ui| {
            ui.heading("HyperDU");
            ui.label("ディスクの使用状況");
            if running
                && ui
                    .add_enabled(!self.cancelling, egui::Button::new("中止"))
                    .clicked()
            {
                if let Some(handle) = &self.scan {
                    handle.cancel();
                    self.cancelling = true;
                    self.state = "中断処理中…".into();
                }
            }
        });
        ui.add_enabled_ui(!running,|ui|{
            ui.horizontal_wrapped(|ui|{
                ui.label("対象フォルダ");
ui.add(egui::TextEdit::singleline(&mut self.root_input).desired_width(430.0).hint_text("C:\\ またはフォルダのパス"));
                if ui.button("選択…").clicked() {if let Some(path)=rfd::FileDialog::new().pick_folder() {self.root_input=path.display().to_string();
}}
                if ui.add_enabled(!self.root_input.trim().is_empty(),egui::Button::new("スキャン開始")).clicked() {self.start_scan(PathBuf::from(self.root_input.trim()));
}
            });
            ui.horizontal_wrapped(|ui|{
                ui.label("走査モード");
                ui.selectable_value(&mut self.params.mode,hyperdu_core::ScanMode::Interactive,"インタラクティブ");
                ui.selectable_value(&mut self.params.mode,hyperdu_core::ScanMode::Batch,"一括");
                ui.weak("インタラクティブ: 子フォルダの結果を順次表示");
            });
            ui.weak("設定の変更は次回のスキャンに適用されます。");
            egui::CollapsingHeader::new("詳細設定 — 除外・サイズ・リンク・速度").show(ui,|ui|{
                egui::ScrollArea::vertical().id_salt("settings_scroll").max_height(230.0).show(ui,|ui|{
                    ui.horizontal_wrapped(|ui|{
                        ui.label("最小ファイルサイズ");
ui.add(egui::TextEdit::singleline(&mut self.params.min_size).desired_width(100.0));
ui.weak("例: 10 MiB");
                        ui.label("深さ上限");
ui.add(egui::DragValue::new(&mut self.params.max_depth).range(0..=u32::MAX));
ui.weak("0 = 無制限");
                    });
                    ui.label("除外パターン (1行に1つ。空欄 = 除外なし)");
                    ui.columns(3,|columns|{
                        columns[0].label("名前・パスに含まれる文字列");
columns[0].add(egui::TextEdit::multiline(&mut self.params.exclude).desired_rows(2).desired_width(f32::INFINITY));
                        columns[1].label("glob");
columns[1].add(egui::TextEdit::multiline(&mut self.params.glob).desired_rows(2).desired_width(f32::INFINITY));
                        columns[2].label("正規表現");
columns[2].add(egui::TextEdit::multiline(&mut self.params.regex).desired_rows(2).desired_width(f32::INFINITY));
                    });
                    ui.horizontal_wrapped(|ui|{ui.checkbox(&mut self.params.follow_links,"リンク先を走査");
ui.checkbox(&mut self.params.count_hardlinks,"ハードリンクを個別に数える");
ui.checkbox(&mut self.params.one_file_system,"同じファイルシステムのみ");
});
                    ui.horizontal_wrapped(|ui|{
                        ui.label("サイズ計算");
ui.selectable_value(&mut self.params.size_mode,0,"物理 + 論理");
ui.selectable_value(&mut self.params.size_mode,1,"論理のみ");
ui.selectable_value(&mut self.params.size_mode,2,"概算 (高速)");
                    });
                    if self.params.size_mode!=0 {ui.colored_label(egui::Color32::DARK_RED,"論理のみ・概算では、物理欄も代替値です。概算はファイルサイズの正確性を優先しません。");
}
                    ui.horizontal_wrapped(|ui|{
                        ui.label("スレッド数 (0 = 自動)");
ui.add(egui::DragValue::new(&mut self.params.threads).range(0..=256));
                        ui.label("I/O");
ui.selectable_value(&mut self.params.io_profile,hyperdu_core::IoProfile::Balanced,"標準");
ui.selectable_value(&mut self.params.io_profile,hyperdu_core::IoProfile::Throughput,"速度優先");
ui.selectable_value(&mut self.params.io_profile,hyperdu_core::IoProfile::Gentle,"低負荷");
                    });
                    ui.horizontal_wrapped(|ui|{
                        ui.label("先読み");
ui.selectable_value(&mut self.params.prefetch,0,"自動");
ui.selectable_value(&mut self.params.prefetch,1,"有効");
ui.selectable_value(&mut self.params.prefetch,2,"無効");
                        ui.label("巨大フォルダの分割間隔 (0 = 無効)");
ui.add(egui::DragValue::new(&mut self.params.dir_yield).range(0..=1_000_000));
                    });
                    #[cfg(windows)] {
                        ui.checkbox(&mut self.params.use_mft,"NTFS MFT直接走査を試す");
                        ui.weak("MFTは管理者権限とNTFSボリュームルートが必要です。対応できない設定では通常走査に戻り、MFT成功時は結果を一括表示します。");
                    }
                });
            });
        });
    }
    fn status(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if self.scanning() {
                ui.spinner();
            }
            ui.strong(&self.state);
            if self.result_size_mode == 2 {
                ui.colored_label(egui::Color32::DARK_RED, "概算結果");
            } else if self.result_size_mode == 1 {
                ui.label("物理欄は論理サイズの代替値");
            }
            if self.model.entry_count() > 0 {
                ui.weak(format!("{} ディレクトリ", self.model.entry_count()));
            }
            if let Some(start) = self.started_at {
                let seconds = self
                    .finished_in
                    .unwrap_or_else(|| start.elapsed().as_secs_f64());
                let total = self.model.total_of(&self.model.root);
                ui.label(format!(
                    "{seconds:.2} 秒 · {} files · 論理 {} · 物理 {} · エラー {}",
                    total.files,
                    format_size(total.logical, BINARY),
                    format_size(total.physical, BINARY),
                    self.errors
                ));
                if let Some(handle) = &self.scan {
                    let n = handle.files_seen.load(Ordering::Relaxed).max(total.files);
                    ui.label(format!("走査済み {n}"));
                    if self.model.pending_count() > 0 {
                        ui.label(format!("残り {} フォルダ", self.model.pending_count()));
                    }
                }
            }
        });
        if !self.error_details.is_empty() {
            egui::CollapsingHeader::new("読み取りエラー (最大20件)").show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(100.0)
                    .show(ui, |ui| {
                        for message in &self.error_details {
                            ui.label(message);
                        }
                    });
            });
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.complete && !self.scanning(),
                    egui::Button::new("JSONへ保存"),
                )
                .clicked()
            {
                self.export(false);
            }
            if ui
                .add_enabled(
                    self.complete && !self.scanning(),
                    egui::Button::new("CSVへ保存"),
                )
                .clicked()
            {
                self.export(true);
            }
            ui.label(&self.export_message);
        });
    }
    fn export(&mut self, csv: bool) {
        let extension = if csv { "csv" } else { "json" };
        if let Some(path) = rfd::FileDialog::new()
            .add_filter(extension, &[extension])
            .set_file_name(format!("hyperdu-report.{extension}"))
            .save_file()
        {
            let rows = self.model.rows();
            let result = std::fs::File::create(&path)
                .map_err(anyhow::Error::from)
                .and_then(|file| {
                    if csv {
                        hyperdu_core::report::write_csv(file, &rows)
                    } else {
                        hyperdu_core::report::write_json(file, &rows)
                    }
                });
            self.export_message = match result {
                Ok(()) => format!("保存しました: {}", path.display()),
                Err(e) => format!("保存できません: {e}"),
            };
        }
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
        let mut budget = 1500;
        self.tree_row(ui, &root, 0, &mut navigate, &mut toggles, &mut budget);
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
        budget: &mut usize,
    ) {
        if depth > MAX_TREE_DEPTH || *budget == 0 {
            return;
        }
        *budget -= 1;
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
            if self.scanning() && self.model.is_pending(dir) {
                ui.spinner();
            }
        });
        if open {
            for &i in self.model.children_of(dir).iter().take(500) {
                let (child, _) = self.model.entry(i);
                self.tree_row(ui, child, depth + 1, navigate, toggles, budget);
            }
        }
    }

    /// Children of the current directory, largest first.
    fn table(&mut self, ui: &mut egui::Ui) {
        let current = self.current.clone();
        ui.horizontal_wrapped(|ui| {
            ui.label("並び順");
            ui.selectable_value(&mut self.sort, 0, "物理サイズ");
            ui.selectable_value(&mut self.sort, 1, "論理サイズ");
            ui.selectable_value(&mut self.sort, 2, "ファイル数");
            ui.selectable_value(&mut self.sort, 3, "名前");
        });
        if self.table_root != current
            || self.table_revision != self.model.revision
            || self.table_sort != self.sort
        {
            self.table_indices = self.model.children_of(&current).to_vec();
            self.table_indices.sort_unstable_by(|a, b| {
                let (pa, sa) = self.model.entry(*a);
                let (pb, sb) = self.model.entry(*b);
                match self.sort {
                    1 => sb.logical.cmp(&sa.logical),
                    2 => sb.files.cmp(&sa.files),
                    3 => pa.cmp(pb),
                    _ => sb.physical.cmp(&sa.physical),
                }
                .then_with(|| pa.cmp(pb))
            });
            self.table_root = current.clone();
            self.table_revision = self.model.revision;
            self.table_sort = self.sort;
        }
        let direct = self.model.direct_files_of(&current);
        if direct.files > 0 {
            let s = direct;
            ui.label(format!(
                "直下のファイル合計: {} files · 物理 {} / 論理 {}",
                s.files,
                format_size(s.physical, BINARY),
                format_size(s.logical, BINARY)
            ));
        }
        let indices = &self.table_indices;
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
                    let name = path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    let (physical, logical, files) = (stat.physical, stat.logical, stat.files);
                    let path = path.to_path_buf();
                    row.col(|ui| {
                        if ui.link(format!("📁 {name}")).clicked() {
                            navigate = Some(path.clone());
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
            .default_width(250.0)
            .show(ctx, |ui| {
                ui.heading("ディレクトリ");
                ui.weak("各階層500件・全体1500行まで。全件は右の一覧で確認できます。");
                // Without this the panel simply clipped everything past the
                // window height, with no way to reach it.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.tree(ui));
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.model.root.as_os_str().is_empty() {
                ui.add_space(60.0);
                ui.heading("容量の内訳を調べる");
                ui.label("対象フォルダを選び、スキャン開始を押してください。");
                ui.label("インタラクティブモードなら、完了した子フォルダから閲覧できます。");
                return;
            }
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
