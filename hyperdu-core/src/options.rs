use crate::{CompatMode, Options};

// Grouped configuration types for clearer construction and composition
#[derive(Default, Clone)]
pub struct FilterConfig {
    pub exclude_contains: Vec<String>,
    pub exclude_regex: Vec<String>,
    pub exclude_glob: Vec<String>,
    pub max_depth: Option<u32>,
    pub min_file_size: Option<u64>,
}

#[derive(Default, Clone)]
pub struct PerformanceConfig {
    pub threads: Option<usize>,
    pub compute_physical: Option<bool>,
    pub approximate_sizes: Option<bool>,
    pub one_file_system: Option<bool>,
    pub follow_links: Option<bool>,
    pub prefer_inner_rayon: Option<bool>,
    pub disable_uring: Option<bool>,
}

#[derive(Default, Clone)]
pub struct OutputConfig {
    pub progress_every: Option<u64>,
}

#[derive(Default, Clone)]
pub struct CompatConfig {
    pub compat_mode: Option<CompatMode>,
    pub count_hardlinks: Option<bool>,
}

#[derive(Default, Clone)]
pub struct TuningConfig {
    pub tune_enabled: Option<bool>,
    pub tune_interval_ms: Option<u64>,
}

#[derive(Default, Clone)]
pub struct WindowsConfig {
    pub win_allow_handle: Option<bool>,
    pub win_handle_sample_every: Option<u64>,
}

#[derive(Default, Clone)]
pub struct OptionsBuilder {
    pub exclude_contains: Vec<String>,
    pub exclude_regex: Vec<String>,
    pub exclude_glob: Vec<String>,
    pub max_depth: Option<u32>,
    pub min_file_size: Option<u64>,
    pub follow_links: Option<bool>,
    pub threads: Option<usize>,
    pub compute_physical: Option<bool>,
    pub approximate_sizes: Option<bool>,
    pub one_file_system: Option<bool>,
    pub progress_every: Option<u64>,
    pub compat_mode: Option<CompatMode>,
    pub count_hardlinks: Option<bool>,
    pub tune_enabled: Option<bool>,
    pub tune_interval_ms: Option<u64>,
    pub prefer_inner_rayon: Option<bool>,
    pub disable_uring: Option<bool>,
    pub win_allow_handle: Option<bool>,
    pub win_handle_sample_every: Option<u64>,
}

