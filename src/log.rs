use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Log entry for deleted items
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeletedItem {
    pub path: PathBuf,
    pub is_dir: bool,
    pub deleted_at: u64,
}

/// Log mode for deletion operations
#[derive(Debug, Clone)]
pub enum LogMode {
    /// No logging
    Disabled,
    /// Auto-generate log filename
    Auto,
    /// Use specific path
    Path(PathBuf),
}

impl LogMode {
    /// Parse log option from CLI string
    pub fn from_opt(opt: &Option<String>) -> Self {
        match opt {
            None => LogMode::Disabled,
            Some(s) if s == "auto" => LogMode::Auto,
            Some(s) => LogMode::Path(PathBuf::from(s)),
        }
    }

    /// Get the log path if logging is enabled
    pub fn path(&self) -> Option<PathBuf> {
        match self {
            LogMode::Disabled => None,
            LogMode::Auto => Some(generate_log_filename()),
            LogMode::Path(p) => Some(p.clone()),
        }
    }
}

/// Generate next available log filename (spacefree_0001.log, spacefree_0002.log, etc.)
/// Uses atomic create_new to avoid TOCTOU race conditions.
pub fn generate_log_filename() -> PathBuf {
    let mut counter = 1;
    loop {
        let filename = format!("spacefree_{:04}.log", counter);
        let path = PathBuf::from(&filename);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return path,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                counter += 1;
            }
            Err(_) => {
                // For other errors, try next filename
                counter += 1;
            }
        }
    }
}
