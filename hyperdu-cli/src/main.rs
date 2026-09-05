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

/// CLI spelling of `hyperdu_core::IoProfile`.
#[derive(ValueEnum, Clone, Copy, Debug)]
enum IoProfileArg {
    Throughput,
    Balanced,
    Gentle,
}

impl From<IoProfileArg> for hyperdu_core::IoProfile {
    fn from(v: IoProfileArg) -> Self {
        match v {
            IoProfileArg::Throughput => hyperdu_core::IoProfile::Throughput,
            IoProfileArg::Balanced => hyperdu_core::IoProfile::Balanced,
            IoProfileArg::Gentle => hyperdu_core::IoProfile::Gentle,
        }
    }
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
    既定では何も除外しません（du と同じ集計になります）。\n\
    部分一致である点に注意してください。--exclude .git は .github にも一致します。\n\
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

    /// Read the NTFS $MFT directly (Windows, volume root, administrator)
    #[arg(
        long = "mft",
        action = ArgAction::SetTrue,
        long_help = "NTFS の $MFT を直接読んでボリューム全体を走査します（Windows のみ）。\n\
    以下のいずれかに当てはまる場合は自動的に通常の列挙へ切り替わります。\n\
      - 管理者権限で実行していない\n\
      - 走査対象がボリュームのルート（例: C:\\）でない\n\
      - NTFS でない、または $MFT の解析に失敗した\n\
    切り替わっても結果は正しく、遅くなるだけです。"
    )]
    mft: bool,

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

    /// Deprecated: the io_uring backend was removed. Accepted and ignored.
    #[arg(
        long = "uring-batch",
        long_help = "非推奨。io_uring バックエンドは削除されました。指定しても無視されます。"
    )]
    uring_batch: Option<usize>,

    /// Deprecated: the io_uring backend was removed. Accepted and ignored.
    #[arg(
        long = "uring-depth",
        long_help = "非推奨。io_uring バックエンドは削除されました。指定しても無視されます。"
    )]
    uring_depth: Option<usize>,

    /// How hard the scan is allowed to hit the storage device
    #[arg(
        long = "io-profile",
        value_enum,
        long_help = "ストレージへの負荷レベルを選びます。
        throughput: 先読みを有効にし最大スループットを狙います（他のI/Oを圧迫します）。
        balanced (既定): 先読みなしで並列度は通常どおり。
        gentle: スレッド数を2に抑え、先読みを無効化します。正確さは変わりません。"
    )]
    io_profile: Option<IoProfileArg>,

    /// Deprecated: the io_uring backend was removed. Accepted and ignored.
    #[arg(
        long = "no-uring",
        action = ArgAction::SetTrue,
        long_help = "非推奨。io_uring バックエンドは削除されました。指定しても無視されます。"
    )]
    no_uring: bool,

    /// Deprecated: the io_uring backend was removed. Accepted and ignored.
    #[arg(
        long = "uring-sqpoll",
        action = ArgAction::SetTrue,
        long_help = "非推奨。io_uring バックエンドは削除されました。指定しても無視されます。"
    )]
    uring_sqpoll: bool,

    /// Deprecated: the io_uring backend was removed. Accepted and ignored.
    #[arg(
        long = "uring-sqpoll-idle-ms",
        long_help = "非推奨。io_uring バックエンドは削除されました。指定しても無視されます。"
    )]
    uring_sqpoll_idle_ms: Option<u32>,

    /// Deprecated: the io_uring backend was removed. Accepted and ignored.
    #[arg(
        long = "uring-sqpoll-cpu",
        long_help = "非推奨。io_uring バックエンドは削除されました。指定しても無視されます。"
    )]
    uring_sqpoll_cpu: Option<u32>,

    /// Deprecated: the io_uring backend was removed. Accepted and ignored.
    #[arg(
        long = "uring-coop",
        action = ArgAction::SetTrue,
        long_help = "非推奨。io_uring バックエンドは削除されました。指定しても無視されます。"
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

    /// Linux: force prefetch advise (posix_fadvise/readahead) on or off
    #[arg(
        long = "prefetch",
        num_args = 0..=1,
        default_missing_value = "true",
        long_help = "Linuxでposix_fadvise/readaheadヒントを強制的に有効/無効にします。--io-profile の既定を上書きします（--prefetch=false で先読みを止められます）。"
    )]
    prefetch: Option<bool>,

    /// Linux: pin worker threads to CPUs (sets HYPERDU_PIN_THREADS=1)
    #[arg(
        long = "pin-threads",
        action = ArgAction::SetTrue,
        long_help = "ワーカースレッドをCPUにピン固定します（Linux）。HYPERDU_PIN_THREADS=1 相当。"
    )]
    pin_threads: bool,

    /// Windows: force the NtQueryDirectoryFile path (default on MSVC builds; HYPERDU_WIN_USE_NTQUERY=0 selects FindFirstFileExW)
    #[arg(
        long = "win-ntquery",
        action = ArgAction::SetTrue,
        long_help = "WindowsでNtQueryDirectoryFileベースの列挙経路を明示的に有効化します（MSVCビルドでは既定で有効）。\n\
    物理サイズとファイルIDを列挙結果から直接取得するため、ファイル毎の追加システムコールが不要です。\n\
    無効化して FindFirstFileExW 経路に切り替えるには環境変数 HYPERDU_WIN_USE_NTQUERY=0 を設定します。"
    )]
    win_ntquery: bool,

    /// Enable live tuning
    #[arg(
        long = "tune",
        action = ArgAction::SetTrue,
        long_help = "ライブチューニングを有効化します。"
    )]
    tune: bool,

    /// Live-tune interval in milliseconds
    #[arg(
        long = "tune-interval-ms",
        value_name = "MS",
        long_help = "ライブチューニングの実行間隔(ms)。"
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
    /// Equivalent to --apparent-size --block-size=1 (GNU du -b)
    #[arg(
        short = 'b',
        action = ArgAction::SetTrue,
        long_help = "--apparent-size --block-size=1 と同義（GNU du の -b と同じく見かけのサイズをバイト単位で出力）。"
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
    turbo: 最速。論理サイズのみ（物理計算オフ）/ハードリンク重複排除なし。\n\
            サイズは正確です。概算にする場合は --approximate を明示してください\n\
            （合計が実測で -95%〜+149% 外れます）。\n\
    balanced: 既定（バランス重視）。\n\
    strict: 互換性最優先（du互換を厳格化/ハードリンク重複排除/エラー出力など）。"
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

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            auto_parallel: false,
            heuristics_mode: "auto".into(),
            prefer_inner_rayon: false,
            tune_enabled: false,
            tune_interval_ms: 800,
            win_allow_handle: false,
            win_handle_sample_every: 64,
        }
    }
}

