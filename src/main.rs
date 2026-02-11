use clap::Parser;
use futures::{StreamExt, stream};
use globset::{Glob, GlobSet, GlobSetBuilder};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{self, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::fs;
use tracing::{debug, error, info, warn};
use trash::delete as trash_delete;
use walkdir::WalkDir;

//
// ──────────────────────────────────────────────────────────
// Errors
// ──────────────────────────────────────────────────────────
//

#[derive(Error, Debug)]
enum DeleterError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Invalid job directory: {0}")]
    JobDir(PathBuf),

    #[error("No valid paths provided")]
    NoValidPaths,

    #[error("User cancelled")]
    Cancelled,

    #[error("Task join error")]
    Join,

    #[error("Invalid glob: {0}")]
    Glob(String),

    #[error("Progress bar error: {0}")]
    ProgressBar(String),

}

//
// ──────────────────────────────────────────────────────────
// Delete Log
// ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
struct DeletedItem {
    path: PathBuf,
    is_dir: bool,
    deleted_at: u64,
}

/// Generate next available log filename (spacefree_0001.log, spacefree_0002.log, etc.)
fn generate_log_filename() -> PathBuf {
    let mut counter = 1;
    loop {
        let filename = format!("spacefree_{:04}.log", counter);
        if !std::path::Path::new(&filename).exists() {
            return PathBuf::from(filename);
        }
        counter += 1;
    }
}

/// Save deleted items to log file
fn save_delete_log(items: &[DeletedItem], path: &PathBuf) -> Result<(), DeleterError> {
    let content = serde_json::to_string_pretty(items)
        .map_err(|e| DeleterError::Io(io::Error::new(io::ErrorKind::Other, e)))?;
    
    std::fs::write(path, content)?;
    info!("Delete log saved to: {}", path.display());
    Ok(())
}

//
// ──────────────────────────────────────────────────────────
// CLI
// ──────────────────────────────────────────────────────────
//

#[derive(Parser, Debug)]
#[command(
    name = "spf",
    about = "🚀 Ultra-fast file deletion CLI tool (supports trash)",
    version
)]
struct Cli {
    /// Paths to scan - can be directories or files to delete (space separated)
    #[arg(required = true, value_name = "PATHS")]
    paths: Vec<PathBuf>,

    /// Path list file containing paths to scan (comma/space/newline separated)
    #[arg(long, value_name = "FILE")]
    path_list_file: Vec<PathBuf>,

    /// Glob pattern for files to delete [default: **/* (all files)]
    #[arg(short, long, value_name = "PATTERN")]
    glob: Option<String>,

    /// Glob pattern to exclude
    #[arg(long, value_name = "PATTERN")]
    exclude: Option<String>,

    /// Minimum file size (e.g., 100, 10k, 5M, 2G, 1T)
    #[arg(long, value_name = "SIZE", default_value = "0", value_parser = parse_size)]
    min_size: u64,

    /// Maximum file size (e.g., 100, 10k, 5M, 2G, 1T)
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    max_size: Option<u64>,

    /// Minimum file age (e.g., 1d, 2w, 3m, 1y) - only files older than this will be deleted
    #[arg(long, value_name = "AGE", value_parser = parse_age)]
    min_age: Option<u64>,

    /// Maximum file age (e.g., 1d, 2w, 3m, 1y) - only files newer than this will be deleted
    #[arg(long, value_name = "AGE", value_parser = parse_age)]
    max_age: Option<u64>,

    /// Move to system trash instead of permanent delete
    #[arg(long)]
    trash: bool,

    /// Preview what would be deleted without actually deleting
    #[arg(long)]
    dry_run: bool,

    /// Skip confirmation prompt
    #[arg(short, long)]
    yes: bool,

    /// Number of parallel workers
    #[arg(short, long, default_value_t = num_cpus::get() * 4, value_name = "N")]
    parallelism: usize,

    /// Show all files to be deleted (verbose mode)
    #[arg(short, long)]
    verbose: bool,

