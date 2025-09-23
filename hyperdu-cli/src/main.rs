use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use anyhow::Result;
use clap::{ArgAction, CommandFactory, Parser, ValueEnum};
use humansize::{format_size, BINARY};

struct KeepAlive {
    done: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl KeepAlive {
    fn start(
        enabled: bool,
        last: Arc<std::sync::Mutex<(u64, std::time::Instant)>>,
    ) -> Option<Self> {
        if !enabled {
            return None;
        }
        let done = Arc::new(AtomicBool::new(false));
        let done_c = done.clone();
        let handle = thread::spawn(move || {
            let keep_secs: u64 = std::env::var("HYPERDU_PROGRESS_KEEPALIVE_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10);
            loop {
                if done_c.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_secs(5));
                if done_c.load(Ordering::Relaxed) {
                    break;
                }
                let (n, t) = *last.lock().unwrap();
                let dt = std::time::Instant::now().duration_since(t).as_secs();
                if dt >= keep_secs {
                    println!("still scanning … processed {n} files (last update {dt}s ago)");
                }
            }
        });
        Some(Self {
            done,
            handle: Some(handle),
        })
    }
}

impl Drop for KeepAlive {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// Cross-platform filesystem stats (total/free) for a given path's volume
fn fs_total_free(path: &Path) -> Option<(u64, u64)> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;

