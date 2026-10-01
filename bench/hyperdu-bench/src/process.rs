use anyhow::{bail, ensure, Result};
use serde_json::{json, Value};
use std::fs;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

/// Owns only this experiment's child/process group, never system-wide collectors.
pub struct OwnedChild(pub Child);
impl OwnedChild {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        #[cfg(unix)] {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        Ok(Self(command.spawn()?))
    }
    pub fn terminate(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            #[cfg(unix)] unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL); }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    pub fn wait(&mut self, timeout: Duration) -> Result<ExitStatus> {
        let start = Instant::now();
        loop {
            if let Some(status) = self.0.try_wait()? { return Ok(status); }
            if start.elapsed() >= timeout { self.terminate(); bail!("child timed out"); }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for OwnedChild { fn drop(&mut self) { self.terminate(); } }

#[cfg(target_os = "linux")]
pub fn snapshot(pid: u32) -> Result<Value> {
    let base = format!("/proc/{pid}");
    let raw = fs::read_to_string(format!("{base}/stat"))?;
    let close = raw.rfind(')').ok_or_else(|| anyhow::anyhow!("bad proc stat"))?;
    let fields: Vec<_> = raw[close+1..].split_whitespace().collect();
    ensure!(fields.len() > 21, "truncated proc stat");
    let number = |i: usize| -> Option<u64> { fields.get(i)?.parse().ok() };
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    ensure!(hz > 0 && page > 0, "invalid system clock/page configuration");
    let ns = |x: Option<u64>| x.map(|v| (v as u128 * 1_000_000_000 / hz as u128) as u64);
    let io = fs::read_to_string(format!("{base}/io")).ok();
    let status = fs::read_to_string(format!("{base}/status")).ok();
    fn field(text: &Option<String>, key: &str) -> Option<u64> {
        text.as_ref()?.lines().find_map(|line| line.strip_prefix(key)?.trim().parse().ok())
    }
    Ok(json!({
        "cpu.user_ns":ns(number(11)), "cpu.system_ns":ns(number(12)),
        "memory.rss_bytes":number(21).and_then(|v| v.checked_mul(page as u64)),
        "memory.minor_faults":number(7), "memory.major_faults":number(9),
        "process.threads":number(17),
        "io.read_bytes":field(&io,"read_bytes:"), "io.write_bytes":field(&io,"write_bytes:"),
        "io.logical_read_bytes":field(&io,"rchar:"), "io.logical_write_bytes":field(&io,"wchar:"),
        "scheduler.leader_voluntary_switches":field(&status,"voluntary_ctxt_switches:"),
        "scheduler.leader_involuntary_switches":field(&status,"nonvoluntary_ctxt_switches:"),
        "measurement.scope":"process; context-switch counters are leader-thread only"
    }))
}

#[cfg(windows)]
pub fn snapshot(pid: u32) -> Result<Value> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::{GetProcessIoCounters, GetProcessTimes, OpenProcess, IO_COUNTERS, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
        ensure!(!handle.is_null(), "cannot query process");
        let mut times: [FILETIME;4] = std::mem::zeroed();
        let cpu_ok = GetProcessTimes(handle, &mut times[0], &mut times[1], &mut times[2], &mut times[3]) != 0;
        let mut memory: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        let mem_ok = GetProcessMemoryInfo(handle, &mut memory, std::mem::size_of_val(&memory) as u32) != 0;
        let mut io: IO_COUNTERS = std::mem::zeroed();
        let io_ok = GetProcessIoCounters(handle, &mut io) != 0;
        CloseHandle(handle);
        let ns = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) * 100;
        Ok(json!({"cpu.user_ns":cpu_ok.then(|| ns(times[3])),
            "cpu.system_ns":cpu_ok.then(|| ns(times[2])),
            "memory.rss_bytes":mem_ok.then_some(memory.WorkingSetSize),
            "memory.peak_rss_bytes":mem_ok.then_some(memory.PeakWorkingSetSize),
            "io.logical_read_bytes":io_ok.then_some(io.ReadTransferCount),
            "io.logical_write_bytes":io_ok.then_some(io.WriteTransferCount),
            "io.read_operations":io_ok.then_some(io.ReadOperationCount),
            "io.write_operations":io_ok.then_some(io.WriteOperationCount),
            "measurement.scope":"process; transfer counters are not physical disk traffic"}))
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn snapshot(_pid: u32) -> Result<Value> { bail!("process metrics unsupported on this OS") }
