use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clock {
    pub mono_ns: u64,
    pub unix_ns: u64,
    pub uncertainty_ns: u64,
    pub domain: String,
}

pub fn mono_ns() -> Result<u64> {
    #[cfg(unix)]
    {
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } == 0,
            "CLOCK_MONOTONIC unavailable"
        );
        Ok(u64::try_from(ts.tv_sec)? * 1_000_000_000 + u64::try_from(ts.tv_nsec)?)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Performance::{
            QueryPerformanceCounter, QueryPerformanceFrequency,
        };
        let (mut count, mut freq) = (0i64, 0i64);
        ensure!(
            unsafe { QueryPerformanceCounter(&mut count) } != 0
                && unsafe { QueryPerformanceFrequency(&mut freq) } != 0
                && freq > 0
                && count >= 0,
            "QPC unavailable"
        );
        Ok(((count as u128 * 1_000_000_000) / freq as u128).try_into()?)
    }
    #[cfg(not(any(unix, windows)))]
    anyhow::bail!("monotonic clock unsupported");
}

impl Clock {
    pub fn capture() -> Result<Self> {
        let before = mono_ns()?;
        let unix_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_nanos()
            .try_into()?;
        let after = mono_ns()?;
        ensure!(after >= before, "monotonic clock went backwards");
        Ok(Self {
            mono_ns: before + (after - before) / 2,
            unix_ns,
            uncertainty_ns: (after - before).div_ceil(2),
            domain: if cfg!(windows) {
                "QPC"
            } else {
                "CLOCK_MONOTONIC"
            }
            .into(),
        })
    }
    pub fn to_unix(&self, mono: u64) -> Result<u64> {
        Ok(u64::try_from(
            self.unix_ns as i128 + mono as i128 - self.mono_ns as i128,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_anchor_supports_events_before_and_after_anchor() {
        let c = Clock {
            mono_ns: 100,
            unix_ns: 1000,
            uncertainty_ns: 1,
            domain: "test".into(),
        };
        assert_eq!(c.to_unix(90).unwrap(), 990);
        assert_eq!(c.to_unix(120).unwrap(), 1020);
        assert_eq!(
            Clock::capture().unwrap().domain,
            if cfg!(windows) {
                "QPC"
            } else {
                "CLOCK_MONOTONIC"
            }
        );
    }
}
