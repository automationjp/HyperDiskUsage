use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use crate::Options;

pub struct TunerGuard {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for TunerGuard {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Start adaptive tuner if HYPERDU_TUNE=1. Returns a guard joining when dropped.
pub fn start_if_enabled(
    opt: Arc<Options>,
    total_files: Arc<std::sync::atomic::AtomicU64>,
) -> Option<TunerGuard> {
    if std::env::var("HYPERDU_TUNE").ok().as_deref() != Some("1") {
        return None;
    }
    let running = Arc::new(AtomicBool::new(true));
    let running_c = running.clone();
    // Yield candidates (0 = disabled)
    let yield_steps: Arc<Vec<usize>> = Arc::new(vec![0, 4096, 8192, 16384, 32768, 65536, 131072]);

    // State shared with the thread
    let state = Arc::new(Mutex::new(TuneState::default()));

    let handle = std::thread::Builder::new()
        .name("hyperdu-tuner".into())
        .spawn(move || {
            let interval = std::env::var("HYPERDU_TUNE_INTERVAL_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(800);
            let log_enabled = std::env::var("HYPERDU_TUNE_LOG").ok().as_deref() == Some("1");
            let mut last_t = Instant::now();
            let mut last_n = total_files.load(Ordering::Relaxed);
            // uring counters
            let mut last_fail = opt.uring_sqe_fail.load(Ordering::Relaxed);
            let mut last_wait = opt.uring_submit_wait_ns.load(Ordering::Relaxed);
            let mut last_cqe = opt.uring_cqe_comp.load(Ordering::Relaxed);
            // init yield idx from current value
            let mut idx = {
                let cur = opt.dir_yield_every.load(Ordering::Relaxed);
                yield_steps
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, &v)| v.abs_diff(cur))
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            };
            let mut dir: isize = 1;
            let mut last_fps: f64 = 0.0;
            let mut last_yield = opt.dir_yield_every.load(Ordering::Relaxed);
            let mut last_batch_logged = opt.uring_batch.load(Ordering::Relaxed);
            let mut last_active_logged = opt.active_threads.load(Ordering::Relaxed);
            loop {
                if !running_c.load(Ordering::Relaxed) || opt.cancel.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(interval));
                let now = Instant::now();
                let dt = now.duration_since(last_t).as_secs_f64().max(1e-3);
                last_t = now;
                // Throughput
                let cur_n = total_files.load(Ordering::Relaxed);
                let dn = cur_n.saturating_sub(last_n) as f64;
                last_n = cur_n;
                let fps = dn / dt;

                // Update yield based on throughput trend
                {
                    let mut st = state.lock().unwrap();
                    let improve = if st.best_fps > 0.0 {
                        (fps - st.best_fps) / st.best_fps
                    } else {
                        0.0
                    };
                    // 5% threshold to change
                    if improve > 0.05 {
                        st.best_fps = fps;
                        // move one step in same direction if possible
                        let next = (idx as isize + dir).clamp(0, (yield_steps.len() - 1) as isize)
                            as usize;
                        if next != idx {
                            idx = next;
                            opt.dir_yield_every
                                .store(yield_steps[idx], Ordering::Relaxed);
                            let newy = yield_steps[idx];
                            if log_enabled && newy != last_yield {
                                println!("[tune] dir_yield_every -> {}", newy);
                            }
                            last_yield = newy;
                        }
                    } else {
                        // reverse direction and try one step
                        dir = -dir;
                        let next = (idx as isize + dir).clamp(0, (yield_steps.len() - 1) as isize)
                            as usize;
                        if next != idx {
                            idx = next;
                            opt.dir_yield_every
                                .store(yield_steps[idx], Ordering::Relaxed);
                            let newy = yield_steps[idx];
                            if log_enabled && newy != last_yield {
                                println!("[tune] dir_yield_every -> {}", newy);
                            }
                            last_yield = newy;
                        }
                    }
                }

                // io_uring tuning (Linux only, but counters exist regardless)
                let cur_fail = opt.uring_sqe_fail.load(Ordering::Relaxed);
                let cur_wait = opt.uring_submit_wait_ns.load(Ordering::Relaxed);
                let cur_cqe = opt.uring_cqe_comp.load(Ordering::Relaxed);
                let dfail = cur_fail.saturating_sub(last_fail);
                let dwait = cur_wait.saturating_sub(last_wait);
                let dcqe = cur_cqe.saturating_sub(last_cqe);
                last_fail = cur_fail;
                last_wait = cur_wait;
                last_cqe = cur_cqe;
                let avg_wait_ms = if dcqe > 0 {
                    (dwait as f64) / (dcqe as f64) / 1.0e6
                } else {
                    0.0
                };
                let mut batch = opt.uring_batch.load(Ordering::Relaxed);
                // adjust batch within [64, 4096]
                let min_b = 64usize;
                let max_b = 4096usize;
                if dfail > 0 {
                    // queue was often full: be conservative
                    batch = batch.saturating_sub(64).max(min_b);
                } else if avg_wait_ms > 2.0 {
                    // high submit wait per CQE: reduce to lower latency
                    batch = batch.saturating_sub(32).max(min_b);
                } else {
                    // ramp up slowly
                    batch = (batch + 32).min(max_b);
                }
                opt.uring_batch.store(batch, Ordering::Relaxed);
                if log_enabled {
                    let cur_b = batch;
                    if cur_b != last_batch_logged {
                        println!("[tune] uring_batch -> {}", cur_b);
                        last_batch_logged = cur_b;
                    }
                }

                // Dynamic threads gating: adjust active_threads in [1, threads]
                let max_threads = opt.threads.max(1);
                let mut active = opt
                    .active_threads
                    .load(Ordering::Relaxed)
                    .clamp(1, max_threads);
                let fps_improve = if last_fps > 0.0 {
                    (fps - last_fps) / last_fps
                } else {
                    0.0
                };
                last_fps = fps;
                if dfail > 0 || avg_wait_ms > 3.0 {
                    // back off
                    if active > 1 {
                        active -= 1;
                    }
                } else if fps_improve > 0.05 && active < max_threads {
                    // ramp up slowly on clear improvement
                    active += 1;
                }
                opt.active_threads.store(active, Ordering::Relaxed);
                if log_enabled {
                    let cur_a = active;
                    if cur_a != last_active_logged {
                        println!("[tune] active_threads -> {}", cur_a);
                        last_active_logged = cur_a;
                    }
                }
            }
        })
        .ok()?;

    Some(TunerGuard {
        running,
        handle: Some(handle),
    })
}

#[derive(Default)]
struct TuneState {
    best_fps: f64,
}