        use windows::{core::PCWSTR, Win32::Storage::FileSystem::GetDiskFreeSpaceExW};
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        // Ensure path ends with backslash and is NUL-terminated for root volume query
        if let Some(&ch) = wide.last() {
            if ch != '\\' as u16 && ch != '/' as u16 {
                wide.push('\\' as u16);
            }
        }
        if *wide.last().unwrap_or(&0) != 0 {
            wide.push(0);
        }
        unsafe {
            let mut free_avail: u64 = 0;
            let mut total: u64 = 0;
            let mut total_free: u64 = 0;
            if GetDiskFreeSpaceExW(
                PCWSTR(wide.as_ptr()),
                Some(&mut free_avail),
                Some(&mut total),
                Some(&mut total_free),
            )
            .is_ok()
            {
                return Some((total, total_free));
            }
        }
        None
    }
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "android",
        target_os = "freebsd"
    ))]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let c = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c.as_ptr(), &mut s as *mut _) };
        if rc == 0 {
            let total = (s.f_blocks as u128).saturating_mul(s.f_frsize as u128) as u64;
            let free = (s.f_bfree as u128).saturating_mul(s.f_frsize as u128) as u64;
            Some((total, free))
        } else {
            None
        }
    }
    #[cfg(not(any(
        target_os = "windows",
        target_os = "linux",
        target_os = "macos",
        target_os = "android",
        target_os = "freebsd"
    )))]
    {
        None
    }
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
enum CompatArg {
    Hyperdu,
    Gnu,
    GnuStrict,
    PosixStrict,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
enum TimeKindArg {
    Mtime,
    Atime,
    Ctime,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
enum PerfArg {
    Turbo,
    Balanced,
    Strict,
}

#[derive(Parser, Debug)]
#[command(
    name = "hyperdu",
    version,
    about = "HyperDU CLI - ultra-fast disk usage analyzer",
    long_about = "超高速なディスク使用量アナライザ HyperDU のCLI。\n\
    パラメータは --help で一覧表示できます。Windows では /help または /? でも同様に表示できます。",
    after_help = "Examples:\n\
      Scan current dir and show top 30\n\
        cargo run -p hyperdu-cli --release -- . --top 30\n\
      Build CLI and GUI binaries\n\
        cargo build -p hyperdu-cli --release\n\
        cargo build -p hyperdu-gui  --release --features mimalloc\n\
      Run GUI\n\
        cargo run -p hyperdu-gui --release\n\
      Print artifacts via helper script\n\
        bash scripts/build_print.sh -p hyperdu-cli --release\n\
      Fast scan profile (Turbo)\n\
        hyperdu --perf turbo <PATH>\n\
      GNU-compatible block reporting\n\
        hyperdu --compat gnu --apparent-size --block-size=1K <PATH>\n\
    "
)]
struct Args {
    /// Root directory(ies) to scan (du互換時は複数可; 省略時は".")
    #[arg(
        value_name = "ROOTS",
        long_help = "スキャン対象のルートディレクトリ（複数指定可）。\n\
        省略時はカレントディレクトリ '.' を使用します。\n\
        互換モード（--compat gnu/posix など）では複数ルートをアルファベット順に列挙します。\n\
        HyperDU標準出力モードでは最初の1つに対して集計レポートを表示します。"
    )]
    roots: Vec<PathBuf>,

    /// Show top N entries by physical size
    #[arg(
        long = "top",
        default_value_t = 30,
        long_help = "上位N件を物理サイズ降順で表示します（HyperDU標準出力時）。\n\
        du互換出力では全行を列挙します。\n\
        注: --apparent-size は上位判定には影響しません（上位判定は常に物理サイズ）。"
    )]
    top: usize,

    /// Comma-separated exclude substrings (e.g. .git,node_modules,target)
    #[arg(
        long,
        long_help = "カンマ区切りの部分一致フィルタ。名前に指定文字列を含むファイル/ディレクトリを除外します。\n\
    例: --exclude .git,node_modules,target"
    )]
    exclude: Option<String>,
    /// Read exclude patterns from file(s), one per line
    #[arg(
        long = "exclude-from",
        long_help = "1行に1パターンを記載した除外パターンファイルを読み込みます。\n\
    行頭接頭辞で種別を指定: 're:' は正規表現、'glob:' はglob、それ以外は部分一致として扱います。\n\
    例: re:^\\.cache$, glob:**/build/**"
    )]
    exclude_from: Vec<PathBuf>,

    /// Maximum depth (0 = unlimited)
    #[arg(
        long = "max-depth",
        default_value_t = 0,
        long_help = "走査の最大深さ。0は無制限。\n\
    1はルート直下のみ、2はその子まで…といった指定になります。"
    )]
    max_depth: u32,

    /// Minimum file size to include in bytes
    #[arg(
        long = "min-file-size",
        default_value_t = 0,
        long_help = "このバイト数未満のファイルは集計から除外します。0は無効。"
    )]
    min_file_size: u64,

    /// Follow symlinks/junctions (use with caution)
    #[arg(
        long = "follow-links",
        action = ArgAction::SetTrue,
        long_help = "シンボリックリンク/ジャンクションに追従します（既定は追従しない）。\n\
    互換出力モードではループ検知を有効化しますが、使用は自己責任でお願いします。"
    )]
    follow_links: bool,
    /// Do not cross filesystem boundaries (mount points)
    #[arg(
        short = 'x',
        long = "one-file-system",
        action = ArgAction::SetTrue,
        long_help = "異なるファイルシステム（マウントポイント）を横断しないようにします。"
    )]
    one_file_system: bool,

    /// Use logical size only (skip physical size queries where possible)
    #[arg(
        long = "logical-only",
        action = ArgAction::SetTrue,
        long_help = "論理サイズのみを使用します。可能な限り物理サイズ取得をスキップするため高速になります。"
    )]
    logical_only: bool,

    /// Approximate file sizes (e.g., 4KiB for regular files) to avoid statx when logical-only
    #[arg(
        long = "approximate",
        action = ArgAction::SetTrue,
        long_help = "概算サイズを使用します（例: 通常ファイルは4KiB相当とみなすなど）。\n\
    compute_physical=false（--logical-onlyや--perf turbo等）時に有効です。"
    )]
    approximate: bool,

    /// Run tuning only (no scan); prints recommended dir_yield_every and exits
    #[arg(
        long = "tune-only",
        action = ArgAction::SetTrue,
        long_help = "スキャンは実行せず、短時間のプローブで適切なdir_yield_every（ディレクトリ分割境界）を推定して表示します。"
    )]
    tune_only: bool,

    /// Tuning time budget in seconds for --tune-only (default 2.0)
    #[arg(
        long = "tune-secs",
        default_value_t = 2.0,
        long_help = "--tune-only の時間予算（秒）。既定は2.0秒。0.1未満を指定した場合は2.0に切り上げます。"
    )]
    tune_secs: f64,

    /// Number of threads (defaults to CPU count)
    #[arg(long, long_help = "スレッド数。省略時は論理CPU数。")]
    threads: Option<usize>,

    /// Write CSV to path
    #[arg(
        long,
        long_help = "CSVを指定パスに出力します（HyperDU標準出力時）。列: path, logical, physical, files"
    )]
    csv: Option<PathBuf>,

    /// Write JSON to path
    #[arg(
        long,
        long_help = "JSONを指定パスに出力します（HyperDU標準出力時）。配列要素に各エントリの統計を出力。"
    )]
    json: Option<PathBuf>,

    /// Classify files by type: basic or deep
    #[arg(
        long = "classify",
        value_name = "MODE",
        long_help = "ファイル種別の分類を実施します: basic|deep。deepは先頭バイトによるMIME推定を行い、若干低速です。"
    )]
    classify: Option<String>,

    /// Write classification JSON report to path
    #[arg(
        long = "class-report",
        value_name = "PATH",
        long_help = "分類結果をJSONへ出力します（--classify 指定時）。"
    )]
    class_report: Option<PathBuf>,

    /// Write classification CSV report to path
    #[arg(
        long = "class-report-csv",
        value_name = "PATH",
        long_help = "分類結果をCSVへ出力します（--classify 指定時）。列: kind, key, files, bytes"
    )]
    class_report_csv: Option<PathBuf>,

    /// Incremental snapshot DB path (sled)
    #[arg(
        long = "incremental-db",
        value_name = "PATH",
        long_help = "インクリメンタルスキャンのスナップショットDB（sled）パス。"
    )]
    incr_db: Option<PathBuf>,

    /// Compute delta against snapshot DB
    #[arg(
        long = "compute-delta",
        action = ArgAction::SetTrue,
        long_help = "スナップショットDBと比較して追加・変更・削除件数を表示します。--incremental-db と併用。"
    )]
    compute_delta: bool,

    /// Update snapshot from current state
    #[arg(
        long = "update-snapshot",
        action = ArgAction::SetTrue,
        long_help = "現在の状態をスナップショットDBへ書き込みます。--incremental-db と併用。"
    )]
    update_snapshot: bool,

    /// Watch filesystem and print changes
    #[arg(
        long = "watch",
        action = ArgAction::SetTrue,
        long_help = "ファイルシステムの変更を監視し、イベントを出力します（Linux/notify対応ビルド時）。"
    )]
    watch: bool,

    /// Print intermittent progress to stderr
    #[arg(
        long,
        action = ArgAction::SetTrue,
        long_help = "処理件数や一部のサンプルパスを定期的にstderrへ表示します。"
    )]
    progress: bool,
    /// Progress emission frequency (files). Default 8192
    #[arg(
        long = "progress-every",
        value_name = "N",
        long_help = "進捗表示の頻度（ファイル件数）。既定は8192。小さい値にすると低速FSでも無反応に見えにくくなります。"
    )]
    progress_every: Option<u64>,

    /// Live-tune threshold (fraction), e.g. 0.05 = 5%
    #[arg(
        long = "tune-threshold",
        default_value_t = 0.05,
        long_help = "ライブチューニングでパラメータ変更を判断する閾値（比率）。0.05は±5%の性能変化を意味します。"
    )]
    tune_threshold: f64,

    /// Print live-tune changes
    #[arg(
        long = "tune-log",
        action = ArgAction::SetTrue,
        long_help = "ライブチューニングによりパラメータが変化した際、その変更内容をstderrに記録します。"
    )]
    tune_log: bool,

    /// Verbose output: also auto-save reports to default filenames
    #[arg(
        long = "verbose",
        short = 'v',
        action = ArgAction::SetTrue,
        long_help = "冗長モード。進捗/ログを詳細化し、JSON/CSV/分類レポートを既定ファイル名でカレントディレクトリ直下に自動出力します（hyperdu-report.json, hyperdu-report.csv, class-report.json, class-report.csv）。"
    )]
    verbose: bool,

    /// Initial io_uring STATX batch size (Linux only; overrides env HYPERDU_STATX_BATCH)
    #[arg(
        long = "uring-batch",
        long_help = "io_uringのSTATXバッチサイズ初期値（Linuxのみ）。環境変数HYPERDU_STATX_BATCHを上書きします。"
    )]
    uring_batch: Option<usize>,

    /// io_uring SQ/CQ depth (Linux only; overrides env HYPERDU_URING_SQ_DEPTH)
    #[arg(
        long = "uring-depth",
        long_help = "io_uringのSQ/CQ深さ（Linuxのみ）。環境変数HYPERDU_URING_SQ_DEPTHを上書きします。"
    )]
    uring_depth: Option<usize>,

    /// Disable io_uring backend (Linux) even if available
    #[arg(
        long = "no-uring",
        action = ArgAction::SetTrue,
        long_help = "Linuxでio_uringバックエンドを無効化します（WSL/ネットワークFSでの安全策）。環境変数HYPERDU_DISABLE_URING=1でも無効化できます。"
    )]
    no_uring: bool,

    /// Enable io_uring SQPOLL (kernel polling) (Linux only)
    #[arg(
        long = "uring-sqpoll",
        action = ArgAction::SetTrue,
        long_help = "io_uringのSQPOLL（カーネル側ポーリング）を有効化します（Linuxのみ）。HYPERDU_URING_SQPOLL=1 相当。"
    )]
    uring_sqpoll: bool,

    /// SQPOLL idle in milliseconds (Linux only)
    #[arg(
        long = "uring-sqpoll-idle-ms",
        value_name = "MS",
        long_help = "SQPOLLスレッドのアイドル時間(ms)。HYPERDU_URING_SQPOLL_IDLE_MS 相当。"
    )]
    uring_sqpoll_idle_ms: Option<u32>,

    /// Pin SQPOLL thread to CPU (Linux only)
    #[arg(
        long = "uring-sqpoll-cpu",
        value_name = "CPU",
        long_help = "SQPOLLスレッドを固定するCPU番号。HYPERDU_URING_SQPOLL_CPU 相当。"
    )]
    uring_sqpoll_cpu: Option<u32>,

    /// Enable io_uring cooperative taskrun (Linux only)
    #[arg(
        long = "uring-coop",
        action = ArgAction::SetTrue,
        long_help = "io_uringのCOOP_TASKRUNを有効化します（Linuxのみ）。HYPERDU_URING_COOP_TASKRUN=1 相当。"
    )]
    uring_coop: bool,

    /// Linux: getdents64 buffer size in KiB (overrides env HYPERDU_GETDENTS_BUF_KB)
    #[arg(
        long = "getdents-buf-kb",
        value_name = "KiB",
        long_help = "Linuxのgetdents64で使用するバッファサイズ（KiB）。環境変数HYPERDU_GETDENTS_BUF_KBを上書きします。"
    )]
    getdents_buf_kb: Option<usize>,

    /// Split large directories every N entries (overrides env HYPERDU_DIR_YIELD_EVERY)
    #[arg(
        long = "dir-yield-every",
        value_name = "N",
        long_help = "巨大ディレクトリをN件ごとに分割スケジュールします。環境変数HYPERDU_DIR_YIELD_EVERYを上書きします。"
    )]
    dir_yield_every: Option<usize>,

    /// Linux: enable prefetch advise (posix_fadvise/readahead) (sets HYPERDU_PREFETCH=1)
    #[arg(
        long = "prefetch",
        action = ArgAction::SetTrue,
        long_help = "Linuxでposix_fadvise/readaheadヒントを有効化します（HYPERDU_PREFETCH=1 相当）。"
    )]
    prefetch: bool,

    /// Linux: pin worker threads to CPUs (sets HYPERDU_PIN_THREADS=1)
    #[arg(
        long = "pin-threads",
        action = ArgAction::SetTrue,
        long_help = "ワーカースレッドをCPUにピン固定します（Linux）。HYPERDU_PIN_THREADS=1 相当。"
    )]
    pin_threads: bool,

    /// Windows: use NT Query API fast path (sets HYPERDU_WIN_USE_NTQUERY=1)
    #[arg(
        long = "win-ntquery",
        action = ArgAction::SetTrue,
        long_help = "WindowsでNT Query APIベースの高速経路を使用します。HYPERDU_WIN_USE_NTQUERY=1 相当。"
    )]
    win_ntquery: bool,

    /// Enable live tuning (overrides env HYPERDU_TUNE)
    #[arg(
        long = "tune",
        action = ArgAction::SetTrue,
        long_help = "ライブチューニングを有効化します。環境変数HYPERDU_TUNEを上書き。"
    )]
    tune: bool,

    /// Live-tune interval in milliseconds (overrides env HYPERDU_TUNE_INTERVAL_MS)
    #[arg(
        long = "tune-interval-ms",
        value_name = "MS",
        long_help = "ライブチューニングの実行間隔(ms)。環境変数HYPERDU_TUNE_INTERVAL_MSを上書きします。"
    )]
    tune_interval_ms: Option<u64>,

    /// Disable filesystem auto strategy (sets HYPERDU_FS_AUTO=0)
    #[arg(
        long = "no-fs-auto",
        action = ArgAction::SetTrue,
        long_help = "ファイルシステム自動最適化を無効化します（HYPERDU_FS_AUTO=0 相当）。"
    )]
    no_fs_auto: bool,

    /// macOS: getattrlistbulk buffer size in KiB (overrides env HYPERDU_GALB_BUF_KB)
    #[arg(
        long = "galb-buf-kb",
        value_name = "KiB",
        long_help = "macOSのgetattrlistbulkバッファサイズ（KiB）。環境変数HYPERDU_GALB_BUF_KBを上書きします。"
    )]
    galb_buf_kb: Option<usize>,

    /// Compatibility mode: hyperdu (default), gnu, gnu-strict, posix-strict
    #[arg(
        long = "compat",
        value_enum,
        default_value_t = CompatArg::Hyperdu,
        long_help = "互換モードを選択。\n\
    hyperdu: 高機能な既定出力（トップ一覧+サマリ）\n\
    gnu: GNU duに近い出力（互換重視の基本設定）\n\
    gnu-strict: GNU duの厳密互換（ハードリンク重複排除・エラー出力など）\n\
    posix-strict: POSIX準拠の出力/ブロックサイズなど"
    )]
    compat: CompatArg,

    /// Override block size used for du-like output (e.g., 512, 1024, 1K, 1M)
    #[arg(
        long = "block-size",
        long_help = "du互換出力のブロックサイズを上書き（例: 512, 1024, 1K, 1M）。\n\
    --si併用でK/M/Gは10進（1000の累乗）として扱います。"
    )]
    block_size: Option<String>,

    /// Count hardlinks as separate files (GNU du defaultは重複排除)
    /// 明示指定がない場合はプロファイル既定値を維持します（例: --perf turbo では既定で有効）。
    #[arg(
        long = "count-links",
        action = ArgAction::SetTrue,
        long_help = "ハードリンクを別ファイルとして数えます（GNU duの既定は重複排除）。\n\
    明示指定がない場合はプロファイル既定値を維持します（例: --perf turbo では既定で有効）。"
    )]
    count_links: Option<bool>,
    /// Print apparent size (logical size). When set, physical size is not computed to save work
    #[arg(
        long = "apparent-size",
        action = ArgAction::SetTrue,
        long_help = "見かけのサイズ（論理サイズ）を出力に使用します。\n\
    du互換出力ではブロック数計算に論理サイズを用い、物理サイズの取得を省略します。"
    )]
    apparent_size: bool,
    /// Use SI units (K=1000, M=1000^2, G=1000^3) for -k/-m/-g and --block-size suffixes
    #[arg(
        long = "si",
        action = ArgAction::SetTrue,
        long_help = "-k/-m/-g と --block-size の接尾辞K/M/Gを10進（1000の累乗）として扱います。既定は2進（1024の累乗）。"
    )]
    si: bool,
    /// Set block-size=1 (bytes)
    #[arg(
        short = 'b',
        action = ArgAction::SetTrue,
        long_help = "--block-size=1 と同義（バイト単位）。"
    )]
    bytes: bool,
    /// Set block-size=1K (1024 or 1000 with --si)
    #[arg(
        short = 'k',
        action = ArgAction::SetTrue,
        long_help = "--block-size=1K と同義（1024、--si併用で1000）。"
    )]
    kib: bool,
    /// Set block-size=1M (1024^2 or 1000^2 with --si)
    #[arg(
        short = 'm',
        action = ArgAction::SetTrue,
        long_help = "--block-size=1M と同義（1024^2、--si併用で1000^2）。"
    )]
    mib: bool,
    /// Set block-size=1G (1024^3 or 1000^3 with --si)
    #[arg(
        short = 'g',
        action = ArgAction::SetTrue,
        long_help = "--block-size=1G と同義（1024^3、--si併用で1000^3）。"
    )]
    gib: bool,

    /// Print time column (default: mtime). Use --time-kind to choose
    #[arg(
        long = "time",
        action = ArgAction::SetTrue,
        long_help = "時刻列を出力に追加します（既定はmtime）。--time-kind と併用可。du互換出力で有効。"
    )]
    time: bool,
    /// Time kind for --time: mtime, atime, ctime
    #[arg(
        long = "time-kind",
        value_enum,
        long_help = "--time で出力する時刻の種類: mtime, atime, ctime。"
    )]
    time_kind: Option<TimeKindArg>,
    /// Time style: iso, long-iso, full-iso (default: iso)
    #[arg(
        long = "time-style",
        long_help = "時刻のフォーマット: iso, long-iso, full-iso または '+<strftimeパターン>'。"
    )]
    time_style: Option<String>,

    /// Performance profile: turbo (fastest), balanced (default), strict (max compatibility)
    #[arg(
        long = "perf",
        value_enum,
        default_value_t = PerfArg::Balanced,
        long_help = "性能プロファイルを選択。\n\
    turbo: もっとも高速（物理サイズ計算オフ/概算サイズ/ハードリンク非重複化=カウント）\n\
    balanced: 既定（バランス重視）\n\
    strict: 互換性最優先（互換モード厳格/ハードリンク重複排除/エラー出力など）"
    )]
    perf: PerfArg,
}