    /// Delete directories as well as files
    #[arg(long)]
    dirs: bool,

    /// Log deleted items to file (default: ./spacefree_0001.log)
    #[arg(short, long, value_name = "PATH")]
    log: Option<Option<PathBuf>>,
}

//
// ──────────────────────────────────────────────────────────
// Utils
// ──────────────────────────────────────────────────────────
//

fn format_size(size: u64) -> String {
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

/// Parse size string with optional unit suffix.
fn parse_size(s: &str) -> Result<u64, String> {
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

/// Parse age string (e.g., 1d, 2w, 3m, 1y) and return seconds.
fn parse_age(s: &str) -> Result<u64, String> {
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
        "M" | "month" | "months" => num * 2592000, // 30 days
        "y" | "year" | "years" => num * 31536000, // 365 days
        _ => return Err(format!("invalid age unit: {}", unit_part)),
    };

    Ok(seconds)
}

fn build_globset(
    include: Option<&str>,
    exclude: &Option<String>,
) -> Result<(GlobSet, Option<globset::GlobMatcher>), DeleterError> {
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

fn format_dirs(paths: &[PathBuf]) -> String {
    let dirs: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    match dirs.len() {
        0 => "directories".into(),
        1 => dirs[0].clone(),
        2 => format!("{} and {}", dirs[0], dirs[1]),
        _ => {
            let last = dirs.last().expect("dirs should have at least 3 elements when len >= 3");
            let rest = &dirs[..dirs.len() - 1];
            format!("{}, and {}", rest.join(", "), last)
        }
    }
}

//
// ──────────────────────────────────────────────────────────
// Config structs
// ──────────────────────────────────────────────────────────

#[derive(Clone)]
struct DeleteConfig {
    use_trash: bool,
    dry_run: bool,
    parallelism: usize,
    min_size: u64,
    max_size: Option<u64>,
    min_age: Option<u64>,
    max_age: Option<u64>,
    verbose: bool,
    dirs: bool,
}

// ──────────────────────────────────────────────────────────
// Scan phase
// ──────────────────────────────────────────────────────────
//

async fn scan_only(
    job_paths: Vec<PathBuf>,
    globset: GlobSet,
    exclude_glob: Option<globset::GlobMatcher>,
    config: &DeleteConfig,
) -> Result<(u64, u64, Vec<PathBuf>, Vec<PathBuf>), DeleterError> {
    // Separate directories and files
    let mut directories = Vec::new();
    let mut individual_files = Vec::new();
    
    for path in job_paths {
        match fs::metadata(&path).await {
            Ok(m) if m.is_dir() => directories.push(path),
            Ok(m) if m.is_file() => individual_files.push(path),
            Ok(_) => {
                warn!("Path is neither file nor directory, skipping: {}", path.display());
            }
            Err(e) => {
                warn!("Cannot access path ({}), skipping: {}", e, path.display());
            }
        }
    }

    let mut total_files = 0;
    let mut total_bytes = 0;
    let mut all_files = Vec::new();
    let mut all_dirs = Vec::new();

    // Process individual files directly
    if !individual_files.is_empty() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System time went backwards")
            .as_secs();

        for path in individual_files {
            // Check exclude first
            if let Some(ref exclude) = exclude_glob {
                if exclude.is_match(&path) {
                    continue;
                }
            }

            // Individual files are always included (glob doesn't apply to direct file paths)
            // unless excluded

            match fs::metadata(&path).await {
                Ok(m) => {
                    let len = m.len();
                    // Size filters
                    if len < config.min_size {
                        continue;
                    }
                    if let Some(max) = config.max_size {
                        if len > max {
                            continue;
                        }
                    }
                    // Age filters
                    if let Ok(modified) = m.modified() {
                        let modified_secs = modified
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(now);
                        let age = now.saturating_sub(modified_secs);
                        if let Some(min) = config.min_age {
                            if age < min {
                                continue;
                            }
                        }
                        if let Some(max) = config.max_age {
                            if age > max {
                                continue;
                            }
                        }
                    }
                    total_files += 1;
                    total_bytes += len;
                    all_files.push(path);
                }
                Err(e) => {
                    warn!("Failed to read metadata for {}: {}", path.display(), e);
                }
            }
        }
    }

    // Process directories by walking them
    if !directories.is_empty() {
        let results = stream::iter(directories)
            .map(|root| {
                let globset = globset.clone();
                let exclude_glob = exclude_glob.clone();
                let min_size = config.min_size;
                let max_size = config.max_size;
                let min_age = config.min_age;
                let max_age = config.max_age;
                let scan_dirs = config.dirs;

                tokio::task::spawn_blocking(move || {
                    let mut files = 0;
                    let mut bytes = 0;
                    let mut file_list = Vec::new();
                    let mut dir_list = Vec::new();
                    let mut metadata_errors = 0;

                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("System time went backwards")
                        .as_secs();

                    for entry in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
                        let path = entry.path();
                        
                        // Check exclude first (cheap)
                        if let Some(ref exclude) = exclude_glob {
                            if exclude.is_match(path) {
                                continue;
                            }
                        }

                        if entry.file_type().is_file() {
                            if !globset.is_match(path) {
                                continue;
                            }
                            
                            match entry.metadata() {
                                Ok(m) => {
                                    let len = m.len();
                                    // Size filters
                                    if len < min_size {
                                        continue;
                                    }
                                    if let Some(max) = max_size {
                                        if len > max {
                                            continue;
                                        }
                                    }
                                    // Age filters
                                    if let Ok(modified) = m.modified() {
                                        let modified_secs = modified
                                            .duration_since(UNIX_EPOCH)
                                            .map(|d| d.as_secs())
                                            .unwrap_or(now);
                                        let age = now.saturating_sub(modified_secs);
                                        if let Some(min) = min_age {
                                            if age < min {
                                                continue;
                                            }
                                        }
                                        if let Some(max) = max_age {
                                            if age > max {
                                                continue;
                                            }
                                        }
                                    }
                                    files += 1;
                                    bytes += len;
                                    file_list.push(path.to_path_buf());
                                }
                                Err(e) => {
                                    metadata_errors += 1;
                                    warn!("Failed to read metadata for {}: {}", path.display(), e);
                                }
                            }
                        } else if scan_dirs && entry.file_type().is_dir() {
                            // Skip root directory
                            if path == root {
                                continue;
                            }
                            // Check if directory matches glob
                            if !globset.is_match(path) {
                                continue;
                            }
                            dir_list.push(path.to_path_buf());
                        }
                    }

                    if metadata_errors > 0 {
                        warn!("{} files had metadata errors in {}", metadata_errors, root.display());
                    }

                    (files, bytes, file_list, dir_list)
                })
            })
            .buffer_unordered(config.parallelism)
            .collect::<Vec<_>>()
            .await;

        for r in results {
            let (f, b, files, dirs) = r.map_err(|_| DeleterError::Join)?;
            total_files += f;
            total_bytes += b;
            all_files.extend(files);
            all_dirs.extend(dirs);
        }
    }

    Ok((total_files, total_bytes, all_files, all_dirs))
}