/// Read the optional config file next to the executable.
///
/// Absent or unreadable means "use the defaults", silently. Every run used to
/// `exists()` the path and then, when it was missing, write the defaults back
/// out and print a line about it. Beside a system-installed binary that
/// directory is not writable, so the attempt failed on every single run — and
/// still printed. The read alone is enough; `read_to_string` already reports a
/// missing file, so the extra `exists()` stat is gone too.
fn load_config() -> AppConfig {
    let dir = exe_dir().unwrap_or_else(|| PathBuf::from("."));
    let path = dir.join("hyperdu-config.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return AppConfig::default();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        eprintln!("config は不正な JSON のため無視します: {}", path.display());
        return AppConfig::default();
    };
    let d = AppConfig::default();
    let get_bool = |k: &str, def: bool| v.get(k).and_then(|x| x.as_bool()).unwrap_or(def);
    let get_u64 = |k: &str, def: u64| v.get(k).and_then(|x| x.as_u64()).unwrap_or(def);
    AppConfig {
        auto_parallel: get_bool("auto_parallel", d.auto_parallel),
        heuristics_mode: v
            .get("heuristics_mode")
            .and_then(|x| x.as_str())
            .map(str::to_owned)
            .unwrap_or(d.heuristics_mode),
        prefer_inner_rayon: get_bool("prefer_inner_rayon", d.prefer_inner_rayon),
        tune_enabled: get_bool("tune_enabled", d.tune_enabled),
        tune_interval_ms: get_u64("tune_interval_ms", d.tune_interval_ms),
        win_allow_handle: get_bool("win_allow_handle", d.win_allow_handle),
        win_handle_sample_every: get_u64("win_handle_sample_every", d.win_handle_sample_every),
    }
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
    let mut args = Args::parse();
    // GNU du: -b is equivalent to --apparent-size --block-size=1
    if args.bytes {
        args.apparent_size = true;
    }
    let cfg = load_config();

    let mut exclude_contains: Vec<String> = args
        .exclude
        .as_deref()
        .unwrap_or("")
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

    // Deliberately more workers than cores: see hyperdu_core::default_threads.
    let threads = args.threads.unwrap_or_else(hyperdu_core::default_threads);

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
            io_profile: args.io_profile.map(Into::into),
            prefetch: args.prefetch,
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
            // Fastest, but still answering the question that was asked.
            //
            // This used to set `approximate_sizes`, which charges every file a
            // flat 4KiB. Measured against `du -sxb` that is wrong by -95% on
            // /var/lib and +149% on /etc -- and the sign is not consistent, so
            // a caller cannot correct for it. "Fast" is a request for a quicker
            // answer, not for a different one, so the guess is opt-in via
            // --approximate rather than implied here. See #29.
            opt.compute_physical = false;
            opt.count_hardlinks = true; // do not dedupe
                                        // keep compat in HyperDU unless明示
                                        // io_uring のチューニングはオプトインのままにする。
                                        // SQPOLL はワーカーごとに ring を持つ設計と噛み合わず、
                                        // カーネル側の polling スレッドが CPU を奪うため、
                                        // 小さなツリーや CPU 数の少ない環境では逆効果になる。
                                        // 必要なら --uring-sqpoll で明示的に有効化する。
                                        // 初期バッチ/深さを強めに（ライブチューナが追従）
                                        // 簡易ヒューリスティクス（環境変数で上書き可）
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
    // The io_uring backend was removed; its flags are still accepted so
    // existing scripts keep working, but they no longer do anything.
    if args.no_uring
        || args.uring_sqpoll
        || args.uring_coop
        || args.uring_batch.is_some()
        || args.uring_depth.is_some()
        || args.uring_sqpoll_idle_ms.is_some()
        || args.uring_sqpoll_cpu.is_some()
    {
        eprintln!(
            "warning: io_uring バックエンドは削除されました。--uring-* / --no-uring は無視されます。"
        );
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(kb) = args.getdents_buf_kb {
            std::env::set_var("HYPERDU_GETDENTS_BUF_KB", kb.to_string());
        }
        if args.pin_threads {
            std::env::set_var("HYPERDU_PIN_THREADS", "1");
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
    // Placed after every path that can set the flag, so `--perf turbo` -- which
    // enables it without the user typing --approximate -- is caught too.
    //
    // Measured against `du -sxb`: +149% on /etc, -95% on /var/lib. The sign is
    // not even consistent, so a caller cannot correct for it. Saying so costs
    // one line on stderr and stops the totals being read as facts. See #29.
    if opt.approximate_sizes {
        eprintln!(
            "warning: 概算モードが有効です。全ファイルを 4KiB とみなすため、\n\
             \x20        合計は実測で -95%〜+149% 外れます（誤差の向きも一定しません）。\n\
             \x20        正確な値が要る場合はこのモードを外してください。"
        );
    }
    opt.use_mft = args.mft;
    opt.one_file_system = args.one_file_system;
    if args.follow_links && !matches!(opt.compat_mode, hyperdu_core::CompatMode::HyperDU) {
        opt.visited_bloom = Some(std::sync::Arc::new(hyperdu_core::Bloom::with_bits(1 << 20)));
        opt.visited_dirs = Some(std::sync::Arc::new(dashmap::DashMap::with_capacity(1024)));
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
        let root_probe = args
            .roots
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."));

        let measure = |yield_every: usize, log: bool| -> Option<(f64, u64, f64)> {
            probe
                .dir_yield_every
                .store(yield_every, std::sync::atomic::Ordering::Relaxed);
            let ts = std::time::Instant::now();
            let map = hyperdu_core::scan_directory(&root_probe, &probe).ok()?;
            let dt = ts.elapsed().as_secs_f64().max(1e-6);
            let total = *map
                .get(&root_probe)
                .unwrap_or(&hyperdu_core::Stat::default());
            let rate = (total.files as f64) / dt;
            if log {
                println!(
                    "tune: yield={} -> {:.0} files/s (files={})",
                    yield_every, rate, total.files
                );
            }
            Some((rate, total.files, dt))
        };

        let t_start = std::time::Instant::now();
        let mut best_yield = 65536usize;
        let mut best_rate = 0.0f64;
        for &y in &candidates {
            if let Some((rate, _, _)) = measure(y, true) {
                if rate > best_rate {
                    best_rate = rate;
                    best_yield = y;
                }
            }
            if t_start.elapsed().as_secs_f64() >= secs {
                break;
            }
        }

        if best_rate > 0.0 && t_start.elapsed().as_secs_f64() < secs {
            let mut confirm_runs = 0usize;
            let mut confirm_files = 0u64;
            let mut confirm_dt = 0.0f64;
            while t_start.elapsed().as_secs_f64() < secs {
                if let Some((_, files, dt)) = measure(best_yield, false) {
                    confirm_runs += 1;
                    confirm_files += files;
                    confirm_dt += dt;
                } else {
                    break;
                }
            }
            if confirm_runs > 0 {
                let avg_rate = (confirm_files as f64) / confirm_dt.max(1e-6);
                println!(
                    "tune: confirm yield={best_yield} -> {avg_rate:.0} files/s avg over {confirm_runs} runs (files={confirm_files})"
                );
            }
        }
        println!("recommended.dir_yield_every={best_yield}");
        println!("hint.nvme=65536-131072, hint.hdd=8192-16384");

        fn shell_quote(token: &str) -> String {
            if token.is_empty() {
                return "\"\"".to_string();
            }
            if token.chars().any(|c| c.is_whitespace() || c == '"') {
                let escaped = token.replace('"', "\\\"");
                format!("\"{escaped}\"")
            } else {
                token.to_string()
            }
        }

        let wants_logical_only = !opt.compute_physical;
        let wants_approximate = opt.approximate_sizes;
        let wants_count_links = opt.count_hardlinks;
        let threads_recommended = opt.threads;

        let raw_args: Vec<String> = std::env::args().skip(1).collect();
        let mut filtered_args: Vec<String> = Vec::with_capacity(raw_args.len());
        let mut had_threads = false;
        let mut had_logical_only = false;
        let mut had_approximate = false;
        let mut had_count_links = false;
        #[cfg(target_os = "windows")]
        let mut had_win_ntquery = false;

        let mut i = 0;
        while i < raw_args.len() {
            let arg = &raw_args[i];
            if arg == "--tune-only" || arg.starts_with("--tune-only=") {
                i += 1;
                continue;
            }
            if arg == "--tune-secs" {
                i += 2;
                continue;
            }
            if arg.starts_with("--tune-secs=") {
                i += 1;
                continue;
            }
            if arg == "--dir-yield-every" {
                i += 2;
                continue;
            }
            if arg.starts_with("--dir-yield-every=") {
                i += 1;
                continue;
            }
            if arg == "--perf" {
                if let Some(next) = raw_args.get(i + 1) {
                    if next.eq_ignore_ascii_case("turbo") {
                        i += 2;
                        continue;
                    }
                    filtered_args.push(arg.clone());
                    filtered_args.push(next.clone());
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }
            if let Some(value) = arg.strip_prefix("--perf=") {
                if value.eq_ignore_ascii_case("turbo") {
                    i += 1;
                    continue;
                }
                filtered_args.push(arg.clone());
                i += 1;
                continue;
            }
            if arg == "--threads" {
                had_threads = true;
                filtered_args.push(arg.clone());
                if let Some(next) = raw_args.get(i + 1) {
                    filtered_args.push(next.clone());
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if arg.starts_with("--threads=") {
                had_threads = true;
                filtered_args.push(arg.clone());
                i += 1;
                continue;
            }
            if arg == "--logical-only" {
                had_logical_only = true;
                filtered_args.push(arg.clone());
                i += 1;
                continue;
            }
            if arg == "--approximate" {
                had_approximate = true;
                filtered_args.push(arg.clone());
                i += 1;
                continue;
            }
            if arg == "--count-links" {
                had_count_links = true;
                filtered_args.push(arg.clone());
                i += 1;
                continue;
            }
            if arg == "--no-count-links" {
                had_count_links = true;
                filtered_args.push(arg.clone());
                i += 1;
                continue;
            }
            #[cfg(target_os = "windows")]
            if arg == "--win-ntquery" {
                had_win_ntquery = true;
                filtered_args.push(arg.clone());
                i += 1;
                continue;
            }
            filtered_args.push(arg.clone());
            i += 1;
        }

        let mut recommended_args: Vec<String> = Vec::new();
        if !had_threads && threads_recommended > 0 {
            recommended_args.push("--threads".to_string());
            recommended_args.push(threads_recommended.to_string());
        }
        if wants_logical_only && !had_logical_only {
            recommended_args.push("--logical-only".to_string());
        }
        if wants_approximate && !had_approximate {
            recommended_args.push("--approximate".to_string());
        }
        if wants_count_links && !had_count_links {
            recommended_args.push("--count-links".to_string());
        }
        #[cfg(target_os = "windows")]
        if !had_win_ntquery {
            recommended_args.push("--win-ntquery".to_string());
        }
        recommended_args.push("--dir-yield-every".to_string());
        recommended_args.push(best_yield.to_string());

        let exe_display = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|name| name.to_string_lossy().to_string()))
            .map(|name| format!("./{name}"))
            .unwrap_or_else(|| "./hyperdu-cli".to_string());

        let mut pieces = Vec::with_capacity(1 + recommended_args.len() + filtered_args.len());
        pieces.push(shell_quote(&exe_display));
        for arg in &recommended_args {
            pieces.push(shell_quote(arg));
        }
        for arg in &filtered_args {
            pieces.push(shell_quote(arg));
        }
        let recommended_cmd = pieces.join(" ");
        println!("recommended.command={recommended_cmd}");
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
            .store(n, std::sync::atomic::Ordering::Relaxed);
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
    }));
    // Keep-alive: emit periodic status if no progress callback fired recently
    let _keepalive = KeepAlive::start(print_progress, last.clone());
    if args.progress {
        opt.progress_sample_callback = Some(std::sync::Arc::new(
            move |s: &hyperdu_core::ProgressSample<'_>| {
                println!(
                    "  sample: {} (size: {})",
                    short_path(s.path),
                    format_size(s.logical, BINARY)
                );
            },
        ));
    }

    // Whether `--mft` could apply to this path at all. Elevation and the
    // filesystem type are checked in the core, which is where the fallback
    // lives; this only catches the case the user can see and fix themselves.
    fn looks_like_volume_root(p: &std::path::Path) -> bool {
        #[cfg(windows)]
        {
            use std::path::{Component, Prefix};
            let mut c = p.components();
            let is_disk = matches!(
                c.next(),
                Some(Component::Prefix(pre))
                    if matches!(pre.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
            );
            is_disk && matches!(c.next(), Some(Component::RootDir)) && c.next().is_none()
        }
        #[cfg(not(windows))]
        {
            let _ = p;
            false
        }
    }

    // Roots: if none provided, use current directory
    let roots: Vec<PathBuf> = if args.roots.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.roots.clone()
    };

    // Falling back silently would let someone believe they had measured the
    // volume the fast way when they had not. The scan is still correct, so this
    // is a warning rather than an error.
    if args.mft && !roots.iter().any(|r| looks_like_volume_root(r)) {
        eprintln!(
            "warning: --mft はボリュームのルート（例: C:\\）にのみ適用されます。\n\
             \x20        指定された対象では通常の列挙で走査します。"
        );
    }

    // Quick Win: Minimal FS detection to improve defaults on DrvFS/Network FS
    #[cfg(target_os = "linux")]
    {
        if std::env::var("HYPERDU_FS_AUTO").ok().as_deref() != Some("0") {
            if let Some(root0) = roots.first() {
                if let Some(rep) = hyperdu_core::fs_strategy::detect_and_apply(root0, &mut opt) {
                    // Apply optional suggestions at CLI level (respect user overrides)
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
                    if rep.recommend_logical_only {
                        meta.push("hint=logical-only".into());
                    }
                    if !rep.changes.is_empty() {
                        meta.push(format!("changes=[{}]", rep.changes.join(",")));
                    }
                    // Diagnostics go to stderr so du-compatible stdout stays parsable.
                    eprintln!("fs-auto: {} for '{}'", meta.join(" "), root0.display());
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        if std::env::var("HYPERDU_FS_AUTO").ok().as_deref() != Some("0") {
            if let Some(root0) = roots.first() {
                // Diagnostics go to stderr so du-compatible stdout stays parsable.
                eprintln!(
                    "fs-auto: fs='unknown' strategy='generic' reason='platform=non-linux' for '{}'",
                    root0.display()
                );
            }
        }
    }
    let mut total_dt = std::time::Duration::from_secs(0);
    let mut exit_code = 0i32;

    if matches!(opt.compat_mode, hyperdu_core::CompatMode::HyperDU) {
        let root = roots.first().expect("at least one root").clone();
        let t0 = std::time::Instant::now();
        // Every root is scanned, and roots on different devices are scanned at
        // the same time. Reporting only the first was surprising for the very
        // case multiple roots exist for: comparing two drives.
        let mut map: hyperdu_core::StatMap = Default::default();
        let mut total_stat = hyperdu_core::Stat::default();
        let mut root_totals: Vec<(PathBuf, hyperdu_core::Stat)> = Vec::new();
        for (r, res) in hyperdu_core::scan_roots(&roots, &opt) {
            let m = res?;
            let s = *m.get(&r).unwrap_or(&hyperdu_core::Stat::default());
            total_stat.logical += s.logical;
            total_stat.physical += s.physical;
            total_stat.files += s.files;
            root_totals.push((r, s));
            // Paths are absolute, so two roots cannot collide unless one
            // contains the other, in which case the inner one's entries are
            // identical and overwriting is correct.
            map.extend(m);
        }
        let dt = t0.elapsed();
        total_dt += dt;
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
        if root_totals.len() == 1 {
            println!("  Root: {}", root.display());
        } else {
            println!("  Roots: {} (別デバイスは並列に走査)", root_totals.len());
            for (r, s) in &root_totals {
                println!(
                    "    {} | phys={} | log={} | files={}",
                    r.display(),
                    format_size(s.physical, BINARY),
                    format_size(s.logical, BINARY),
                    s.files
                );
            }
        }
        println!("  Elapsed: {:.3}s", dt.as_secs_f64());
        // The profile may cap the worker count; report what actually runs.
        println!("  Threads: {}", hyperdu_core::effective_threads(&opt));
        // Print the active exclusions. They change the totals, so a comparison
        // against du that does not account for them is not a fair one.
        if opt.exclude_contains.is_empty()
            && opt.exclude_regex.is_empty()
            && opt.exclude_glob.is_empty()
        {
            println!("  Excludes: (none)");
        } else {
            let mut parts: Vec<String> = Vec::new();
            if !opt.exclude_contains.is_empty() {
                parts.push(format!("contains[{}]", opt.exclude_contains.join(",")));
            }
            if !opt.exclude_regex.is_empty() {
                parts.push(format!("regex[{}]", opt.exclude_regex.join(",")));
            }
            if !opt.exclude_glob.is_empty() {
                parts.push(format!("glob[{}]", opt.exclude_glob.join(",")));
            }
            println!("  Excludes: {}", parts.join(" "));
        }
        println!("  Follow links: {}", args.follow_links);
        println!(
            "  Total: files={} | phys={} | log={} | dirs={}",
            total_stat.files,
            format_size(total_stat.physical, BINARY),
            format_size(total_stat.logical, BINARY),
            dirs_scanned
        );

        // Disk/Volume usage (best-effort)
        if let Some((vol_total, vol_free)) = fs_total_free(&root) {
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
            let class_stats = hyperdu_core::classify::classify_directory(&root, &opt, cmode);
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