#[derive(Debug, Clone)]
struct AppConfig {
    auto_parallel: bool,
    heuristics_mode: String,
    prefer_inner_rayon: bool,
    tune_enabled: bool,
    tune_interval_ms: u64,
    win_allow_handle: bool,
    win_handle_sample_every: u64,
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

fn load_or_init_config() -> AppConfig {
    let dir = exe_dir().unwrap_or_else(|| PathBuf::from("."));
    let path = dir.join("hyperdu-config.json");
    if path.exists() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                let get_bool =
                    |k: &str, def: bool| v.get(k).and_then(|x| x.as_bool()).unwrap_or(def);
                let get_u64 = |k: &str, def: u64| v.get(k).and_then(|x| x.as_u64()).unwrap_or(def);
                let get_str = |k: &str, def: &str| {
                    v.get(k).and_then(|x| x.as_str()).unwrap_or(def).to_string()
                };
                return AppConfig {
                    auto_parallel: get_bool("auto_parallel", false),
                    heuristics_mode: get_str("heuristics_mode", "auto"),
                    prefer_inner_rayon: get_bool("prefer_inner_rayon", false),
                    tune_enabled: get_bool("tune_enabled", false),
                    tune_interval_ms: get_u64("tune_interval_ms", 800),
                    win_allow_handle: get_bool("win_allow_handle", false),
                    win_handle_sample_every: get_u64("win_handle_sample_every", 64),
                };
            }
        }
    }
    let cfg = AppConfig {
        auto_parallel: false,
        heuristics_mode: "auto".into(),
        prefer_inner_rayon: false,
        tune_enabled: false,
        tune_interval_ms: 800,
        win_allow_handle: false,
        win_handle_sample_every: 64,
    };
    let s = serde_json::to_string_pretty(&serde_json::json!({
        "auto_parallel": cfg.auto_parallel,
        "heuristics_mode": cfg.heuristics_mode,
        "prefer_inner_rayon": cfg.prefer_inner_rayon,
        "tune_enabled": cfg.tune_enabled,
        "tune_interval_ms": cfg.tune_interval_ms,
        "win_allow_handle": cfg.win_allow_handle,
        "win_handle_sample_every": cfg.win_handle_sample_every,
    }))
    .unwrap();
    let _ = std::fs::write(&path, s);
    eprintln!("initialized config: {}", path.display());
    cfg
}

