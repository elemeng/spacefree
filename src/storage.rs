/// Storage device type for adaptive optimization
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StorageKind {
    /// Hard disk drive - requires sequential access optimization
    Hdd,
    /// Solid state drive - can handle high parallelism
    Ssd,
    /// Unknown - default to SSD behavior
    Unknown,
}

impl StorageKind {
    /// Detect storage type from a path
    pub fn from_path(path: &std::path::Path) -> Self {
        #[cfg(target_os = "linux")]
        {
            if let Some(kind) = Self::detect_linux(path) {
                return kind;
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(kind) = Self::detect_macos(path) {
                return kind;
            }
        }
        #[cfg(target_os = "windows")]
        {
            if let Some(kind) = Self::detect_windows(path) {
                return kind;
            }
        }
        StorageKind::Unknown
    }

    /// Linux: check /sys/block/*/queue/rotational
    #[cfg(target_os = "linux")]
    fn detect_linux(path: &std::path::Path) -> Option<StorageKind> {
        use std::fs::read_to_string;
        use std::os::unix::fs::MetadataExt;

        // Get device ID
        let metadata = std::fs::metadata(path).ok()?;
        let _dev = metadata.dev();

        // Try to find the block device
        let sys_block = std::path::Path::new("/sys/block");
        if let Ok(entries) = std::fs::read_dir(sys_block) {
            for entry in entries.flatten() {
                let rotational_path = entry.path().join("queue/rotational");
                if let Ok(content) = read_to_string(&rotational_path) {
                    let is_rotational = content.trim() == "1";
                    return Some(if is_rotational {
                        StorageKind::Hdd
                    } else {
                        StorageKind::Ssd
                    });
                }
            }
        }
        None
    }

    /// macOS: Use diskutil or check if it's an APFS SSD
    #[cfg(target_os = "macos")]
    fn detect_macos(_path: &std::path::Path) -> Option<StorageKind> {
        // macOS detection would require IOKit or diskutil parsing
        // For now, assume SSD on modern Macs
        Some(StorageKind::Ssd)
    }

    /// Windows: Check DeviceSeekPenalty
    #[cfg(target_os = "windows")]
    fn detect_windows(_path: &std::path::Path) -> Option<StorageKind> {
        // Windows detection would require WMI or DeviceIoControl
        // For now, return Unknown to use default behavior
        None
    }

    /// Get optimal parallelism for this storage type
    pub fn optimal_parallelism(&self) -> usize {
        match self {
            StorageKind::Hdd => 1,                       // Sequential for HDD
            StorageKind::Ssd => num_cpus::get() * 4,     // High parallelism for SSD
            StorageKind::Unknown => num_cpus::get() * 2, // Conservative default
        }
    }

    /// Whether to use sorted ordering for optimal access
    pub fn should_sort(&self) -> bool {
        matches!(self, StorageKind::Hdd)
    }
}