//
// ──────────────────────────────────────────────────────────
// Delete / Trash phase (true streaming)
// ──────────────────────────────────────────────────────────
//

async fn delete_streaming(
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
    config: DeleteConfig,
    pb: ProgressBar,
    deleted_items: &mut Vec<DeletedItem>,
) -> Result<(u64, u64, Vec<PathBuf>), DeleterError> {
    let deleted = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    
    // Use channel for collecting failed paths to avoid mutex contention
    let (fail_tx, mut fail_rx) = tokio::sync::mpsc::channel::<PathBuf>(100);
    let fail_tx = Arc::new(tokio::sync::Mutex::new(fail_tx));
    
    // Use Arc<Mutex<Vec<DeletedItem>>> for thread-safe access
    let deleted_items_arc = Arc::new(tokio::sync::Mutex::new(deleted_items));

    // Semaphore to limit blocking tasks and prevent thread explosion
    let blocking_semaphore = Arc::new(tokio::sync::Semaphore::new(config.parallelism));
    
    // First, delete all files using batch processing for non-trash deletions
    let file_stream = stream::iter(files);
    file_stream
        .for_each_concurrent(config.parallelism, |path| {
            let deleted = deleted.clone();
            let failed = failed.clone();
            let fail_tx = fail_tx.clone();
            let deleted_items_arc = deleted_items_arc.clone();
            let pb = pb.clone();
            let config = config.clone();
            let blocking_semaphore = blocking_semaphore.clone();

            async move {
                if config.verbose {
                    pb.println(path.display().to_string());
                }

                if !config.dry_run {
                    let path_clone = path.clone();
                    
                    let success = if config.use_trash {
                        // Acquire semaphore permit before spawning blocking task
                        let _permit = blocking_semaphore.acquire().await.expect("Semaphore closed");
                        match tokio::task::spawn_blocking(move || trash_delete(&path_clone)).await {
                            Ok(Ok(())) => {
                                info!("Moved to trash: {}", path.display());
                                true
                            }
                            Ok(Err(e)) => {
                                error!("Failed to move to trash {}: {}", path.display(), e);
                                false
                            }
                            Err(e) => {
                                error!("Task join error for {}: {}", path.display(), e);
                                false
                            }
                        }
                    } else {
                        // Direct async file deletion without blocking
                        match fs::remove_file(&path).await {
                            Ok(()) => {
                                info!("Deleted: {}", path.display());
                                true
                            }
                            Err(e) => {
                                error!("Failed to delete {}: {}", path.display(), e);
                                false
                            }
                        }
                    };

                    if success {
                        deleted.fetch_add(1, Ordering::Relaxed);
                        // Add to deleted items log
                        deleted_items_arc.lock().await.push(DeletedItem {
                            path,
                            is_dir: false,
                            deleted_at: SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .expect("System time went backwards")
                                .as_secs(),
                        });
                    } else {
                        failed.fetch_add(1, Ordering::Relaxed);
                        let _ = fail_tx.lock().await.send(path.clone()).await;
                        if config.verbose {
                            pb.suspend(|| {
                                eprintln!("❌ Failed to delete: {}", path.display());
                            });
                        }
                    }
                }

                pb.inc(1);
            }
        })
        .await;

    // Then, delete empty directories (sorted by depth, deepest first) in parallel
    if !dirs.is_empty() {
        let mut sorted_dirs = dirs;
        sorted_dirs.sort_by(|a, b| {
            let depth_a = a.components().count();
            let depth_b = b.components().count();
            depth_b.cmp(&depth_a)
        });

        // Process directories in parallel
        stream::iter(sorted_dirs)
            .for_each_concurrent(config.parallelism, |dir| {
                let deleted = deleted.clone();
                let failed = failed.clone();
                let fail_tx = fail_tx.clone();
                let deleted_items_arc = deleted_items_arc.clone();
                let pb = pb.clone();
                let config = config.clone();
                let blocking_semaphore = blocking_semaphore.clone();

                async move {
                    if config.verbose {
                        pb.println(dir.display().to_string());
                    }

                    if !config.dry_run {
                        let dir_for_check = dir.clone();

                        // Check if directory is empty
                        let is_empty = match tokio::task::spawn_blocking(move || {
                            match std::fs::read_dir(&dir_for_check) {
                                Ok(mut entries) => entries.next().is_none(),
                                Err(e) => {
                                    warn!("Failed to read directory {}: {}", dir_for_check.display(), e);
                                    false
                                }
                            }
                        }).await {
                            Ok(empty) => empty,
                            Err(e) => {
                                error!("Task join error for directory check {}: {}", dir.display(), e);
                                false
                            }
                        };

                        if is_empty {
                            let dir_clone = dir.clone();
                            let success = if config.use_trash {
                                // Acquire semaphore permit before spawning blocking task
                                let _permit = blocking_semaphore.acquire().await.expect("Semaphore closed");
                                match tokio::task::spawn_blocking(move || trash_delete(&dir_clone)).await {
                                    Ok(Ok(())) => {
                                        info!("Moved directory to trash: {}", dir.display());
                                        true
                                    }
                                    Ok(Err(e)) => {
                                        error!("Failed to move dir to trash {}: {}", dir.display(), e);
                                        false
                                    }
                                    Err(e) => {
                                        error!("Task join error for dir {}: {}", dir.display(), e);
                                        false
                                    }
                                }
                            } else {
                                match fs::remove_dir(&dir).await {
                                    Ok(()) => {
                                        info!("Deleted directory: {}", dir.display());
                                        true
                                    }
                                    Err(e) => {
                                        error!("Failed to delete directory {}: {}", dir.display(), e);
                                        false
                                    }
                                }
                            };

                            if success {
                                deleted.fetch_add(1, Ordering::Relaxed);
                                deleted_items_arc.lock().await.push(DeletedItem {
                                    path: dir,
                                    is_dir: true,
                                    deleted_at: SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .expect("System time went backwards")
                                        .as_secs(),
                                });
                            } else {
                                failed.fetch_add(1, Ordering::Relaxed);
                                let _ = fail_tx.lock().await.send(dir.clone()).await;
                                if config.verbose {
                                    pb.suspend(|| {
                                        eprintln!("❌ Failed to delete directory: {}", dir.display());
                                    });
                                }
                            }
                        } else {
                            if config.verbose {
                                pb.suspend(|| {
                                    eprintln!("⏭️  Skipping non-empty directory: {}", dir.display());
                                });
                            }
                        }
                    }

                    pb.inc(1);
                }
            })
            .await;
    }
    
    // Close channel and collect failed paths
    drop(fail_tx);
    let mut failed_paths = Vec::new();
    while let Some(path) = fail_rx.recv().await {
        failed_paths.push(path);
    }

    pb.finish();

    let failed_count = failed.load(Ordering::Relaxed);
    Ok((deleted.load(Ordering::Relaxed), failed_count, failed_paths))
}

