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

    /// macOS: Use diskutil info to detect SSD
    #[cfg(target_os = "macos")]
    fn detect_macos(path: &std::path::Path) -> Option<StorageKind> {
        use std::process::Command;

        // Get the mount point for the path
        let mount_point = Self::get_macos_mount_point(path)?;

        // Run diskutil info to get disk properties
        let output = Command::new("diskutil")
            .args(["info", &mount_point])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Check for "Solid State" or "SSD" in the output
        // diskutil reports "Solid State: Yes" for SSDs
        for line in stdout.lines() {
            let line_lower = line.to_lowercase();
            if line_lower.contains("solid state") {
                if line_lower.contains("yes") {
                    return Some(StorageKind::Ssd);
                } else if line_lower.contains("no") {
                    return Some(StorageKind::Hdd);
                }
            }
        }

        // Alternative: Check if it's APFS (typically SSD)
        // or if media name contains SSD
        if stdout.to_lowercase().contains("ssd") {
            return Some(StorageKind::Ssd);
        }

        None
    }

    /// Get mount point for a path on macOS
    #[cfg(target_os = "macos")]
    fn get_macos_mount_point(path: &std::path::Path) -> Option<String> {
        use std::process::Command;

        let output = Command::new("df").arg(path).output().ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        // Parse df output: second line contains mount point
        let lines: Vec<&str> = stdout.lines().collect();
        if lines.len() >= 2 {
            // The mount point is the last column
            let parts: Vec<&str> = lines[1].split_whitespace().collect();
            if parts.len() >= 9 {
                return Some(parts[8].to_string());
            }
        }

        None
    }

    /// Windows: Use WMI to detect SSD via Win32_DiskDrive
    #[cfg(target_os = "windows")]
    fn detect_windows(path: &std::path::Path) -> Option<StorageKind> {
        // Try to get the drive letter from the path
        let drive = Self::get_windows_drive(path)?;

        // Use WMI to query disk properties via PowerShell
        Self::query_wmi_disk_type(&drive)
            .or_else(|| Self::detect_windows_by_fs_characteristics(path))
    }

    /// Get Windows drive letter from path (e.g., "C:\\")
    #[cfg(target_os = "windows")]
    fn get_windows_drive(path: &std::path::Path) -> Option<String> {
        let canonical = std::fs::canonicalize(path).ok()?;
        let path_str = canonical.to_str()?;

        // Extract drive letter (format: \\?\\C:\\...)
        if let Some(idx) = path_str.find(':') {
            if idx >= 1 {
                let drive_letter = &path_str[idx - 1..idx];
                return Some(format!("{}:", drive_letter.to_uppercase()));
            }
        }

        None
    }

    /// Query WMI for disk type using PowerShell
    #[cfg(target_os = "windows")]
    fn query_wmi_disk_type(drive: &str) -> Option<StorageKind> {
        use std::process::Command;

        // PowerShell command to get disk drive info via WMI
        let ps_cmd = format!(
            "Get-WmiObject -Class Win32_DiskDrive | Where-Object {{ $_.DeviceID -like '*{}*' }} | Select-Object -ExpandProperty MediaType",
            drive.trim_end_matches(':').to_lowercase()
        );

        let output = Command::new("powershell")
            .args(["-Command", &ps_cmd])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();

        if stdout.contains("ssd") || stdout.contains("solid state") {
            return Some(StorageKind::Ssd);
        } else if stdout.contains("hdd") || stdout.contains("fixed hard disk") {
            return Some(StorageKind::Hdd);
        }

        None
    }

    /// Fallback: Detect SSD on Windows by checking NTFS characteristics
    #[cfg(target_os = "windows")]
    fn detect_windows_by_fs_characteristics(path: &std::path::Path) -> Option<StorageKind> {
        use std::process::Command;

        let output = Command::new("fsutil")
            .args(["behavior", "query", "DisableDeleteNotify"])
            .output()
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();

        // If TRIM is enabled (DisableDeleteNotify = 0), it's likely an SSD
        if stdout.contains("disabledeletenotify = 0") {
            // TRIM enabled suggests SSD
            return Some(StorageKind::Ssd);
        }

        // Check if path is on a drive with seek penalty
        // This is a heuristic - SSDs generally have 0 seek penalty
        let canonical = std::fs::canonicalize(path).ok()?;
        let volume = canonical.to_str()?.get(0..3)?;

        let output = Command::new("fsutil")
            .args(["fsinfo", "ntfsinfo", volume])
            .output()
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);

        // If Bytes Per Cluster is small (4KB), it's more likely an SSD
        // This is a heuristic fallback
        if stdout.contains("Bytes Per Cluster : 4096") {
            return Some(StorageKind::Ssd);
        }

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
