//! Desktop controls and bounded result rendering over the shared core scan API.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Instant,
};

use egui::{Align, Layout, RichText};
use egui_extras::TableBuilder;
use humansize::{format_size, BINARY};

use crate::{fonts, i18n::Lang, scan};

mod export;
/// Bound display ingestion by nodes and elapsed time, not subtree message count.
const NODES_PER_FRAME: usize = 1024;
const INGEST_MS: u64 = 3;
/// How often to wake while a scan is running, so rows appear as they land.
const REFRESH_MS: u64 = 250;
/// Bound recursive tree rendering. Deeper directories remain accessible in the table.
const MAX_TREE_DEPTH: usize = 64;

/// What the scan is doing. Kept as data and worded at paint time, so switching
/// the language also rewords a message that is already on screen.
#[derive(Debug)]
enum Status {
    Idle,
    Scanning,
    StartFailed(anyhow::Error),
    MftBatch,
    Done,
    Unreadable(u64),
    Cancelling,
    Cancelled,
    BadMessage,
    Failed(String),
    Crashed,
    Disconnected,
}

#[derive(Debug, Default)]
enum ExportStatus {
    #[default]
    None,
    Saving,
    Cancelling,
    StartFailed(String),
    Done(export::Outcome),
}

pub struct App {
    lang: Lang,
    root_input: String,
    params: scan::Params,
    scan: Option<scan::Handle>,
    model: scan::Model,
    current: PathBuf,
    expanded: HashSet<PathBuf>,
    started_at: Option<Instant>,
    finished_in: Option<f64>,
    state: Status,
    complete: bool,
    result_size_mode: u8,
    cancelling: bool,
    errors: u64,
    error_details: Vec<String>,
    export_message: ExportStatus,
    export_task: Option<export::Task>,
    sort: u8,
    pending: Option<scan::Msg>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            lang: Lang::Ja,
            root_input: String::new(),
            params: scan::Params::default(),
            scan: None,
            model: scan::Model::default(),
            current: PathBuf::new(),
            expanded: HashSet::new(),
            started_at: None,
            finished_in: None,
            state: Status::Idle,
            complete: false,
            result_size_mode: 0,
            cancelling: false,
            errors: 0,
            error_details: Vec::new(),
            export_message: ExportStatus::None,
            export_task: None,
            sort: 0,
            pending: None,
        }
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let lang = Lang::detect();
        fonts::configure_fonts(&cc.egui_ctx, lang);
        let mut visuals = egui::Visuals::light();
        visuals.selection.bg_fill = egui::Color32::from_rgb(55, 115, 94);
        cc.egui_ctx.set_visuals(visuals);
        Self {
            lang,
            ..Self::default()
        }
    }
    fn start_scan(&mut self, root: PathBuf, ctx: &egui::Context) {
        if self.scanning() || self.export_task.is_some() {
            return;
        }
        let context = ctx.clone();
        match scan::start(
            root.clone(),
            self.params.clone(),
            std::sync::Arc::new(move || context.request_repaint()),
        ) {
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
                self.export_message = ExportStatus::None;
                self.state = Status::Scanning;
                self.pending = None;
                ctx.request_repaint();
            }
            Err(error) => {
                self.state = Status::StartFailed(error);
                self.complete = false;
            }
        }
    }
    fn scanning(&self) -> bool {
        self.scan.is_some()
    }
    fn drain(&mut self, ctx: &egui::Context) {
        let Some(handle) = &self.scan else { return };
        let mut done = false;
        let frame_start = Instant::now();
        let mut work = 0;
        while work < NODES_PER_FRAME && frame_start.elapsed().as_millis() < u128::from(INGEST_MS) {
            let message = match self.pending.take() {
                Some(message) => Ok(message),
                None => handle.rx.try_recv(),
            };
            match message {
                Ok(scan::Msg::Update(update)) => {
                    let cost = update.work();
                    if work + cost > NODES_PER_FRAME {
                        self.pending = Some(scan::Msg::Update(update));
                        break;
                    }
                    work += cost;
                    self.model.apply(update);
                }
                Ok(scan::Msg::Core(hyperdu_core::ScanEvent::BatchFallback { .. })) => {
                    self.state = Status::MftBatch
                }
                Ok(scan::Msg::Core(hyperdu_core::ScanEvent::Finished)) => {
                    self.errors = handle.errors.load(Ordering::Relaxed);
                    self.complete = self.errors == 0 && !self.cancelling;
                    self.state = if self.cancelling {
                        Status::Cancelled
                    } else if self.complete {
                        Status::Done
                    } else {
                        Status::Unreadable(self.errors)
                    };
                    done = true;
                    break;
                }
                Ok(scan::Msg::Core(hyperdu_core::ScanEvent::Cancelled)) => {
                    self.state = Status::Cancelled;
                    done = true;
                    break;
                }
                Ok(scan::Msg::Core(_)) => {
                    self.state = Status::BadMessage;
                    done = true;
                    break;
                }
                Ok(scan::Msg::Failed(error)) => {
                    self.state = Status::Failed(error);
                    done = true;
                    break;
                }
                Ok(scan::Msg::Crashed) => {
                    self.state = Status::Crashed;
                    done = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.state = Status::Disconnected;
                    done = true;
                    break;
                }
            }
        }
        if work > 0 || self.pending.is_some() {
            ctx.request_repaint();
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
            self.pending = None;
            self.cancelling = false;
        }
    }
    fn controls(&mut self, ui: &mut egui::Ui) {
        let running = self.scanning();
        ui.horizontal_wrapped(|ui| {
            ui.heading("HyperDU");
            ui.label(
                self.lang
                    .t("ディスクの使用状況", "Disk usage", "磁盘使用情况"),
            );
            if running
                && ui
                    .add_enabled(
                        !self.cancelling,
                        egui::Button::new(self.lang.t("中止", "Cancel", "中止")),
                    )
                    .clicked()
            {
                if let Some(handle) = &self.scan {
                    handle.cancel();
                    self.cancelling = true;
                    self.state = Status::Cancelling;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let before = self.lang;
                egui::ComboBox::from_id_salt("language")
                    .selected_text(self.lang.name())
                    .show_ui(ui, |ui| {
                        for lang in Lang::ALL {
                            ui.selectable_value(&mut self.lang, lang, lang.name());
                        }
                    });
                if self.lang != before {
                    // Font order follows the language: shared CJK characters
                    // take Chinese or Japanese glyph shapes accordingly.
                    fonts::configure_fonts(ui.ctx(), self.lang);
                }
            });
        });
        let l = self.lang;
        ui.add_enabled_ui(!running && self.export_task.is_none(),|ui|{
            ui.horizontal_wrapped(|ui|{
                ui.label(l.t("対象フォルダ", "Folder", "目标文件夹"));
ui.add(egui::TextEdit::singleline(&mut self.root_input).desired_width(430.0).hint_text(l.t("C:\\ またはフォルダのパス", "C:\\ or a folder path", "C:\\ 或文件夹路径")));
                if ui.button(l.t("選択…", "Browse…", "选择…")).clicked() {if let Some(path)=rfd::FileDialog::new().pick_folder() {self.root_input=path.display().to_string();
}}
                if ui.add_enabled(!self.root_input.trim().is_empty(),egui::Button::new(l.t("スキャン開始", "Start scan", "开始扫描"))).clicked() {self.start_scan(PathBuf::from(self.root_input.trim()), ui.ctx());
}
            });
            ui.horizontal_wrapped(|ui|{
                ui.label(l.t("走査モード", "Scan mode", "扫描模式"));
                ui.selectable_value(&mut self.params.mode,hyperdu_core::ScanMode::Interactive,l.t("インタラクティブ", "Interactive", "交互式"));
                ui.selectable_value(&mut self.params.mode,hyperdu_core::ScanMode::Batch,l.t("一括", "Batch", "批量"));
                ui.weak(l.t("インタラクティブ: 子フォルダの結果を順次表示", "Interactive: shows each subfolder as it completes", "交互式：按子文件夹依次显示结果"));
            });
            ui.weak(l.t("設定の変更は次回のスキャンに適用されます。", "Setting changes apply to the next scan.", "设置更改将在下次扫描时生效。"));
            egui::CollapsingHeader::new(l.t("詳細設定 — 除外・サイズ・リンク・速度", "Advanced — exclusions, sizes, links, speed", "高级设置 — 排除、大小、链接、速度")).id_salt("advanced").show(ui,|ui|{
                egui::ScrollArea::vertical().id_salt("settings_scroll").max_height(230.0).show(ui,|ui|{
                    ui.horizontal_wrapped(|ui|{
                        ui.label(l.t("最小ファイルサイズ", "Minimum file size", "最小文件大小"));
ui.add(egui::TextEdit::singleline(&mut self.params.min_size).desired_width(100.0));
ui.weak(l.t("例: 10 MiB", "e.g. 10 MiB", "例如：10 MiB"));
                        ui.label(l.t("走査を打ち切る深さ", "Stop scanning below depth", "扫描截止深度"));
ui.add(egui::DragValue::new(&mut self.params.prune_depth).range(0..=u32::MAX));
ui.weak(l.t("0 = 無制限。1以上にすると、その下は集計されず合計が小さく出ます", "0 = unlimited. At 1 or more nothing below is counted, so totals read smaller", "0 = 不限制。设为 1 或以上时不统计其下层，合计会偏小"));
                    });
                    ui.label(l.t("除外パターン (1行に1つ。空欄 = 除外なし)", "Exclude patterns (one per line; empty = none)", "排除模式（每行一个；留空 = 不排除）"));
                    ui.columns(3,|columns|{
                        columns[0].label(l.t("名前・パスに含まれる文字列", "Text in a name or path", "名称或路径中包含的文字"));
columns[0].add(egui::TextEdit::multiline(&mut self.params.exclude).desired_rows(2).desired_width(f32::INFINITY));
                        columns[1].label("glob");
columns[1].add(egui::TextEdit::multiline(&mut self.params.glob).desired_rows(2).desired_width(f32::INFINITY));
                        columns[2].label(l.t("正規表現", "Regular expression", "正则表达式"));
columns[2].add(egui::TextEdit::multiline(&mut self.params.regex).desired_rows(2).desired_width(f32::INFINITY));
                    });
                    ui.horizontal_wrapped(|ui|{ui.checkbox(&mut self.params.follow_links,l.t("リンク先を走査", "Follow links", "跟随链接"));
ui.checkbox(&mut self.params.count_hardlinks,l.t("ハードリンクを個別に数える", "Count every hard link", "分别计算硬链接"));
ui.checkbox(&mut self.params.one_file_system,l.t("同じファイルシステムのみ", "Stay on one file system", "仅限同一文件系统"));
});
                    ui.horizontal_wrapped(|ui|{
                        ui.label(l.t("サイズ計算", "Size", "大小计算"));
ui.selectable_value(&mut self.params.size_mode,0,l.t("物理 + 論理", "Physical + logical", "物理 + 逻辑"));
ui.selectable_value(&mut self.params.size_mode,1,l.t("論理のみ", "Logical only", "仅逻辑"));
ui.selectable_value(&mut self.params.size_mode,2,l.t("概算 (高速)", "Approximate (fast)", "估算（快速）"));
                    });
                    if self.params.size_mode!=0 {ui.colored_label(egui::Color32::DARK_RED,l.t("論理のみ・概算では、物理欄も代替値です。概算はファイルサイズの正確性を優先しません。", "With logical only or approximate, the physical column is a substitute too. Approximate does not aim for accurate file sizes.", "仅逻辑或估算模式下，物理列也是替代值。估算模式不保证文件大小准确。"));
}
                    ui.horizontal_wrapped(|ui|{
                        ui.label(l.t("スレッド数 (0 = 自動)", "Threads (0 = auto)", "线程数（0 = 自动）"));
ui.add(egui::DragValue::new(&mut self.params.threads).range(0..=256));
                        ui.label("I/O");
ui.selectable_value(&mut self.params.io_profile,hyperdu_core::IoProfile::Balanced,l.t("標準", "Balanced", "标准"));
ui.selectable_value(&mut self.params.io_profile,hyperdu_core::IoProfile::Throughput,l.t("速度優先", "Throughput", "速度优先"));
ui.selectable_value(&mut self.params.io_profile,hyperdu_core::IoProfile::Gentle,l.t("低負荷", "Gentle", "低负载"));
                    });
                    ui.horizontal_wrapped(|ui|{
                        ui.label(l.t("先読み", "Prefetch", "预读"));
ui.selectable_value(&mut self.params.prefetch,0,l.t("自動", "Auto", "自动"));
ui.selectable_value(&mut self.params.prefetch,1,l.t("有効", "On", "启用"));
ui.selectable_value(&mut self.params.prefetch,2,l.t("無効", "Off", "禁用"));
                        ui.label(l.t("巨大フォルダの分割間隔 (0 = 無効)", "Split large folders every N entries (0 = off)", "大文件夹拆分间隔（0 = 禁用）"));
ui.add(egui::DragValue::new(&mut self.params.dir_yield).range(0..=1_000_000));
                    });
                    #[cfg(windows)] {
                        ui.checkbox(&mut self.params.use_mft,l.t("NTFS MFT直接走査を試す", "Try a direct NTFS MFT scan", "尝试直接扫描 NTFS MFT"));
                        ui.weak(l.t("MFTは管理者権限とNTFSボリュームルートが必要です。対応できない設定では通常走査に戻り、MFT成功時は結果を一括表示します。", "MFT needs administrator rights and an NTFS volume root. Unsupported settings fall back to a normal scan; a successful MFT scan shows its result in one batch.", "MFT 需要管理员权限和 NTFS 卷根目录。不支持的设置会回退到普通扫描；MFT 成功时会一次性显示结果。"));
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
            let l = self.lang;
            ui.strong(self.status_text());
            if self.result_size_mode == 2 {
                ui.colored_label(
                    egui::Color32::DARK_RED,
                    l.t("概算結果", "Approximate result", "估算结果"),
                );
            } else if self.result_size_mode == 1 {
                ui.label(l.t(
                    "物理欄は論理サイズの代替値",
                    "Physical column shows the logical size",
                    "物理列为逻辑大小的替代值",
                ));
            }
            if self.model.entry_count() > 0 {
                let n = self.model.entry_count();
                ui.weak(match l {
                    Lang::Ja => format!("{n} ディレクトリ"),
                    Lang::En => format!("{n} directories"),
                    Lang::Zh => format!("{n} 个目录"),
                });
            }
            if let Some(start) = self.started_at {
                let seconds = self
                    .finished_in
                    .unwrap_or_else(|| start.elapsed().as_secs_f64());
                let total = self.model.total_of(&self.model.root);
                let (files, errors) = (total.files, self.errors);
                let logical = format_size(total.logical, BINARY);
                let physical = format_size(total.physical, BINARY);
                ui.label(match l {
                    Lang::Ja => format!(
                        "{seconds:.2} 秒 · {files} files · 論理 {logical} · 物理 {physical} · エラー {errors}"
                    ),
                    Lang::En => format!(
                        "{seconds:.2} s · {files} files · logical {logical} · physical {physical} · errors {errors}"
                    ),
                    Lang::Zh => format!(
                        "{seconds:.2} 秒 · {files} 个文件 · 逻辑 {logical} · 物理 {physical} · 错误 {errors}"
                    ),
                });
                if let Some(handle) = &self.scan {
                    let n = handle.files_seen.load(Ordering::Relaxed).max(total.files);
                    ui.label(match l {
                        Lang::Ja => format!("走査済み {n}"),
                        Lang::En => format!("Scanned {n}"),
                        Lang::Zh => format!("已扫描 {n}"),
                    });
                    let pending = self.model.pending_count();
                    if pending > 0 {
                        ui.label(match l {
                            Lang::Ja => format!("残り {pending} フォルダ"),
                            Lang::En => format!("{pending} folders remaining"),
                            Lang::Zh => format!("剩余 {pending} 个文件夹"),
                        });
                    }
                }
            }
        });
        if !self.error_details.is_empty() {
            let title = self.lang.t(
                "読み取りエラー (最大20件)",
                "Read errors (up to 20)",
                "读取错误（最多 20 条）",
            );
            egui::CollapsingHeader::new(title)
                .id_salt("read_errors")
                .show(ui, |ui| {
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
                    self.complete && !self.scanning() && self.export_task.is_none(),
                    egui::Button::new(self.lang.t("JSONへ保存", "Save JSON", "保存为 JSON")),
                )
                .clicked()
            {
                self.export(false, ui.ctx());
            }
            if ui
                .add_enabled(
                    self.complete && !self.scanning() && self.export_task.is_none(),
                    egui::Button::new(self.lang.t("CSVへ保存", "Save CSV", "保存为 CSV")),
                )
                .clicked()
            {
                self.export(true, ui.ctx());
            }
            if let Some(task) = &self.export_task {
                ui.spinner();
                if ui
                    .button(self.lang.t("保存を中止", "Cancel save", "取消保存"))
                    .clicked()
                {
                    task.cancel();
                    self.export_message = ExportStatus::Cancelling;
                }
            }
            ui.label(self.export_text());
        });
    }
    fn export(&mut self, csv: bool, ctx: &egui::Context) {
        if !self.complete || self.scanning() || self.export_task.is_some() {
            return;
        }
        let format = if csv {
            export::Format::Csv
        } else {
            export::Format::Json
        };
        let context = ctx.clone();
        match export::Task::start(
            self.model.export_snapshot(),
            format,
            std::sync::Arc::new(move || context.request_repaint()),
        ) {
            Ok(task) => {
                self.export_task = Some(task);
                self.export_message = ExportStatus::Saving;
            }
            Err(error) => self.export_message = ExportStatus::StartFailed(error.to_string()),
        }
    }
    fn drain_export(&mut self) {
        let outcome = self.export_task.as_ref().and_then(export::Task::poll);
        if let Some(outcome) = outcome {
            self.export_task = None;
            self.export_message = ExportStatus::Done(outcome);
        }
    }
    fn status_text(&self) -> String {
        let l = self.lang;
        let failed = |detail: &str| match l {
            Lang::Ja => format!("スキャン失敗: {detail}"),
            Lang::En => format!("Scan failed: {detail}"),
            Lang::Zh => format!("扫描失败：{detail}"),
        };
        match &self.state {
            Status::Idle => l
                .t(
                    "フォルダを選んでスキャンを開始してください",
                    "Choose a folder and start a scan",
                    "请选择文件夹并开始扫描",
                )
                .into(),
            Status::Scanning => l.t("スキャン中", "Scanning", "正在扫描").into(),
            Status::StartFailed(error) => {
                let detail = match error.downcast_ref::<scan::SizeError>() {
                    Some(scan::SizeError::Format) => l
                        .t(
                            "サイズは整数と B / KB / KiB / MB / MiB / GB / GiB で入力してください",
                            "Enter the size as an integer with B / KB / KiB / MB / MiB / GB / GiB",
                            "请以整数加 B / KB / KiB / MB / MiB / GB / GiB 输入大小",
                        )
                        .to_string(),
                    Some(scan::SizeError::TooLarge) => l
                        .t(
                            "サイズが大きすぎます",
                            "The size is too large",
                            "大小超出范围",
                        )
                        .to_string(),
                    None => error.to_string(),
                };
                match l {
                    Lang::Ja => format!("開始できません: {detail} — 表示中の結果は前回の走査です"),
                    Lang::En => format!(
                        "Cannot start: {detail} — the results shown are from the previous scan"
                    ),
                    Lang::Zh => format!("无法开始：{detail} — 当前显示的是上一次扫描的结果"),
                }
            }
            Status::MftBatch => l
                .t(
                    "MFT: 一括結果を受信中",
                    "MFT: receiving the batch result",
                    "MFT：正在接收批量结果",
                )
                .into(),
            Status::Done => l.t("完了", "Done", "完成").into(),
            Status::Unreadable(n) => match l {
                Lang::Ja => format!("一部を読み取れませんでした ({n} 件)"),
                Lang::En => format!("Some entries could not be read ({n})"),
                Lang::Zh => format!("部分条目无法读取（{n} 个）"),
            },
            Status::Cancelling => l.t("中断処理中…", "Cancelling…", "正在中止…").into(),
            Status::Cancelled => l
                .t(
                    "中断しました — 表示は部分結果です",
                    "Cancelled — the results shown are partial",
                    "已中止 — 显示的是部分结果",
                )
                .into(),
            Status::BadMessage => l
                .t(
                    "表示データの受信形式が不正です",
                    "Display data arrived in an unexpected form",
                    "收到的显示数据格式无效",
                )
                .into(),
            Status::Failed(error) => failed(error),
            Status::Crashed => failed(l.t(
                "走査スレッドが異常終了しました",
                "the scan thread ended abnormally",
                "扫描线程异常终止",
            )),
            Status::Disconnected => failed(l.t(
                "完了通知なしに接続が終了しました",
                "the connection closed without a completion notice",
                "连接在未收到完成通知的情况下关闭",
            )),
        }
    }
    fn export_text(&self) -> String {
        let l = self.lang;
        match &self.export_message {
            ExportStatus::None => String::new(),
            ExportStatus::Saving => l
                .t(
                    "保存先を選択・保存中…",
                    "Choosing a destination and saving…",
                    "正在选择位置并保存…",
                )
                .into(),
            ExportStatus::Cancelling => l
                .t("保存を中断中…", "Cancelling the save…", "正在取消保存…")
                .into(),
            ExportStatus::StartFailed(error) => match l {
                Lang::Ja => format!("保存を開始できません: {error}"),
                Lang::En => format!("Cannot start saving: {error}"),
                Lang::Zh => format!("无法开始保存：{error}"),
            },
            ExportStatus::Done(export::Outcome::Saved(path)) => {
                let path = path.display();
                match l {
                    Lang::Ja => format!("保存しました: {path}"),
                    Lang::En => format!("Saved: {path}"),
                    Lang::Zh => format!("已保存：{path}"),
                }
            }
            ExportStatus::Done(export::Outcome::Cancelled) => l
                .t("保存を中止しました", "Save cancelled", "已取消保存")
                .into(),
            ExportStatus::Done(export::Outcome::Failed(error)) => match l {
                Lang::Ja => format!("保存できません: {error}"),
                Lang::En => format!("Could not save: {error}"),
                Lang::Zh => format!("无法保存：{error}"),
            },
            ExportStatus::Done(export::Outcome::Crashed) => l
                .t(
                    "保存できません: 保存スレッドが異常終了しました",
                    "Could not save: the save thread ended abnormally",
                    "无法保存：保存线程异常终止",
                )
                .into(),
        }
    }
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
            ui.label(
                self.lang
                    .t("スキャン結果なし", "No scan results", "暂无扫描结果"),
            );
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
            let l = self.lang;
            ui.label(l.t("並び順", "Sort by", "排序"));
            ui.selectable_value(
                &mut self.sort,
                0,
                l.t("物理サイズ", "Physical size", "物理大小"),
            );
            ui.selectable_value(
                &mut self.sort,
                1,
                l.t("論理サイズ", "Logical size", "逻辑大小"),
            );
            ui.selectable_value(&mut self.sort, 2, l.t("ファイル数", "Files", "文件数"));
            ui.selectable_value(&mut self.sort, 3, l.t("名前", "Name", "名称"));
        });
        let direct = self.model.direct_files_of(&current);
        if direct.files > 0 {
            let s = direct;
            let (files, physical, logical) = (
                s.files,
                format_size(s.physical, BINARY),
                format_size(s.logical, BINARY),
            );
            ui.label(match self.lang {
                Lang::Ja => {
                    format!("直下のファイル合計: {files} files · 物理 {physical} / 論理 {logical}")
                }
                Lang::En => {
                    format!("Files directly here: {files} files · physical {physical} / logical {logical}")
                }
                Lang::Zh => {
                    format!("直接包含的文件：{files} 个文件 · 物理 {physical} / 逻辑 {logical}")
                }
            });
        }
        let indices = self.model.children_sorted(&current, self.sort);
        if indices.is_empty() {
            ui.label(if self.scanning() {
                self.lang.t("スキャン中…", "Scanning…", "正在扫描…")
            } else {
                self.lang.t(
                    "表示するものがありません",
                    "Nothing to show",
                    "没有可显示的内容",
                )
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
                for title in [
                    self.lang.t("名前", "Name", "名称"),
                    self.lang.t("ファイル数", "Files", "文件数"),
                    self.lang.t(
                        "サイズ(物理 / 論理)",
                        "Size (physical / logical)",
                        "大小（物理 / 逻辑）",
                    ),
                ] {
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

impl App {
    fn show_ui(&mut self, ctx: &egui::Context) {
        self.drain(ctx);
        self.drain_export();
        egui::TopBottomPanel::top("controls").show(ctx, |ui| {
            self.controls(ui);
            self.status(ui);
        });
        egui::SidePanel::left("tree")
            .resizable(true)
            .default_width(250.0)
            .show(ctx, |ui| {
                let l = self.lang;
                ui.heading(l.t("ディレクトリ", "Directories", "目录"));
                ui.weak(l.t(
                    "各階層500件・全体1500行まで。全件は右の一覧で確認できます。",
                    "Up to 500 per level and 1,500 rows in all. The list on the right shows everything.",
                    "每层最多 500 项，共 1500 行。完整内容见右侧列表。",
                ));
                // Without this the panel simply clipped everything past the
                // window height, with no way to reach it.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.tree(ui));
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.model.root.as_os_str().is_empty() {
                ui.add_space(60.0);
                let l = self.lang;
                ui.heading(l.t(
                    "容量の内訳を調べる",
                    "See what is using the space",
                    "查看空间占用",
                ));
                ui.label(l.t(
                    "対象フォルダを選び、スキャン開始を押してください。",
                    "Choose a folder and press Start scan.",
                    "请选择目标文件夹，然后点击开始扫描。",
                ));
                ui.label(l.t(
                    "インタラクティブモードなら、完了した子フォルダから閲覧できます。",
                    "In interactive mode, subfolders can be browsed as they complete.",
                    "在交互式模式下，可以先浏览已完成的子文件夹。",
                ));
                return;
            }
            self.breadcrumb(ui);
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| self.table(ui));
        });
        if self.scanning() || self.export_task.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(REFRESH_MS));
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.show_ui(ctx);
    }
}

#[cfg(test)]
mod ui_tests;

#[cfg(test)]
mod tests {
    use hyperdu_core::{ScanEvent, Stat};

    use super::*;

    #[test]
    fn start_sets_working_state_before_any_result_is_drained() {
        let root = tempfile::tempdir().unwrap();
        let mut app = App::default();
        app.start_scan(root.path().to_path_buf(), &egui::Context::default());
        assert!(app.scanning() && !app.complete);
        assert!(app.started_at.is_some() && app.finished_in.is_none());
        assert!(matches!(app.state, Status::Scanning));
        assert_eq!(app.model.entry_count(), 0);
    }

    #[test]
    fn large_result_is_bounded_and_finished_waits_for_all_chunks() {
        let (tx, handle) = scan::test_handle(8);
        let root = PathBuf::from("root");
        for chunk in 0..5 {
            let nodes = (0..scan::CHUNK_NODES)
                .map(|n| {
                    let index = (chunk * scan::CHUNK_NODES + n) as u32;
                    (index, root.join(index.to_string()), Stat::default(), false)
                })
                .collect();
            tx.send(scan::Msg::Update(scan::Update::Nodes(nodes)))
                .unwrap();
        }
        tx.send(scan::Msg::Core(ScanEvent::Finished)).unwrap();
        let mut app = App {
            scan: Some(handle),
            model: scan::Model::new(root),
            ..App::default()
        };
        let ctx = egui::Context::default();
        app.drain(&ctx);
        assert!(app.model.entry_count() <= NODES_PER_FRAME);
        assert!(app.scanning() && !app.complete);
        for _ in 0..20 {
            if !app.scanning() {
                break;
            }
            app.drain(&ctx);
        }
        assert_eq!(app.model.entry_count(), 5 * scan::CHUNK_NODES);
        assert!(app.complete && !app.scanning());
    }

    #[test]
    fn cancel_overrides_queued_finished_and_replaced_handles_isolate_events() {
        let (old, handle) = scan::test_handle(1);
        let (new, replacement) = scan::test_handle(1);
        let mut app = App {
            scan: Some(handle),
            ..App::default()
        };
        app.scan = Some(replacement);
        assert!(old.send(scan::Msg::Failed("stale".into())).is_err());
        new.send(scan::Msg::Core(ScanEvent::Finished)).unwrap();
        app.cancelling = true;
        app.drain(&egui::Context::default());
        assert!(!app.complete && !app.scanning());
        assert!(matches!(app.state, Status::Cancelled));
    }
}