//
// ──────────────────────────────────────────────────────────
// Collect paths from CLI (support file containing paths)
// ──────────────────────────────────────────────────────────
//

fn parse_paths_from_content(content: &str) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();

    for line in content.lines() {
        for part in line.split([',', ' ', '\t']) {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                continue;
            }
            let path = PathBuf::from(trimmed);
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
    }

    paths
}

async fn collect_paths(input_paths: &[PathBuf], path_list_files: &[PathBuf]) -> Result<Vec<PathBuf>, DeleterError> {
    let mut all_paths = Vec::new();
    let mut seen = HashSet::new();

    // Handle path list files (files containing paths to scan)
    for path_file in path_list_files {
        let content = fs::read_to_string(path_file).await?;
        for p in parse_paths_from_content(&content) {
            if seen.insert(p.clone()) {
                all_paths.push(p);
            }
        }
    }

    // Handle direct paths (directories or files to delete)
    for path in input_paths {
        let metadata = fs::metadata(path).await?;
        if metadata.is_dir() {
            // Directory - add directly
            if seen.insert(path.clone()) {
                all_paths.push(path.clone());
            }
        } else if metadata.is_file() {
            // Regular file - add as a file to delete, not a path list
            if seen.insert(path.clone()) {
                all_paths.push(path.clone());
            }
        } else {
            return Err(DeleterError::JobDir(path.clone()));
        }
    }

    if all_paths.is_empty() {
        return Err(DeleterError::NoValidPaths);
    }

    Ok(all_paths)
}