impl OptionsBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_exclude_contains(mut self, list: impl IntoIterator<Item = String>) -> Self {
        self.exclude_contains = list.into_iter().collect();
        self
    }
    pub fn with_exclude_regex(mut self, list: impl IntoIterator<Item = String>) -> Self {
        self.exclude_regex = list.into_iter().collect();
        self
    }
    pub fn with_exclude_glob(mut self, list: impl IntoIterator<Item = String>) -> Self {
        self.exclude_glob = list.into_iter().collect();
        self
    }
    pub fn with_filters(mut self, cfg: FilterConfig) -> Self {
        if !cfg.exclude_contains.is_empty() {
            self.exclude_contains = cfg.exclude_contains;
        }
        if !cfg.exclude_regex.is_empty() {
            self.exclude_regex = cfg.exclude_regex;
        }
        if !cfg.exclude_glob.is_empty() {
            self.exclude_glob = cfg.exclude_glob;
        }
        self.max_depth = cfg.max_depth.or(self.max_depth);
        self.min_file_size = cfg.min_file_size.or(self.min_file_size);
        self
    }
    pub fn max_depth(mut self, v: u32) -> Self {
        self.max_depth = Some(v);
        self
    }
    pub fn min_file_size(mut self, v: u64) -> Self {
        self.min_file_size = Some(v);
        self
    }
    pub fn follow_links(mut self, v: bool) -> Self {
        self.follow_links = Some(v);
        self
    }
    pub fn threads(mut self, v: usize) -> Self {
        self.threads = Some(v);
        self
    }
    pub fn compute_physical(mut self, v: bool) -> Self {
        self.compute_physical = Some(v);
        self
    }
    pub fn approximate_sizes(mut self, v: bool) -> Self {
        self.approximate_sizes = Some(v);
        self
    }
    pub fn one_file_system(mut self, v: bool) -> Self {
        self.one_file_system = Some(v);
        self
    }
    pub fn with_performance(mut self, cfg: PerformanceConfig) -> Self {
        self.threads = cfg.threads.or(self.threads);
        self.compute_physical = cfg.compute_physical.or(self.compute_physical);
        self.approximate_sizes = cfg.approximate_sizes.or(self.approximate_sizes);
        self.one_file_system = cfg.one_file_system.or(self.one_file_system);
        self.follow_links = cfg.follow_links.or(self.follow_links);
        self.prefer_inner_rayon = cfg.prefer_inner_rayon.or(self.prefer_inner_rayon);
        self.disable_uring = cfg.disable_uring.or(self.disable_uring);
        self
    }
    pub fn progress_every(mut self, n: u64) -> Self {
        self.progress_every = Some(n);
        self
    }
    pub fn with_output(mut self, cfg: OutputConfig) -> Self {
        self.progress_every = cfg.progress_every.or(self.progress_every);
        self
    }
    pub fn compat_mode(mut self, m: CompatMode) -> Self {
        self.compat_mode = Some(m);
        self
    }
    pub fn count_hardlinks(mut self, v: bool) -> Self {
        self.count_hardlinks = Some(v);
        self
    }
    pub fn with_compat(mut self, cfg: CompatConfig) -> Self {
        self.compat_mode = cfg.compat_mode.or(self.compat_mode);
        self.count_hardlinks = cfg.count_hardlinks.or(self.count_hardlinks);
        self
    }
    pub fn with_tuning(mut self, cfg: TuningConfig) -> Self {
        self.tune_enabled = cfg.tune_enabled.or(self.tune_enabled);
        self.tune_interval_ms = cfg.tune_interval_ms.or(self.tune_interval_ms);
        self
    }
    pub fn with_windows(mut self, cfg: WindowsConfig) -> Self {
        self.win_allow_handle = cfg.win_allow_handle.or(self.win_allow_handle);
        self.win_handle_sample_every =
            cfg.win_handle_sample_every.or(self.win_handle_sample_every);
        self
    }

    pub fn build(self) -> Options {
        // Start from default to inherit tuned env defaults
        let mut opt = Options::default();
        if let Some(v) = self.max_depth {
            opt.max_depth = v;
        }
        if let Some(v) = self.min_file_size {
            opt.min_file_size = v;
        }
        if let Some(v) = self.follow_links {
            opt.follow_links = v;
        }
        if let Some(v) = self.threads { opt.threads = v; }
        if let Some(v) = self.compute_physical {
            opt.compute_physical = v;
        }
        if let Some(v) = self.approximate_sizes {
            opt.approximate_sizes = v;
        }
        if let Some(v) = self.one_file_system {
            opt.one_file_system = v;
        }
        if let Some(v) = self.progress_every {
            opt.progress_every = v;
        }
        if let Some(v) = self.compat_mode {
            opt.compat_mode = v;
        }
        if let Some(v) = self.count_hardlinks {
            opt.count_hardlinks = v;
        }
        if let Some(v) = self.tune_enabled {
            opt.tune_enabled = v;
        }
        if let Some(v) = self.tune_interval_ms {
            opt.tune_interval_ms = v;
        }
        if let Some(v) = self.prefer_inner_rayon {
            opt.prefer_inner_rayon = v;
        }
        if let Some(v) = self.disable_uring {
            opt.disable_uring = v;
        }
        if let Some(v) = self.win_allow_handle {
            opt.win_allow_handle = v;
        }
        if let Some(v) = self.win_handle_sample_every {
            opt.win_handle_sample_every = v;
        }
        if !self.exclude_contains.is_empty() {
            opt.exclude_contains = self.exclude_contains;
        }
        if !self.exclude_regex.is_empty() {
            opt.exclude_regex = self.exclude_regex;
        }
        if !self.exclude_glob.is_empty() {
            opt.exclude_glob = self.exclude_glob;
        }
        // Initialize runtime-tunable active_threads to full threads
        opt.active_threads.store(opt.threads.max(1), std::sync::atomic::Ordering::Relaxed);
        // Compile filters similar to scan bootstrap
        super::compile_filters_in_place(&mut opt);
        opt
    }
}
