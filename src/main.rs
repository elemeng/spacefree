use clap::Parser;
use globset::{Glob, GlobMatcher, GlobSet, GlobSetBuilder};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{fs, io::AsyncWriteExt, sync::mpsc, task::spawn_blocking};
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
    Io(#[from] std::io::Error),

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
//

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

    /// Allow deleting root directory (requires -y as well)
    #[arg(long)]
    delete_root_dir: bool,

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
        "M" | "month" | "months" => num * 2592000,
        "y" | "year" | "years" => num * 31536000,
        _ => return Err(format!("invalid age unit: {}", unit_part)),
    };

    Ok(seconds)
}

fn build_globset(
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

fn format_dirs(paths: &[PathBuf]) -> String {
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

//
// ──────────────────────────────────────────────────────────
// Config structs
// ──────────────────────────────────────────────────────────
//

#[derive(Clone)]
#[allow(dead_code)]
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
    glob_pattern: String,
    glob_matcher: GlobSet,
    exclude_matcher: Option<GlobMatcher>,
}

//
// ──────────────────────────────────────────────────────────
// Scan result
// ──────────────────────────────────────────────────────────
//

#[derive(Clone)]
struct ScanResult {
    path: PathBuf,
    is_dir: bool,
    size: u64,
}

//
// ──────────────────────────────────────────────────────────
// Collect paths
// ──────────────────────────────────────────────────────────
//

fn parse_paths_from_content(content: &str) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
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

async fn collect_paths(
    input_paths: &[PathBuf],
    path_list_files: &[PathBuf],
) -> Result<Vec<PathBuf>, DeleterError> {
    let mut all_paths = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for path_file in path_list_files {
        let content = fs::read_to_string(path_file).await?;
        for p in parse_paths_from_content(&content) {
            if seen.insert(p.clone()) {
                all_paths.push(p);
            }
        }
    }

    for path in input_paths {
        let _metadata = fs::metadata(path).await?;
        // Add both directories and files to all_paths
        if seen.insert(path.clone()) {
            all_paths.push(path.clone());
        }
    }

    if all_paths.is_empty() {
        return Err(DeleterError::NoValidPaths);
    }

    Ok(all_paths)
}

//
// ──────────────────────────────────────────────────────────
// Scan phase (streaming to channel)
// ──────────────────────────────────────────────────────────
//

async fn scan_to_channel(
    root: PathBuf,
    file_tx: mpsc::Sender<ScanResult>,
    config: DeleteConfig,
) -> Result<(), DeleterError> {
    tokio::task::spawn_blocking(move || {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System time went backwards")
            .as_secs();

        let mut scan_dirs = Vec::new();

        for entry in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();

            // Skip the root directory itself - we'll add it after WalkDir completes
            if path == root {
                continue;
            }

            if entry.file_type().is_file() {
                let metadata = match entry.metadata() {
                    Ok(m) => m,
                    Err(e) => {
                        warn!("Failed to read metadata for {}: {}", path.display(), e);
                        continue;
                    }
                };

                let len = metadata.len();
                if len < config.min_size {
                    continue;
                }
                if let Some(max) = config.max_size {
                    if len > max {
                        continue;
                    }
                }

                if let Ok(modified) = metadata.modified() {
                    let modified_secs = modified
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(now);
                    let age = now.saturating_sub(modified_secs);
                    if let Some(min_age_val) = config.min_age {
                        if age < min_age_val {
                            continue;
                        }
                    }
                    if let Some(max_age_val) = config.max_age {
                        if age > max_age_val {
                            continue;
                        }
                    }
                }

                if config.dirs {
                    // When --dirs is enabled, skip individual files - directories will handle them
                    continue;
                }

                if !config.glob_matcher.is_match(path) {
                    continue;
                }

                if let Some(ref exclude) = config.exclude_matcher {
                    if exclude.is_match(path) {
                        continue;
                    }
                }

                if file_tx
                    .blocking_send(ScanResult {
                        path: path.to_path_buf(),
                        is_dir: false,
                        size: len,
                    })
                    .is_err()
                {
                    break;
                }
            } else if config.dirs && entry.file_type().is_dir() {
                // Include ALL directories when --dirs is enabled
                // Don't filter by glob - only files need glob matching
                scan_dirs.push(ScanResult {
                    path: path.to_path_buf(),
                    is_dir: true,
                    size: 0,
                });
            }
        }

        // After WalkDir completes, add the root directory if --dirs is enabled
        // This ensures WalkDir has fully released the directory before we try to delete it
        if config.dirs {
            scan_dirs.push(ScanResult {
                path: root.to_path_buf(),
                is_dir: true,
                size: 0,
            });
        }

        // Sort directories by depth (deepest first) to ensure proper deletion order
        scan_dirs.sort_by(|a, b| {
            let depth_a = a.path.components().count();
            let depth_b = b.path.components().count();
            depth_b.cmp(&depth_a) // Reverse order - deepest first
        });

        for dir in scan_dirs {
            if file_tx.blocking_send(dir).is_err() {
                break;
            }
        }
    })
    .await
    .map_err(|_| DeleterError::Join)?;

    Ok(())
}

