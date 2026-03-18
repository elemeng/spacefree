use crate::error::DeleterError;
use clap::Parser;
use globset::{Glob, GlobMatcher, GlobSet, GlobSetBuilder};
use std::path::PathBuf;

/// Command-line interface definition
#[derive(Parser, Debug)]
#[command(
    name = "spf",
    about = "🚀 Ultra-fast file deletion CLI tool (supports trash)",
    version
)]
pub struct Cli {
    /// Paths to scan - can be directories or files to delete (space separated)
    #[arg(required = true, value_name = "PATHS")]
    pub paths: Vec<PathBuf>,

    /// Path list file containing paths to scan (comma/space/newline separated)
    #[arg(long, value_name = "FILE")]
    pub path_list_file: Vec<PathBuf>,

    /// Glob pattern for files to delete [default: **/* (all files)]
    #[arg(short, long, value_name = "PATTERN")]
    pub glob: Option<String>,

    /// Glob pattern to exclude
    #[arg(long, value_name = "PATTERN")]
    pub exclude: Option<String>,

    /// Minimum file size (e.g., 100, 10k, 5M, 2G, 1T)
    #[arg(long, value_name = "SIZE", default_value = "0", value_parser = parse_size)]
    pub min_size: u64,

    /// Maximum file size (e.g., 100, 10k, 5M, 2G, 1T)
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub max_size: Option<u64>,

    /// Minimum file age (e.g., 1d, 2w, 3m, 1y) - only files older than this will be deleted
    #[arg(long, value_name = "AGE", value_parser = parse_age)]
    pub min_age: Option<u64>,

    /// Maximum file age (e.g., 1d, 2w, 3m, 1y) - only files newer than this will be deleted
    #[arg(long, value_name = "AGE", value_parser = parse_age)]
    pub max_age: Option<u64>,

    /// Move to system trash instead of permanent delete
    #[arg(long)]
    pub trash: bool,

    /// Preview what would be deleted without actually deleting
    #[arg(long)]
    pub dry_run: bool,

    /// Skip confirmation prompt
    #[arg(short, long)]
    pub yes: bool,

    /// Allow deleting root directory (requires -y as well)
    #[arg(long)]
    pub delete_root_dir: bool,

    /// Number of parallel workers (0 = auto-detect based on storage type)
    #[arg(short, long, default_value_t = 0, value_name = "N")]
    pub parallelism: usize,

    /// Show all files to be deleted (verbose mode)
    #[arg(short, long)]
    pub verbose: bool,

    /// Delete directories as well as files
    #[arg(long)]
    pub dirs: bool,

    /// Do not follow symbolic links during directory traversal
    #[arg(long)]
    pub no_follow_symlinks: bool,

    /// Log deleted items to file (use without value for auto-named log, or specify path)
    #[arg(short, long, value_name = "PATH", default_missing_value = "auto", num_args = 0..=1)]
    pub log: Option<String>,
}

/// Parse size string (e.g., "10M", "1G") into bytes
pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(0);
    }

    let (num_part, unit_part) = s
        .find(|c: char| !c.is_ascii_digit())
        .map(|i| s.split_at(i))
        .unwrap_or((s, ""));

    let num: u64 = num_part
        .parse()
        .map_err(|_| format!("invalid number: {}", num_part))?;
    let unit = unit_part.trim().to_uppercase();

    let multiplier = match unit.as_str() {
        "" | "B" => 1u64,
        "K" | "KB" => 1024,
        "M" | "MB" => 1024 * 1024,
        "G" | "GB" => 1024 * 1024 * 1024,
        "T" | "TB" => 1024_u64 * 1024 * 1024 * 1024,
        _ => return Err(format!("invalid unit: {}", unit_part)),
    };

    num.checked_mul(multiplier)
        .ok_or_else(|| "size overflow".to_string())
}

/// Parse age string (e.g., "1d", "2w") into seconds
pub fn parse_age(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("age cannot be empty".to_string());
    }

    let (num_part, unit_part) = s
        .find(|c: char| !c.is_ascii_digit())
        .map(|i| s.split_at(i))
        .unwrap_or((s, ""));

    let num: u64 = num_part
        .parse()
        .map_err(|_| format!("invalid number: {}", num_part))?;
    let unit = unit_part.trim().to_lowercase();

    let seconds = match unit.as_str() {
        "s" | "sec" | "second" | "seconds" => num,
        "m" | "min" | "minute" | "minutes" => num * 60,
        "h" | "hour" | "hours" => num * 3600,
        "d" | "day" | "days" => num * 86400,
        "w" | "week" | "weeks" => num * 604800,
        "M" | "month" | "months" => num * 2592000,
        "y" | "year" | "years" => num * 31536000,
        _ => return Err(format!("invalid age unit: {}", unit_part)),
    };

    Ok(seconds)
}

/// Build globset from include/exclude patterns
pub fn build_globset(
    include: Option<&str>,
    exclude: &Option<String>,
) -> Result<(GlobSet, Option<GlobMatcher>), DeleterError> {
    let mut builder = GlobSetBuilder::new();

    let pattern = include.unwrap_or("**/*");
    builder.add(Glob::new(pattern).map_err(|e| DeleterError::Glob(e.to_string()))?);

    let globset = builder
        .build()
        .map_err(|e| DeleterError::Glob(e.to_string()))?;

    let exclude_matcher = exclude
        .as_ref()
        .map(|ex| {
            Glob::new(ex)
                .map_err(|e| DeleterError::Glob(e.to_string()))
                .map(|g| g.compile_matcher())
        })
        .transpose()?;

    Ok((globset, exclude_matcher))
}

/// Format list of directories for display
pub fn format_dirs(paths: &[PathBuf]) -> String {
    let dirs: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    match dirs.len() {
        0 => "directories".into(),
        1 => dirs[0].clone(),
        2 => format!("{} and {}", dirs[0], dirs[1]),
        _ => {
            let last = dirs
                .last()
                .expect("dirs should have at least 3 elements when len >= 3");
            let rest = &dirs[..dirs.len() - 1];
            format!("{}, and {}", rest.join(", "), last)
        }
    }
}

/// Check if a path points to a root directory
pub fn is_root_path(path: &std::path::Path) -> bool {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical.parent().is_none()
}

/// Format file size in human-readable format
pub fn format_size(size: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];

    let mut f = size as f64;
    let mut u = 0;

    while f >= 1024.0 && u < UNITS.len() - 1 {
        f /= 1024.0;
        u += 1;
    }

    if u == 0 {
        format!("{size} B")
    } else {
        format!("{f:.2} {}", UNITS[u])
    }
}