fn main() -> Result<()> {
    env_logger::init();
    #[cfg(feature = "debug-eyre")]
    {
        let _ = color_eyre::install();
    }
    // Windows系の慣習的なヘルプエイリアスに対応（/help, /?）。
    // 早期判定してヘルプを表示して終了します。
    if std::env::args().any(|a| a == "/help" || a == "/?") {
        let mut cmd = Args::command();
        cmd.print_help()?;
        println!();
        return Ok(());
    }
    let args = Args::parse();
    let cfg = load_or_init_config();

    let mut exclude_contains: Vec<String> = args
        .exclude
        .as_deref()
        .unwrap_or(".git,node_modules,target")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut exclude_regex: Vec<String> = Vec::new();
    let mut exclude_glob: Vec<String> = Vec::new();
    for f in &args.exclude_from {
        if let Ok(text) = std::fs::read_to_string(f) {
            for line in text.lines() {
                let s = line.trim();
                if s.is_empty() || s.starts_with('#') {
                    continue;
                }
                if let Some(rest) = s.strip_prefix("re:") {
                    exclude_regex.push(rest.trim().to_string());
                } else if let Some(rest) = s.strip_prefix("glob:") {
                    exclude_glob.push(rest.trim().to_string());
                } else {
                    exclude_contains.push(s.to_string());
                }
            }
        }
    }

    let threads = args.threads.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });

    let mut opt = hyperdu_core::OptionsBuilder::new()
        .with_exclude_contains(exclude_contains)
        .with_exclude_regex(exclude_regex)
        .with_exclude_glob(exclude_glob)
        .max_depth(args.max_depth)
        .min_file_size(args.min_file_size)
        .follow_links(args.follow_links)
        .threads(threads)
        .with_tuning(hyperdu_core::TuningConfig {
            tune_enabled: Some(if args.tune { true } else { cfg.tune_enabled }),
            tune_interval_ms: Some(args.tune_interval_ms.unwrap_or(cfg.tune_interval_ms)),
        })
        .with_performance(hyperdu_core::PerformanceConfig {
            prefer_inner_rayon: Some(cfg.prefer_inner_rayon),
            disable_uring: Some(args.no_uring),
            ..Default::default()
        })
        .with_windows(hyperdu_core::WindowsConfig {
            win_allow_handle: Some(cfg.win_allow_handle),
            win_handle_sample_every: Some(cfg.win_handle_sample_every),
        })
        .build();

    // Graceful cancel: Ctrl-C updates opt.cancel; report once
    {
        let cancel = opt.cancel.clone();
        let notified = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notified2 = notified.clone();
        let _ = ctrlc::set_handler(move || {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            if !notified2.swap(true, std::sync::atomic::Ordering::Relaxed) {
                eprintln!("signal: cancelling… 現在までの集計を出力します");
            }
        });
    }
    // Apply performance profile (maps to existing flags)
    match args.perf {
        PerfArg::Turbo => {
            // fastest: approximate sizes, skip dedupe, lightweight errors
            opt.compute_physical = false;
            opt.approximate_sizes = true;
            opt.count_hardlinks = true; // do not dedupe
                                        // keep compat in HyperDU unless明示
                                        // io_uring の強化を環境変数でオプトイン（安全・可搬性優先）
            if std::env::var("HYPERDU_URING_SQPOLL").is_err() {
                std::env::set_var("HYPERDU_URING_SQPOLL", "1");
            }
            if std::env::var("HYPERDU_URING_COOP_TASKRUN").is_err() {
                std::env::set_var("HYPERDU_URING_COOP_TASKRUN", "1");
            }
            if std::env::var("HYPERDU_USE_URING").is_err() {
                std::env::set_var("HYPERDU_USE_URING", "1");
            }
            // 初期バッチ/深さを強めに（ライブチューナが追従）
            // 簡易ヒューリスティクス（環境変数で上書き可）
            let (b_default, d_default) =
                match std::env::var("HYPERDU_DEVICE_ROTATIONAL").ok().as_deref() {
                    Some("1") => (128usize, 512usize), // HDD
                    _ => (512usize, 2048usize),        // SSD/NVMe既定
                };
            opt.uring_batch
                .store(b_default, std::sync::atomic::Ordering::Relaxed);
            opt.uring_sq_depth
                .store(d_default, std::sync::atomic::Ordering::Relaxed);
        }
        PerfArg::Balanced => {
            // keep defaults (current behavior)
        }
        PerfArg::Strict => {
            // full compatibility target
            opt.compat_mode = match args.compat {
                CompatArg::Hyperdu | CompatArg::Gnu => hyperdu_core::CompatMode::GnuStrict,
                CompatArg::GnuStrict => hyperdu_core::CompatMode::GnuStrict,
                CompatArg::PosixStrict => hyperdu_core::CompatMode::PosixStrict,
            };
            opt.count_hardlinks = false; // dedupe
        }
    }
    // io_uring flags from CLI (Linux only; set envs expected by backend builder)
    #[cfg(target_os = "linux")]
    {
        if args.no_uring {
            std::env::set_var("HYPERDU_DISABLE_URING", "1");
        }
        if let Some(kb) = args.getdents_buf_kb {
            std::env::set_var("HYPERDU_GETDENTS_BUF_KB", kb.to_string());
        }
        if args.prefetch {
            std::env::set_var("HYPERDU_PREFETCH", "1");
        }
        if args.pin_threads {
            std::env::set_var("HYPERDU_PIN_THREADS", "1");
        }
        if args.uring_sqpoll {
            std::env::set_var("HYPERDU_URING_SQPOLL", "1");
        }
        if let Some(ms) = args.uring_sqpoll_idle_ms {
            std::env::set_var("HYPERDU_URING_SQPOLL_IDLE_MS", ms.to_string());
        }
        if let Some(cpu) = args.uring_sqpoll_cpu {
            std::env::set_var("HYPERDU_URING_SQPOLL_CPU", cpu.to_string());
        }
        if args.uring_coop {
            std::env::set_var("HYPERDU_URING_COOP_TASKRUN", "1");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(kb) = args.galb_buf_kb {
            std::env::set_var("HYPERDU_GALB_BUF_KB", kb.to_string());
        }
    }
    #[cfg(target_os = "windows")]
    {
        if args.win_ntquery {
            std::env::set_var("HYPERDU_WIN_USE_NTQUERY", "1");
        }
    }
    if args.no_fs_auto {
        std::env::set_var("HYPERDU_FS_AUTO", "0");
    }
    // Map compat flag
    // Preserve stricter compat selected by `--perf strict`.
    let perf_is_strict = matches!(args.perf, PerfArg::Strict);
    if !perf_is_strict {
        opt.compat_mode = match args.compat {
            CompatArg::Hyperdu => hyperdu_core::CompatMode::HyperDU,
            CompatArg::Gnu => hyperdu_core::CompatMode::GnuBasic,
            CompatArg::GnuStrict => hyperdu_core::CompatMode::GnuStrict,
            CompatArg::PosixStrict => hyperdu_core::CompatMode::PosixStrict,
        };
    }
    // Hardlink behavior
    // Keep performance profile's default unless user explicitly passed --count-links
    if let Some(true) = args.count_links {
        opt.count_hardlinks = true;
    }
    if !opt.count_hardlinks && !matches!(opt.compat_mode, hyperdu_core::CompatMode::HyperDU) {
        opt.inode_cache = Some(std::sync::Arc::new(dashmap::DashMap::with_capacity(1024)));
    }
    // Error reporter for compat modes (stderr)
    if !matches!(opt.compat_mode, hyperdu_core::CompatMode::HyperDU) {
        opt.error_report = Some(std::sync::Arc::new(|msg: &str| eprintln!("{msg}")));
    }
    // Apparent size: avoid computing physical size to minimize work
    if !matches!(opt.compat_mode, hyperdu_core::CompatMode::HyperDU) && args.apparent_size {
        opt.compute_physical = false;
    }
    if args.logical_only {
        opt.compute_physical = false;
    }
    if args.approximate {
        opt.approximate_sizes = true;
    }
    opt.one_file_system = args.one_file_system;
    if args.follow_links && !matches!(opt.compat_mode, hyperdu_core::CompatMode::HyperDU) {
        opt.visited_bloom = Some(std::sync::Arc::new(hyperdu_core::Bloom::with_bits(1 << 20)));
        opt.visited_dirs = Some(std::sync::Arc::new(dashmap::DashMap::with_capacity(1024)));
    }
    if let Some(b) = args.uring_batch {
        opt.uring_batch
            .store(b.max(1), std::sync::atomic::Ordering::Relaxed);
    }
    if let Some(d) = args.uring_depth {
        opt.uring_sq_depth
            .store(d.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    // Tuning-only mode: probe several candidates quickly and exit
    if args.tune_only {
        let secs = if args.tune_secs <= 0.1 {
            2.0
        } else {
            args.tune_secs
        };
        let mut probe = opt.clone();
        if probe.max_depth == 0 {
            probe.max_depth = 1;
        }
        probe.compute_physical = false;
        probe.progress_every = 0;
        let candidates: [usize; 6] = [8192, 16384, 32768, 65536, 131072, 262144];
        let t_start = std::time::Instant::now();
        let mut best_yield = 65536usize;
        let mut best_rate = 0.0f64;
        for y in candidates {
            probe
                .dir_yield_every
                .store(y, std::sync::atomic::Ordering::Relaxed);
            let ts = std::time::Instant::now();
            let root_probe = args
                .roots
                .first()
                .cloned()
                .unwrap_or_else(|| PathBuf::from("."));
            let map = match hyperdu_core::scan_directory(&root_probe, &probe) {
                Ok(m) => m,
                Err(_) => {
                    continue;
                }
            };
            let dt = ts.elapsed().as_secs_f64().max(1e-6);
            let total = *map
                .get(&root_probe)
                .unwrap_or(&hyperdu_core::Stat::default());
            let rate = (total.files as f64) / dt;
            println!(
                "tune: yield={} -> {:.0} files/s (files={})",
                y, rate, total.files
            );
            if rate > best_rate {
                best_rate = rate;
                best_yield = y;
            }
            if t_start.elapsed().as_secs_f64() >= secs {
                break;
            }
        }
        println!("recommended.dir_yield_every={best_yield}");
        println!("hint.nvme=65536-131072, hint.hdd=8192-16384");
        return Ok(());
    }

    // Live tuning enabled even without progress printing
    fn short_path(p: &std::path::Path) -> String {
        let name = p.file_name().and_then(|s| s.to_str());
        if let Some(n) = name {
            return n.to_string();
        }
        let s = p.to_string_lossy();
        let s: &str = &s;
        if s.len() <= 80 {
            s.to_string()
        } else {
            format!("…{}", &s[s.len() - 60..])
        }
    }
    if let Some(n) = args.dir_yield_every {
        opt.dir_yield_every
            .store(n.max(0), std::sync::atomic::Ordering::Relaxed);
    }
    opt.progress_every = args.progress_every.unwrap_or(8192);
    let print_progress = args.progress;
    let print_tune = args.tune_log || args.verbose;
    let tune_threshold = if args.tune_threshold <= 0.0 {
        0.05
    } else {
        args.tune_threshold
    };
    let t_start = std::time::Instant::now();
    let last = std::sync::Arc::new(std::sync::Mutex::new((0u64, t_start)));
    let last_cb = last.clone();
    let tuner_state = std::sync::Arc::new(std::sync::Mutex::new((2usize, 1isize, 0.0f64))); // (idx, dir, last_rate)
    let yield_candidates: [usize; 5] = [8192, 16384, 32768, 65536, 131072];
    let y_atomic = opt.dir_yield_every.clone();
    // io_uring batch tuner (Linux only; safe to set even if unused)
    let batch_state = std::sync::Arc::new(std::sync::Mutex::new((1usize, 1isize))); // start idx=1 -> 128, dir
    let batch_candidates_fast: [usize; 3] = [128, 256, 512];
    let batch_candidates_slow: [usize; 3] = [64, 128, 256];
    let b_atomic = opt.uring_batch.clone();
    // uring depth tuner (Linux only)
    let depth_state = std::sync::Arc::new(std::sync::Mutex::new((1usize, 1isize, 0u32))); // idx, dir, zero_fail_intervals
    let depth_candidates_fast: [u32; 4] = [256, 512, 1024, 2048];
    let depth_candidates_slow: [u32; 4] = [128, 256, 512, 1024];
    let d_atomic = opt.uring_sq_depth.clone();
    // metrics snapshot
    let m_prev = std::sync::Arc::new(std::sync::Mutex::new((0u64, 0u64, 0u64, 0u64))); // fail, wait_ns, enq, cqe
    let m_fail = opt.uring_sqe_fail.clone();
    let m_wait = opt.uring_submit_wait_ns.clone();
    let m_enq = opt.uring_sqe_enq.clone();
    let m_cqe = opt.uring_cqe_comp.clone();
    let _m_err = opt.uring_cqe_err.clone();
    let ema_state = std::sync::Arc::new(std::sync::Mutex::new((0.0f64, 0.0f64, 0.0f64))); // (ema_rate, ema_fail, ema_wait_ms)
    opt.progress_callback = Some(std::sync::Arc::new(move |n| {
        let now = std::time::Instant::now();
        let total_dt = now.duration_since(t_start).as_secs_f64().max(1e-6);
        let total_rate = (n as f64) / total_dt;
        let (prev_n, prev_t) = *last_cb.lock().unwrap();
        let delta_n = n.saturating_sub(prev_n);
        let delta_dt = now.duration_since(prev_t).as_secs_f64().max(1e-6);
        let recent_rate = (delta_n as f64) / delta_dt;
        *last_cb.lock().unwrap() = (n, now);
        if print_progress {
            println!(
                "progress: processed {n} files | rate: {total_rate:.0} f/s (recent {recent_rate:.0} f/s)"
            );
        }
        // Live tuning
        let mut st = tuner_state.lock().unwrap();
        let (ref mut idx, ref mut dir, ref mut last_rate) = *st;
        if *last_rate == 0.0 {
            *last_rate = recent_rate;
        }
        let degrade = recent_rate < *last_rate * (1.0 - tune_threshold);
        let improve = recent_rate > *last_rate * (1.0 + tune_threshold);
        if degrade {
            *dir = -*dir;
        }
        if degrade || improve {
            let new_idx =
                (*idx as isize + *dir).clamp(0, (yield_candidates.len() - 1) as isize) as usize;
            if new_idx != *idx {
                *idx = new_idx;
                let new_y = yield_candidates[*idx];
                y_atomic.store(new_y, std::sync::atomic::Ordering::Relaxed);
                if print_tune {
                    eprintln!("[live-tune] dir_yield_every -> {new_y}");
                }
            }
        }
        *last_rate = recent_rate;
        // Read metrics delta
        let (dfail, dwait_ns, denq, dcqe) = {
            let prev = &mut *m_prev.lock().unwrap();
            let cur_fail = m_fail.load(std::sync::atomic::Ordering::Relaxed);
            let cur_wait = m_wait.load(std::sync::atomic::Ordering::Relaxed);
            let cur_enq = m_enq.load(std::sync::atomic::Ordering::Relaxed);
            let cur_cqe = m_cqe.load(std::sync::atomic::Ordering::Relaxed);
            let df = cur_fail.saturating_sub(prev.0);
            let dw = cur_wait.saturating_sub(prev.1);
            let de = cur_enq.saturating_sub(prev.2);
            let dc = cur_cqe.saturating_sub(prev.3);
            *prev = (cur_fail, cur_wait, cur_enq, cur_cqe);
            (df, dw, de, dc)
        };
        // Update EMA metrics
        {
            let mut ema = ema_state.lock().unwrap();
            let alpha = 0.2f64;
            // files/s EMA
            ema.0 = if ema.0 == 0.0 {
                recent_rate
            } else {
                alpha * recent_rate + (1.0 - alpha) * ema.0
            };
            // fail ratio EMA
            let fr = if denq > 0 {
                (dfail as f64) / (denq as f64)
            } else {
                0.0
            };
            ema.1 = if ema.1 == 0.0 {
                fr
            } else {
                alpha * fr + (1.0 - alpha) * ema.1
            };
            // wait per enq (ms) EMA
            let w_ms = if denq > 0 {
                (dwait_ns as f64) / 1.0e6 / (denq as f64)
            } else {
                0.0
            };
            ema.2 = if ema.2 == 0.0 {
                w_ms
            } else {
                alpha * w_ms + (1.0 - alpha) * ema.2
            };
        }
        let (ema_rate, ema_fail, ema_wait_ms) = {
            let e = ema_state.lock().unwrap();
            (e.0, e.1, e.2)
        };
        // Choose candidate sets heuristically（NVMe vs HDD）
        let fast_device = ema_wait_ms < 0.02 && ema_rate > 20000.0;
        let batch_candidates = if fast_device {
            &batch_candidates_fast[..]
        } else {
            &batch_candidates_slow[..]
        };
        let depth_candidates = if fast_device {
            &depth_candidates_fast[..]
        } else {
            &depth_candidates_slow[..]
        };
        // Tune io_uring batch similarly
        {
            let mut bst = batch_state.lock().unwrap();
            let (ref mut bidx, ref mut bdir) = *bst;
            if degrade {
                *bdir = -*bdir;
            }
            if degrade || improve {
                let new_bidx = (*bidx as isize + *bdir)
                    .clamp(0, (batch_candidates.len() - 1) as isize)
                    as usize;
                if new_bidx != *bidx {
                    *bidx = new_bidx;
                    let new_b = batch_candidates[*bidx];
                    b_atomic.store(new_b, std::sync::atomic::Ordering::Relaxed);
                    if print_tune {
                        eprintln!("[live-tune] uring_batch -> {new_b}");
                    }
                }
            }
        }
        // Tune io_uring depth (increase on queue saturation, decrease slowly)
        {
            let mut dst = depth_state.lock().unwrap();
            let (ref mut didx, ref mut _ddir, ref mut zero_fail) = *dst;
            let saturated = dfail > 0 || (denq > 0 && dcqe * 2 < denq) || ema_fail > 0.05;
            if saturated {
                // push deeper
                let new_didx =
                    (*didx as isize + 1).clamp(0, (depth_candidates.len() - 1) as isize) as usize;
                if new_didx != *didx {
                    *didx = new_didx;
                    let new_d = depth_candidates[*didx] as usize;
                    d_atomic.store(new_d, std::sync::atomic::Ordering::Relaxed);
                    if print_tune {
                        eprintln!("[live-tune] uring_depth -> {new_d}");
                    }
                }
                *zero_fail = 0;
            } else {
                *zero_fail = zero_fail.saturating_add(1);
                if *zero_fail >= 3 && !degrade {
                    // ease off one step if stable and no saturation
                    let new_didx = (*didx as isize - 1)
                        .clamp(0, (depth_candidates.len() - 1) as isize)
                        as usize;
                    if new_didx != *didx {
                        *didx = new_didx;
                        let new_d = depth_candidates[*didx] as usize;
                        d_atomic.store(new_d, std::sync::atomic::Ordering::Relaxed);
                        if print_tune {
                            eprintln!("[live-tune] uring_depth -> {new_d}");
                        }
                    }
                    *zero_fail = 0;
                }
            }
        }
    }));
    // Keep-alive: emit periodic status if no progress callback fired recently
    let _keepalive = KeepAlive::start(print_progress, last.clone());
    if args.progress {
        opt.progress_path_callback = Some(std::sync::Arc::new(move |p: &std::path::Path| {
            let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            println!(
                "  sample: {} (size: {})",
                short_path(p),
                format_size(size, BINARY)
            );
        }));
    }

    // Roots: if none provided, use current directory
    let roots: Vec<PathBuf> = if args.roots.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.roots.clone()
    };

    // Quick Win: Minimal FS detection to improve defaults on DrvFS/Network FS
    #[cfg(target_os = "linux")]
    {
        if std::env::var("HYPERDU_FS_AUTO").ok().as_deref() != Some("0") {
            if let Some(root0) = roots.first() {
                if let Some(rep) = hyperdu_core::fs_strategy::detect_and_apply(root0, &mut opt) {
                    // Apply optional suggestions at CLI level (respect user overrides)
                    if rep.disable_uring {
                        std::env::set_var("HYPERDU_DISABLE_URING", "1");
                        opt.disable_uring = true;
                    }
                    if args.threads.is_none() {
                        if let Some(t) = rep.recommended_threads {
                            // clamp to [1, cpu]
                            let cpu = std::thread::available_parallelism()
                                .map(|n| n.get())
                                .unwrap_or(4);
                            let t = t.clamp(1, cpu);
                            opt.threads = t;
                        }
                    }
                    // Emit detailed report
                    let mut meta = vec![
                        format!("fs='{}'", rep.fs_type),
                        format!("strategy='{}'", rep.strategy),
                        format!("reason='{}'", rep.reason),
                    ];
                    if let Some(t) = rep.recommended_threads {
                        meta.push(format!("threads_reco={t}"));
                    }
                    if rep.disable_uring {
                        meta.push("disable_uring=1".into());
                    }
                    if rep.recommend_logical_only {
                        meta.push("hint=logical-only".into());
                    }
                    if !rep.changes.is_empty() {
                        meta.push(format!("changes=[{}]", rep.changes.join(",")));
                    }
                    println!("fs-auto: {} for '{}'", meta.join(" "), root0.display());
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        if std::env::var("HYPERDU_FS_AUTO").ok().as_deref() != Some("0") {
            if let Some(root0) = roots.first() {
                println!(
                    "fs-auto: fs='unknown' strategy='generic' reason='platform=non-linux' for '{}'",
                    root0.display()
                );
            }
        }
    }
    let mut total_dt = std::time::Duration::from_secs(0);
    let mut exit_code = 0i32;

    if matches!(opt.compat_mode, hyperdu_core::CompatMode::HyperDU) {
        if roots.len() > 1 {
            eprintln!("note: multiple roots given; showing report for first only");
        }
        let root = roots.first().expect("at least one root");
        let t0 = std::time::Instant::now();
        let map = hyperdu_core::scan_directory(root, &opt)?;
        let dt = t0.elapsed();
        total_dt += dt;
        let total_stat = *map.get(root).unwrap_or(&hyperdu_core::Stat::default());
        // Emit a final progress line if progress enabled and threshold未達で未出力の場合
        if print_progress {
            let now = std::time::Instant::now();
            let (prev_n, prev_t) = *last.lock().unwrap();
            if total_stat.files > prev_n {
                let total_dt_s = now.duration_since(t_start).as_secs_f64().max(1e-6);
                let total_rate = (total_stat.files as f64) / total_dt_s;
                let delta_n = total_stat.files.saturating_sub(prev_n);
                let delta_dt = now.duration_since(prev_t).as_secs_f64().max(1e-6);
                let recent_rate = (delta_n as f64) / delta_dt;
                println!(
                    "progress: processed {files} files | rate: {total_rate:.0} f/s (recent {recent_rate:.0} f/s)",
                    files = total_stat.files
                );
                *last.lock().unwrap() = (total_stat.files, now);
            }
        }
        let dirs_scanned = map.len();
        let mut v: Vec<(PathBuf, hyperdu_core::Stat)> = map.into_iter().collect();
        if args.top > 0 && v.len() > args.top {
            let n = args.top.min(v.len());
            let idx = n - 1;
            v.select_nth_unstable_by(idx, |a, b| b.1.physical.cmp(&a.1.physical));
            v[..n].sort_unstable_by_key(|(_, s)| std::cmp::Reverse(s.physical));
        } else {
            v.sort_unstable_by_key(|(_, s)| std::cmp::Reverse(s.physical));
        }

        println!("Top {} under {} (physical desc):", args.top, root.display());
        for (i, (p, s)) in v.iter().take(args.top).enumerate() {
            println!(
                "{:>3}. {:<} | phys={} | log={} | files={}",
                i + 1,
                p.display(),
                format_size(s.physical, BINARY),
                format_size(s.logical, BINARY),
                s.files
            );
        }
        println!();
        println!("Summary:");
        println!("  Root: {}", root.display());
        println!("  Elapsed: {:.3}s", dt.as_secs_f64());
        println!("  Threads: {threads}");
        println!("  Follow links: {}", args.follow_links);
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            println!(
                "  Uring: depth={} | batch={}",
                opt.uring_sq_depth
                    .load(std::sync::atomic::Ordering::Relaxed),
                opt.uring_batch.load(std::sync::atomic::Ordering::Relaxed)
            );
            let fail = opt
                .uring_sqe_fail
                .load(std::sync::atomic::Ordering::Relaxed);
            let wait_ns = opt
                .uring_submit_wait_ns
                .load(std::sync::atomic::Ordering::Relaxed);
            let enq = opt.uring_sqe_enq.load(std::sync::atomic::Ordering::Relaxed);
            let cqe = opt
                .uring_cqe_comp
                .load(std::sync::atomic::Ordering::Relaxed);
            println!(
                "  Uring-metrics: sqe_fail={} | submit_wait={:.2}ms | enq={} | cqe={}",
                fail,
                (wait_ns as f64) / 1.0e6,
                enq,
                cqe
            );
        }
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        {
            println!("  Uring: n/a | batch=n/a");
            println!("  Uring-metrics: n/a");
        }
        println!(
            "  Total: files={} | phys={} | log={} | dirs={}",
            total_stat.files,
            format_size(total_stat.physical, BINARY),
            format_size(total_stat.logical, BINARY),
            dirs_scanned
        );

        // Disk/Volume usage (best-effort)
        if let Some((vol_total, vol_free)) = fs_total_free(root) {
            let used = vol_total.saturating_sub(vol_free);
            let pct: f64 = if vol_total > 0 {
                (used as f64) * 100.0 / (vol_total as f64)
            } else {
                0.0
            };
            println!(
                "  Disk: total={} | used={} | free={} | usage={:.1}%",
                format_size(vol_total, BINARY),
                format_size(used, BINARY),
                format_size(vol_free, BINARY),
                pct
            );
        }

        // CSV / JSON exports (auto-save on --verbose)
        let auto_json = args.verbose.then(|| PathBuf::from("hyperdu-report.json"));
        let auto_csv = args.verbose.then(|| PathBuf::from("hyperdu-report.csv"));
        if let Some(csv_path) = args.csv.as_ref().or(auto_csv.as_ref()) {
            let mut wtr = csv::Writer::from_path(csv_path)?;
            wtr.write_record(["path", "logical", "physical", "files"])?;
            for (p, s) in &v {
                wtr.write_record([
                    p.to_string_lossy().as_ref(),
                    &s.logical.to_string(),
                    &s.physical.to_string(),
                    &s.files.to_string(),
                ])?;
            }
            wtr.flush()?;
            println!("wrote CSV: {}", csv_path.display());
        }
        if let Some(json_path) = args.json.as_ref().or(auto_json.as_ref()) {
            let mut file = File::create(json_path)?;
            let json = serde_json::to_string_pretty(&v.iter().map(|(p, s)| serde_json::json!({"path": p, "logical": s.logical, "physical": s.physical, "files": s.files})).collect::<Vec<_>>())?;
            file.write_all(json.as_bytes())?;
            println!("wrote JSON: {}", json_path.display());
        }
        // Optional classification after scan
        if let Some(mode) = &args.classify {
            let cmode = match mode.as_str() {
                "deep" => hyperdu_core::classify::ClassifyMode::Deep,
                _ => hyperdu_core::classify::ClassifyMode::Basic,
            };
            let class_stats = hyperdu_core::classify::classify_directory(root, &opt, cmode);
            println!(
                "classify: categories={} extensions={} top_entries={}",
                class_stats.by_category.len(),
                class_stats.by_extension.len(),
                class_stats.top_consumers.len()
            );
            let auto_cjson = args.verbose.then(|| PathBuf::from("class-report.json"));
            let auto_ccsv = args.verbose.then(|| PathBuf::from("class-report.csv"));
            if let Some(p) = args.class_report.as_ref().or(auto_cjson.as_ref()) {
                let mut file = File::create(p)?;
                let json = serde_json::to_string_pretty(&serde_json::json!({
                    "by_category": class_stats.by_category,
                    "by_extension": class_stats.by_extension,
                    "top_consumers": class_stats.top_consumers.iter().rev().take(200).map(|(sz, v)| serde_json::json!({"size": sz, "paths": v})).collect::<Vec<_>>()
                }))?;
                file.write_all(json.as_bytes())?;
                println!("wrote class-report: {}", p.display());
            }
            if let Some(p) = args.class_report_csv.as_ref().or(auto_ccsv.as_ref()) {
                let mut wtr = csv::Writer::from_path(p)?;
                wtr.write_record(["kind", "key", "files", "bytes"])?;
                for (k, v) in class_stats.by_category.iter() {
                    wtr.write_record(["category", k, &v.files.to_string(), &v.bytes.to_string()])?;
                }
                for (k, v) in class_stats.by_extension.iter() {
                    wtr.write_record(["extension", k, &v.files.to_string(), &v.bytes.to_string()])?;
                }
                wtr.flush()?;
                println!("wrote class-report-csv: {}", p.display());
            }
        }
        // Optional incremental delta/snapshot
        if let Some(dbp) = &args.incr_db {
            let db = hyperdu_core::incremental::open_db(dbp)?;
            if args.compute_delta {
                let d = hyperdu_core::incremental::compute_delta(&db, root, &opt)?;
                eprintln!(
                    "delta: added={} modified={} removed={}",
                    d.added, d.modified, d.removed
                );
            }
            if args.update_snapshot {
                hyperdu_core::incremental::snapshot_walk_and_update(&db, root, &opt)?;
                let pruned = hyperdu_core::incremental::snapshot_prune_removed(&db, root)?;
                eprintln!(
                    "snapshot: updated DB at {} (pruned {} stale entries)",
                    dbp.display(),
                    pruned
                );
            }
            if args.watch {
                eprintln!("watch: monitoring {} (Ctrl-C to stop)", root.display());
                let _w = hyperdu_core::incremental::watch(root, |kind, p| {
                    eprintln!("fswatch: {} {}", kind, p.display())
                });
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(60));
                }
            }
        }
        // progress already emitted during scan when enabled
        Ok(())
    } else {
        // du-like output: blocks<TAB>path sorted alphabetically
        let bs = if args.bytes {
            1
        } else if args.kib {
            if args.si {
                1000
            } else {
                1024
            }
        } else if args.mib {
            if args.si {
                1000 * 1000
            } else {
                1024 * 1024
            }
        } else if args.gib {
            if args.si {
                1000 * 1000 * 1000
            } else {
                1024 * 1024 * 1024
            }
        } else if let Some(bs) = &args.block_size {
            parse_block_size_with_si(bs, args.si).unwrap_or(1024)
        } else if std::env::var_os("POSIXLY_CORRECT").is_some()
            || matches!(opt.compat_mode, hyperdu_core::CompatMode::PosixStrict)
        {
            512
        } else {
            1024
        };
        // optional time output
        let print_time = args.time || args.time_kind.is_some();
        let time_kind = args.time_kind.unwrap_or(TimeKindArg::Mtime);
        let time_style = args.time_style.as_deref().unwrap_or("iso");
        // Heuristics mode from config
        opt.heuristics_mode = match cfg.heuristics_mode.as_str() {
            "outer" => hyperdu_core::HeuristicsMode::OuterOnly,
            "inner" => hyperdu_core::HeuristicsMode::InnerOnly,
            _ => hyperdu_core::HeuristicsMode::Auto,
        };

        #[cfg(feature = "rayon-par")]
        {
            if cfg.auto_parallel {
                let t0 = std::time::Instant::now();
                let merged = hyperdu_core::auto_parallel_scan(roots.clone(), &opt)?;
                total_dt += t0.elapsed();
                for root in roots {
                    let mut entries: Vec<(PathBuf, hyperdu_core::Stat)> = merged
                        .iter()
                        .filter(|(p, _)| p.starts_with(&root))
                        .map(|(p, s)| (p.clone(), *s))
                        .collect();
                    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                    if print_progress {
                        let total_files: u64 = entries.iter().map(|(_, s)| s.files).sum();
                        let now = std::time::Instant::now();
                        let (prev_n, prev_t) = *last.lock().unwrap();
                        if total_files > prev_n {
                            let total_dt_s = now.duration_since(t_start).as_secs_f64().max(1e-6);
                            let total_rate = (total_files as f64) / total_dt_s;
                            let delta_n = total_files.saturating_sub(prev_n);
                            let delta_dt = now.duration_since(prev_t).as_secs_f64().max(1e-6);
                            let recent_rate = (delta_n as f64) / delta_dt;
                            println!(
                                "progress: processed {total_files} files | rate: {total_rate:.0} f/s (recent {recent_rate:.0} f/s)"
                            );
                            *last.lock().unwrap() = (total_files, now);
                        }
                    }
                    for (p, s) in entries {
                        if p.as_os_str().is_empty() {
                            continue;
                        }
                        let bytes = if args.apparent_size {
                            s.logical
                        } else {
                            s.physical
                        };
                        let blocks = div_ceil(bytes, bs as u64);
                        if print_time {
                            println!(
                                "{}\t{}\t{}",
                                blocks,
                                format_time(&p, time_kind, time_style),
                                p.display()
                            );
                        } else {
                            println!("{}\t{}", blocks, p.display());
                        }
                    }
                }
                return Ok(());
            }
            if cfg.auto_parallel
                && matches!(opt.heuristics_mode, hyperdu_core::HeuristicsMode::OuterOnly)
            {
                let t0 = std::time::Instant::now();
                let merged = hyperdu_core::parallel_scan(roots.clone(), &opt)?;
                total_dt += t0.elapsed();
                for root in roots {
                    let mut entries: Vec<(PathBuf, hyperdu_core::Stat)> = merged
                        .iter()
                        .filter(|(p, _)| p.starts_with(&root))
                        .map(|(p, s)| (p.clone(), *s))
                        .collect();
                    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                    if print_progress {
                        let total_files: u64 = entries.iter().map(|(_, s)| s.files).sum();
                        let now = std::time::Instant::now();
                        let (prev_n, prev_t) = *last.lock().unwrap();
                        if total_files > prev_n {
                            let total_dt_s = now.duration_since(t_start).as_secs_f64().max(1e-6);
                            let total_rate = (total_files as f64) / total_dt_s;
                            let delta_n = total_files.saturating_sub(prev_n);
                            let delta_dt = now.duration_since(prev_t).as_secs_f64().max(1e-6);
                            let recent_rate = (delta_n as f64) / delta_dt;
                            println!(
                                "progress: processed {} files | rate: {:.0} f/s (recent {:.0} f/s)",
                                total_files, total_rate, recent_rate
                            );
                            *last.lock().unwrap() = (total_files, now);
                        }
                    }
                    for (p, s) in entries {
                        if p.as_os_str().is_empty() {
                            continue;
                        }
                        let bytes = if args.apparent_size {
                            s.logical
                        } else {
                            s.physical
                        };
                        let blocks = div_ceil(bytes, bs as u64);
                        if print_time {
                            println!(
                                "{}\t{}\t{}",
                                blocks,
                                format_time(&p, time_kind, time_style),
                                p.display()
                            );
                        } else {
                            println!("{}\t{}", blocks, p.display());
                        }
                    }
                }
                return Ok(());
            }
        }
        #[cfg(not(feature = "rayon-par"))]
        {
            if cfg.auto_parallel {
                eprintln!(
                    "note: built without 'rayon-par' feature; falling back to sequential scan"
                );
            }
        }

        for root in roots {
            let t0 = std::time::Instant::now();
            match hyperdu_core::scan_directory(&root, &opt) {
                Ok(map) => {
                    let mut entries: Vec<(PathBuf, hyperdu_core::Stat)> = map.into_iter().collect();
                    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                    if print_progress {
                        let total_files = entries
                            .iter()
                            .find(|(p, _)| p == &root)
                            .map(|(_, s)| s.files)
                            .unwrap_or_else(|| entries.iter().map(|(_, s)| s.files).sum());
                        let now = std::time::Instant::now();
                        let (prev_n, prev_t) = *last.lock().unwrap();
                        if total_files > prev_n {
                            let total_dt_s = now.duration_since(t_start).as_secs_f64().max(1e-6);
                            let total_rate = (total_files as f64) / total_dt_s;
                            let delta_n = total_files.saturating_sub(prev_n);
                            let delta_dt = now.duration_since(prev_t).as_secs_f64().max(1e-6);
                            let recent_rate = (delta_n as f64) / delta_dt;
                            println!(
                                "progress: processed {} files | rate: {:.0} f/s (recent {:.0} f/s)",
                                total_files, total_rate, recent_rate
                            );
                            *last.lock().unwrap() = (total_files, now);
                        }
                    }
                    for (p, s) in entries {
                        if p.as_os_str().is_empty() {
                            continue;
                        }
                        let bytes = if args.apparent_size {
                            s.logical
                        } else {
                            s.physical
                        };
                        let blocks = div_ceil(bytes, bs as u64);
                        if print_time {
                            println!(
                                "{}\t{}\t{}",
                                blocks,
                                format_time(&p, time_kind, time_style),
                                p.display()
                            );
                        } else {
                            println!("{}\t{}", blocks, p.display());
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{}: {}", root.display(), e);
                    exit_code = 1;
                }
            }
            total_dt += t0.elapsed();
        }
        let errn = opt.error_count.load(std::sync::atomic::Ordering::Relaxed);
        if errn > 0 || exit_code != 0 {
            std::process::exit(1);
        }
        Ok(())
    }
}

#[allow(dead_code)]
fn parse_block_size(s: &str) -> Option<u64> {
    let sl = s.trim().to_ascii_lowercase();
    let (num, mul) = if sl.ends_with('k') {
        (&sl[..sl.len() - 1], 1024u64)
    } else if sl.ends_with('m') {
        (&sl[..sl.len() - 1], 1024u64 * 1024)
    } else if sl.ends_with('g') {
        (&sl[..sl.len() - 1], 1024u64 * 1024 * 1024)
    } else {
        (sl.as_str(), 1u64)
    };
    num.parse::<u64>().ok().map(|n| n.saturating_mul(mul))
}

#[inline(always)]
fn div_ceil(n: u64, d: u64) -> u64 {
    n.div_ceil(d)
}

fn parse_block_size_with_si(s: &str, si: bool) -> Option<u64> {
    let sl = s.trim().to_ascii_lowercase();
    let (num, mul) = if sl.ends_with('k') {
        (&sl[..sl.len() - 1], if si { 1000 } else { 1024 })
    } else if sl.ends_with('m') {
        (
            &sl[..sl.len() - 1],
            if si { 1000 * 1000 } else { 1024 * 1024 },
        )
    } else if sl.ends_with('g') {
        (
            &sl[..sl.len() - 1],
            if si {
                1000 * 1000 * 1000
            } else {
                1024 * 1024 * 1024
            },
        )
    } else {
        (sl.as_str(), 1u64)
    };
    num.parse::<u64>().ok().map(|n| n.saturating_mul(mul))
}

#[cfg(feature = "time-format")]
fn format_time(p: &std::path::Path, when: TimeKindArg, style: &str) -> String {
    // Only called when user explicitly requested --time; keep it minimal
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(md) = std::fs::symlink_metadata(p) {
            let (secs, _nsec) = match when {
                TimeKindArg::Mtime => (md.mtime(), md.mtime_nsec()),
                TimeKindArg::Atime => (md.atime(), md.atime_nsec()),
                TimeKindArg::Ctime => (md.ctime(), md.ctime_nsec()),
            };
            let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
                .map(|d| d.naive_utc())
                .unwrap_or_else(|| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
                        .unwrap()
                        .naive_utc()
                });
            return match style {
                "full-iso" => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
                "long-iso" => dt.format("%Y-%m-%d %H:%M").to_string(),
                s if s.starts_with('+') => dt.format(&s[1..]).to_string(),
                _ => dt.format("%Y-%m-%d %H:%M").to_string(),
            };
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if let Ok(md) = std::fs::symlink_metadata(p) {
            let t100 = match when {
                TimeKindArg::Mtime => md.last_write_time(),
                TimeKindArg::Atime => md.last_access_time(),
                TimeKindArg::Ctime => md.creation_time(),
            };
            // FILETIME epoch (1601) to Unix epoch (1970)
            let secs = ((t100 / 10_000_000) as i64) - 11644473600i64;
            let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
                .map(|d| d.naive_utc())
                .unwrap_or_else(|| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
                        .unwrap()
                        .naive_utc()
                });
            return match style {
                "full-iso" => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
                "long-iso" => dt.format("%Y-%m-%d %H:%M").to_string(),
                s if s.starts_with('+') => dt.format(&s[1..]).to_string(),
                _ => dt.format("%Y-%m-%d %H:%M").to_string(),
            };
        }
    }
    String::from("-")
}

#[cfg(not(feature = "time-format"))]
fn format_time(_p: &std::path::Path, _when: TimeKindArg, _style: &str) -> String {
    String::from("-")
}

// fs detection moved to hyperdu-core::fs_strategy