//
// ──────────────────────────────────────────────────────────
// Main run
// ──────────────────────────────────────────────────────────
//

async fn run(cli: Cli) -> Result<(), DeleterError> {
    let all_paths = collect_paths(&cli.paths, &cli.path_list_file).await?;
    let (globset, exclude_glob) = build_globset(cli.glob.as_deref(), &cli.exclude)?;

    println!("🔍 Scanning...");

    let config = DeleteConfig {
        use_trash: cli.trash,
        dry_run: cli.dry_run,
        parallelism: cli.parallelism,
        min_size: cli.min_size,
        max_size: cli.max_size,
        min_age: cli.min_age,
        max_age: cli.max_age,
        verbose: cli.verbose,
        dirs: cli.dirs,
    };

    let (files_count, bytes, file_list, dir_list) = scan_only(
        all_paths.clone(),
        globset.clone(),
        exclude_glob.clone(),
        &config,
    )
    .await?;

    let total_items = files_count + dir_list.len() as u64;

    if total_items == 0 {
        println!("Nothing matched.");
        return Ok(());
    }

    let glob_pattern = cli.glob.as_deref().unwrap_or("**/*");
    let mode = if cli.trash { "TRASH" } else { "PERMANENT" };
    let item_type = if cli.dirs {
        "files/empty dirs"
    } else {
        "files"
    };
    println!(
        "ALL '{}' {} in {} will be {} deleted!",
        glob_pattern,
        item_type,
        format_dirs(&all_paths),
        mode
    );

    println!("Files: {}, Size: {}", files_count, format_size(bytes));
    if cli.dirs && !dir_list.is_empty() {
        println!("Empty directories to check: {}", dir_list.len());
    }

    if cli.verbose {
        for p in &file_list {
            println!("  {}", p.display());
        }
        for p in &dir_list {
            println!("  {} (dir)", p.display());
        }
    }

    if !cli.dry_run && !cli.yes {
        print!("\nType YES/Yes/yes/Y/y to continue: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let trimmed = input.trim().to_lowercase();
        if trimmed != "yes" && trimmed != "y" {
            return Err(DeleterError::Cancelled);
        }
    }

    let mp = MultiProgress::new();
    let pb = mp.add(ProgressBar::new(total_items));

    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.red} [{elapsed_precise}] [{bar:40}] {pos}/{len}")
            .map_err(|e| DeleterError::ProgressBar(e.to_string()))?,
    );

    println!("🗑️  Processing...");

    let mut deleted_items: Vec<DeletedItem> = Vec::new();
    let (deleted, failed, failed_paths) = delete_streaming(file_list, dir_list, config, pb, &mut deleted_items).await?;
    
    // Save log if --log is provided, not dry_run, and there are actual deleted items
    if !cli.dry_run && cli.log.is_some() && !deleted_items.is_empty() {
        let log_path = if let Some(Some(p)) = cli.log.as_ref() {
            p.clone()
        } else {
            generate_log_filename()
        };
        if let Err(e) = save_delete_log(&deleted_items, &log_path) {
            warn!("Failed to save delete log: {}", e);
        }
    }

    if cli.dry_run {
        println!("Preview complete.");
    } else {
        if failed > 0 {
            eprintln!();
            eprintln!("⚠️  {} item(s) failed to delete:", failed);
            for path in &failed_paths {
                eprintln!("  - {}", path.display());
            }
            eprintln!("  (Check file permissions)");
        }
        println!("✅ Removed {} item(s), freed {}", deleted, format_size(bytes));
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), DeleterError> {
    // Initialize tracing subscriber
    tracing_subscriber::fmt::init();
    
    let cli = Cli::parse();
    
    debug!("Starting spacefree with CLI args: {:?}", cli);
    
    if let Err(e) = run(cli).await {
        error!("Application error: {}", e);
        return Err(e);
    }
    
    info!("spacefree completed successfully");
    Ok(())
}
#[cfg(test)]
pub mod test;
