/// Platform-specific constants
pub mod platform {
    /// Default buffer size for getdents64 on Linux
    pub const LINUX_GETDENTS_BUFFER_SIZE: usize = 64 * 1024;

    /// Default buffer size for getattrlistbulk on macOS
    pub const MACOS_GALB_BUFFER_SIZE: usize = 64 * 1024;

    /// Windows path buffer initial capacity
    pub const WINDOWS_PATH_BUFFER_CAPACITY: usize = 260;

    /// Long path prefix for Windows
    pub const WINDOWS_LONG_PATH_PREFIX: &[u16] = &['\\' as u16, '\\' as u16, '?' as u16, '\\' as u16];
}

/// Performance tuning constants
pub mod perf {
    /// Number of entries to process before checking yield condition
    pub const YIELD_CHECK_INTERVAL: usize = 4096;

    /// Default directory yield interval
    pub const DEFAULT_DIR_YIELD: usize = 16384;

    /// Memory pool shrink threshold
    pub const MEMORY_POOL_SHRINK_THRESHOLD: usize = 4 << 20; // 4MB

    /// Memory pool target size after shrink
    pub const MEMORY_POOL_SHRINK_TARGET: usize = 1 << 20; // 1MB
}

/// File size constants
pub mod sizes {
    /// Default estimated file size for approximate mode
    pub const APPROXIMATE_FILE_SIZE: u64 = 4096;

    /// Block size for physical size calculation
    pub const BLOCK_SIZE: u64 = 512;
}