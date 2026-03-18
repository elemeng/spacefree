use crate::storage::StorageKind;
use globset::{GlobMatcher, GlobSet};
use std::path::PathBuf;

/// Configuration for delete operations
#[derive(Clone)]
#[allow(dead_code)]
pub struct DeleteConfig {
    pub use_trash: bool,
    pub dry_run: bool,
    pub parallelism: usize,
    pub min_size: u64,
    pub max_size: Option<u64>,
    pub min_age: Option<u64>,
    pub max_age: Option<u64>,
    pub verbose: bool,
    pub dirs: bool,
    pub no_follow_symlinks: bool,
    pub glob_pattern: String,
    pub glob_matcher: GlobSet,
    pub exclude_matcher: Option<GlobMatcher>,
    /// True if glob_pattern is "**/*" (matches all) - allows skipping glob check
    pub skip_glob_match: bool,
    /// Storage type for adaptive optimization
    pub storage_kind: StorageKind,
}

/// Result from scanning a file or directory
#[derive(Clone)]
pub struct ScanResult {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
}