async fn scan_files_direct(
    paths: Vec<PathBuf>,
    file_tx: mpsc::Sender<ScanResult>,
    config: DeleteConfig,
) -> Result<(), DeleterError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("System time went backwards")
        .as_secs();

    for path in paths {
        let metadata = match fs::metadata(&path).await {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to read metadata for {}: {}", path.display(), e);
                continue;
            }
        };

        let len = metadata.len();
        if len < config.min_size {
            continue;
        }
        if let Some(max) = config.max_size {
            if len > max {
                continue;
            }
        }

        if let Ok(modified) = metadata.modified() {
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

        if file_tx
            .send(ScanResult {
                path,
                is_dir: false,
                size: len,
            })
            .await
            .is_err()
        {
            break;
        }
    }

    Ok(())
}

//
// ──────────────────────────────────────────────────────────
// Delete phase (true streaming)
// ──────────────────────────────────────────────────────────
//

async fn run_deletion_pipeline(
    directories: Vec<PathBuf>,
    individual_files: Vec<PathBuf>,
    config: DeleteConfig,
    pb: ProgressBar,
    log_path: Option<PathBuf>,
) -> Result<(u64, u64, u64, Vec<PathBuf>), DeleterError> {
    // Channels for streaming pipeline
    let (scan_tx, mut scan_rx) = mpsc::channel::<ScanResult>(1000);
    let (deleted_tx, mut deleted_rx) = mpsc::channel::<DeletedItem>(1000);
    let (trash_tx, mut trash_rx) = mpsc::channel::<PathBuf>(1000);
    let fail_tx = Arc::new(mpsc::channel::<PathBuf>(100).0);
    let mut fail_rx = mpsc::channel::<PathBuf>(100).1;

    let deleted_count = Arc::new(AtomicU64::new(0));
    let failed_count = Arc::new(AtomicU64::new(0));
    let bytes_freed = Arc::new(AtomicU64::new(0));

    // Dedicated trash worker thread
    let deleted_count_trash = deleted_count.clone();
    let failed_count_trash = failed_count.clone();
    let deleted_tx_trash = deleted_tx.clone();
    let fail_tx_trash = fail_tx.clone();
    spawn_blocking(move || {
        while let Some(path) = trash_rx.blocking_recv() {
            match trash_delete(&path) {
                Ok(_) => {
                    info!("Moved to trash: {}", path.display());
                    deleted_count_trash.fetch_add(1, Ordering::Relaxed);
                    let _ = deleted_tx_trash.blocking_send(DeletedItem {
                        path: path.clone(),
                        is_dir: false,
                        deleted_at: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .expect("System time went backwards")
                            .as_secs(),
                    });
                }
                Err(e) => {
                    error!("Failed to move to trash {}: {}", path.display(), e);
                    failed_count_trash.fetch_add(1, Ordering::Relaxed);
                    let _ = fail_tx_trash.blocking_send(path);
                }
            }
        }
    });

    // Logger task (NDJSON incremental write)
    let log_handle = log_path.map(|path| {
        tokio::spawn(async move {
            if let Ok(mut file) = fs::File::create(&path).await {
                while let Some(item) = deleted_rx.recv().await {
                    if let Ok(json) = serde_json::to_string(&item) {
                        let _ = file.write_all(json.as_bytes()).await;
                        let _ = file.write_all(b"\n").await;
                    }
                }
                info!("Delete log saved to: {}", path.display());
            }
        })
    });

    // Spawn scanner tasks
    let scan_handles: Vec<_> = directories
        .into_iter()
        .map(|root| {
            let scan_tx = scan_tx.clone();
            let config = config.clone();
            tokio::spawn(async move {
                let _ = scan_to_channel(root, scan_tx, config).await;
            })
        })
        .collect();

    if !individual_files.is_empty() {
        let scan_tx = scan_tx.clone();
        let config = config.clone();
        tokio::spawn(async move {
            let _ = scan_files_direct(individual_files, scan_tx, config).await;
        });
    }

    drop(scan_tx);

    // Delete consumer with limited concurrency
    let delete_count = deleted_count.clone();
    let fail_count = failed_count.clone();
    let total_bytes = bytes_freed.clone();
    let fail_tx_for_tasks = fail_tx.clone();
    let pb_clone = pb.clone();
    let delete_handle = tokio::spawn(async move {
        let (task_tx, mut task_rx) =
            mpsc::channel::<tokio::task::JoinHandle<()>>(config.parallelism);

        // Task processor
        let processor = tokio::spawn(async move {
            while let Some(task) = task_rx.recv().await {
                let _ = task.await;
            }
        });

        while let Some(result) = scan_rx.recv().await {
            let deleted_tx = deleted_tx.clone();
            let fail_tx = fail_tx_for_tasks.clone();
            let trash_tx = trash_tx.clone();
            let pb = pb_clone.clone();
            let config = config.clone();
            let delete_count = delete_count.clone();
            let fail_count = fail_count.clone();
            let total_bytes = total_bytes.clone();

            let task = tokio::spawn(async move {
                if config.verbose {
                    pb.println(result.path.display().to_string());
                }

                if !config.dry_run {
                    let success = if result.is_dir {
                        match fs::remove_dir_all(&result.path).await {
                            Ok(_) => true,
                            Err(_e) => {
                                // Check if directory still exists - if not, it was deleted despite the error
                                let mut still_exists = true;
                                for _ in 0..3 {
                                    if fs::metadata(&result.path).await.is_err() {
                                        still_exists = false;
                                        break;
                                    }
                                    tokio::time::sleep(tokio::time::Duration::from_millis(10))
                                        .await;
                                }
                                if !still_exists {
                                    info!(
                                        "Directory deleted despite error: {}",
                                        result.path.display()
                                    );
                                    true
                                } else {
                                    error!("Failed to delete directory: {}", result.path.display());
                                    false
                                }
                            }
                        }
                    } else {
                        if config.use_trash {
                            trash_tx.send(result.path.clone()).await.is_ok()
                        } else {
                            match fs::remove_file(&result.path).await {
                                Ok(_) => {
                                    info!("Deleted: {}", result.path.display());
                                    true
                                }
                                Err(e) => {
                                    error!("Failed to delete {}: {}", result.path.display(), e);
                                    false
                                }
                            }
                        }
                    };

                    if success {
                        delete_count.fetch_add(1, Ordering::Relaxed);
                        if !result.is_dir {
                            total_bytes.fetch_add(result.size, Ordering::Relaxed);
                        }
                        deleted_tx
                            .send(DeletedItem {
                                path: result.path,
                                is_dir: result.is_dir,
                                deleted_at: SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .expect("System time went backwards")
                                    .as_secs(),
                            })
                            .await
                            .ok();
                    } else if !result.is_dir {
                        fail_count.fetch_add(1, Ordering::Relaxed);
                        fail_tx.send(result.path).await.ok();
                    }
                }

                pb.inc(1);
            });

            task_tx.send(task).await.ok();
        }

        drop(task_tx);
        processor.await.ok();
    });

    for handle in scan_handles {
        handle.await.ok();
    }
    delete_handle.await.map_err(|_| DeleterError::Join)?;
    if let Some(log_handle) = log_handle {
        log_handle.await.map_err(|_| DeleterError::Join)?;
    }

    drop(fail_tx);
    let mut failed_paths = Vec::new();
    while let Some(path) = fail_rx.recv().await {
        failed_paths.push(path);
    }

    pb.finish();

    Ok((
        deleted_count.load(Ordering::Relaxed),
        failed_count.load(Ordering::Relaxed),
        bytes_freed.load(Ordering::Relaxed),
        failed_paths,
    ))
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

    let glob_pattern = cli.glob.as_deref().unwrap_or("**/*").to_string();

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
        glob_pattern: glob_pattern.clone(),
        glob_matcher: globset,
        exclude_matcher: exclude_glob,
    };

    // Check for root directory and require explicit confirmation
    for path in &all_paths {
        if path.as_os_str() == "/" || path.as_os_str() == "" {
            if !cli.delete_root_dir {
                eprintln!("❌ ERROR: Attempting to delete root directory (/)");
                eprintln!("This is extremely dangerous and could destroy your entire system.");
                eprintln!();
                eprintln!("To delete root directory, you must use BOTH:");
                eprintln!("  -y (skip confirmation)");
                eprintln!("  --delete-root-dir (explicitly allow root deletion)");
                eprintln!();
                eprintln!("Example: spf / -y --delete-root-dir");
                return Err(DeleterError::Cancelled);
            }
            if !cli.yes {
                eprintln!("❌ ERROR: Deleting root directory (/) requires -y flag");
                eprintln!();
                eprintln!("You must use both:");
                eprintln!("  -y (skip confirmation)");
                eprintln!("  --delete-root-dir (explicitly allow root deletion)");
                return Err(DeleterError::Cancelled);
            }
        }
    }

    // Separate directories and individual files
    let mut directories = Vec::new();
    let mut individual_files = Vec::new();

    for path in &all_paths {
        match fs::metadata(path).await {
            Ok(m) if m.is_dir() => directories.push(path.clone()),
            Ok(m) if m.is_file() => individual_files.push(path.clone()),
            Ok(_) => warn!(
                "Path is neither file nor directory, skipping: {}",
                path.display()
            ),
            Err(e) => warn!("Cannot access path ({}), skipping: {}", e, path.display()),
        }
    }

    // Quick scan for preview (sample first few paths)
    let mut preview_files = 0;
    let mut _preview_bytes = 0;
    let mut preview_dirs = 0;

    for path in &individual_files {
        if let Ok(m) = fs::metadata(path).await {
            preview_files += 1;
            _preview_bytes += m.len();
        }
    }
    for dir in &directories {
        for entry in WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .take(1000)
        {
            if entry.file_type().is_file() {
                if let Ok(m) = entry.metadata() {
                    if m.len() >= cli.min_size {
                        preview_files += 1;
                        _preview_bytes += m.len();
                    }
                }
            } else if cli.dirs && entry.file_type().is_dir() {
                preview_dirs += 1;
            }
        }
    }

    let total_estimate = preview_files + preview_dirs;
    if total_estimate == 0 {
        println!("Nothing matched.");
        return Ok(());
    }

    let mode = if cli.trash { "TRASH" } else { "PERMANENT" };
    let item_type = if cli.dirs {
        "files/empty dirs"
    } else {
        "files"
    };

    println!(
        "Will scan and delete '{}' {} in {} with {} mode",
        glob_pattern,
        item_type,
        format_dirs(&all_paths),
        mode
    );

    println!("Estimated items: {}", total_estimate);

    if !cli.dry_run && !cli.yes {
        print!("\nType exactly YES to continue: ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let trimmed = input.trim();
        if trimmed != "YES" {
            return Err(DeleterError::Cancelled);
        }
    }

    let mp = MultiProgress::new();
    let pb = mp.add(ProgressBar::new(total_estimate as u64));

    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.red} [{elapsed_precise}] [{bar:40}] {pos}/{len}")
            .map_err(|e| DeleterError::ProgressBar(e.to_string()))?,
    );

    println!("🗑️  Processing...");

    let log_path = if !cli.dry_run && cli.log.is_some() {
        if let Some(Some(p)) = cli.log.as_ref() {
            Some(p.clone())
        } else {
            Some(generate_log_filename())
        }
    } else {
        None
    };

    let (deleted, failed, bytes, failed_paths) =
        run_deletion_pipeline(directories, individual_files, config, pb, log_path).await?;

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
        println!(
            "✅ Removed {} item(s), freed {}",
            deleted,
            format_size(bytes)
        );
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), DeleterError> {
    let cli = Cli::parse();

    // Set log level based on verbose flag
    if cli.verbose {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .init();
    }

    debug!("Starting spacefree with CLI args: {:?}", cli);

    if let Err(e) = run(cli).await {
        error!("Application error: {}", e);
        return Err(e);
    }

    info!("spacefree completed successfully");
    Ok(())
}
