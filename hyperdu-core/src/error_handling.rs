use std::{
    fmt,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

use crate::Options;

/// Recommended next step when a recoverable error occurs.
#[derive(Debug, Clone, Copy)]
pub enum RecoveryAction {
    SkipEntry,
    SkipDirectory,
    Retry,
    Abort,
}

/// Typed scan errors; used for diagnostics and recovery hints.
#[derive(Debug)]
pub enum ScanError {
    IoError {
        path: PathBuf,
        source: std::io::Error,
    },
    PermissionDenied {
        path: PathBuf,
    },
    InvalidPath {
        path: PathBuf,
    },
    SystemCall {
        path: PathBuf,
        call: &'static str,
        errno: i32,
    },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::IoError { path, source } => write!(f, "{}: {}", path.display(), source),
            ScanError::PermissionDenied { path } => {
                write!(f, "{}: permission denied", path.display())
            }
            ScanError::InvalidPath { path } => write!(f, "{}: invalid path", path.display()),
            ScanError::SystemCall { path, call, errno } => {
                write!(f, "{}: {} failed (errno={})", path.display(), call, errno)
            }
        }
    }
}

pub type ScanResult<T> = Result<T, ScanError>;

pub trait ErrorRecovery {
    fn is_recoverable(&self) -> bool;
    fn recovery_action(&self) -> RecoveryAction;
}

impl ErrorRecovery for ScanError {
    fn is_recoverable(&self) -> bool {
        matches!(
            self.recovery_action(),
            RecoveryAction::SkipEntry | RecoveryAction::SkipDirectory | RecoveryAction::Retry
        )
    }
    fn recovery_action(&self) -> RecoveryAction {
        match self {
            ScanError::PermissionDenied { .. } => RecoveryAction::SkipEntry,
            ScanError::InvalidPath { .. } => RecoveryAction::SkipEntry,
            ScanError::IoError { source, .. } => match source.kind() {
                std::io::ErrorKind::NotFound => RecoveryAction::SkipEntry,
                std::io::ErrorKind::PermissionDenied => RecoveryAction::SkipEntry,
                std::io::ErrorKind::Interrupted => RecoveryAction::Retry,
                std::io::ErrorKind::WouldBlock => RecoveryAction::Retry,
                _ => RecoveryAction::SkipEntry,
            },
            ScanError::SystemCall { errno, .. } => match *errno {
                // EACCES, EPERM
                13 | 1 => RecoveryAction::SkipEntry,
                // ENOENT, ENOTDIR
                2 | 20 => RecoveryAction::SkipEntry,
                // EBUSY, EAGAIN
                16 | 11 => RecoveryAction::Retry,
                _ => RecoveryAction::SkipEntry,
            },
        }
    }
}

/// Increment counters and notify callback with a formatted typed error.
#[inline]
pub fn record_error(opt: &Options, err: &ScanError) {
    opt.error_count.fetch_add(1, Ordering::Relaxed);
    if let Some(cb) = &opt.error_report {
        cb(&err.to_string());
    }
}

/// Compatibility shim: stringly-typed error report (legacy call sites)
#[inline]
pub fn report_error(opt: &Options, path: &Path, error: &str) {
    opt.error_count.fetch_add(1, Ordering::Relaxed);
    if let Some(cb) = &opt.error_report {
        cb(&format!("{}: {}", path.display(), error));
    }
}

/// Try an operation with typed error reporting and early return
#[macro_export]
macro_rules! try_with_error {
    ($opt:expr, $path:expr, $op:expr, $call:expr) => {
        match $op {
            Ok(val) => val,
            Err(e) => {
                let p = std::path::PathBuf::from($path);
                let se = $crate::error_handling::ScanError::IoError { path: p, source: e };
                $crate::error_handling::record_error($opt, &se);
                return;
            }
        }
    };
}

/// Try an operation, continue loop on typed error
#[macro_export]
macro_rules! try_or_continue {
    ($opt:expr, $path:expr, $op:expr, $call:expr) => {
        match $op {
            Ok(val) => val,
            Err(e) => {
                let p = std::path::PathBuf::from($path);
                let se = $crate::error_handling::ScanError::IoError { path: p, source: e };
                $crate::error_handling::record_error($opt, &se);
                continue;
            }
        }
    };
}

/// Build a SystemCall error from the last OS error (errno/GetLastError)
#[inline]
pub fn last_os_error_systemcall(path: &Path, call: &'static str) -> ScanError {
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(-1);
    ScanError::SystemCall {
        path: path.to_path_buf(),
        call,
        errno,
    }
}
