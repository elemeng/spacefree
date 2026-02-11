use clap::Parser;
use futures::{StreamExt, stream};
use globset::{Glob, GlobSet, GlobSetBuilder};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::{
    collections::HashSet,
    io::{self, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use thiserror::Error;
use tokio::fs;
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
    /// Job directories to scan (space separated, e.g., J12 J13).
    /// Can also be CSV/TXT files containing paths (comma/space/newline separated).
    #[arg(required = true, value_name = "PATHS")]
    paths: Vec<PathBuf>,

    /// Glob pattern for files to delete [default: **/* (all files)]
    #[arg(short, long, value_name = "PATTERN")]
    glob: Option<String>,

    /// Glob pattern to exclude
    #[arg(long, value_name = "PATTERN")]
    exclude: Option<String>,

    /// Minimum file size (e.g., 100, 10k, 5M, 2G, 1T)
    #[arg(long, value_name = "SIZE", default_value = "0", value_parser = parse_size)]
    min_size: u64,

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
            let last = dirs.last().unwrap();
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
    verbose: bool,
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
) -> Result<(u64, u64, Vec<PathBuf>), DeleterError> {
    let results = stream::iter(job_paths)
        .map(|root| {
            let globset = globset.clone();
            let exclude_glob = exclude_glob.clone();
            let min_size = config.min_size;

            tokio::task::spawn_blocking(move || {
                let mut files = 0;
                let mut bytes = 0;
                let mut file_list = Vec::new();

                for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
                    if !entry.file_type().is_file() {
                        continue;
                    }
                    if !globset.is_match(entry.path()) {
                        continue;
                    }
                    if let Some(ref exclude) = exclude_glob {
                        if exclude.is_match(entry.path()) {
                            continue;
                        }
                    }
                    let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    if len < min_size {
                        continue;
                    }

                    files += 1;
                    bytes += len;
                    file_list.push(entry.path().to_path_buf());
                }

                (files, bytes, file_list)
            })
        })
        .buffer_unordered(config.parallelism)
        .collect::<Vec<_>>()
        .await;

    let mut total_files = 0;
    let mut total_bytes = 0;
    let mut all_files = Vec::new();

    for r in results {
        let (f, b, files) = r.map_err(|_| DeleterError::Join)?;
        total_files += f;
        total_bytes += b;
        all_files.extend(files);
    }

    Ok((total_files, total_bytes, all_files))
}

//
// ──────────────────────────────────────────────────────────
// Delete / Trash phase (true streaming)
// ──────────────────────────────────────────────────────────
//

async fn delete_streaming(
    roots: Vec<PathBuf>,
    globset: GlobSet,
    exclude_glob: Option<globset::GlobMatcher>,
    config: DeleteConfig,
    pb: ProgressBar,
) -> Result<(u64, u64, Vec<PathBuf>), DeleterError> {
    let deleted = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let failed_paths = Arc::new(std::sync::Mutex::new(Vec::new()));

    let stream = stream::iter(roots).flat_map(|root| {
        let globset = globset.clone();
        let exclude_glob = exclude_glob.clone();
        let min_size = config.min_size;
        stream::iter(
            WalkDir::new(root)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(move |e| {
                    if !e.file_type().is_file() {
                        return false;
                    }
                    if !globset.is_match(e.path()) {
                        return false;
                    }
                    if let Some(ref exclude) = exclude_glob {
                        if exclude.is_match(e.path()) {
                            return false;
                        }
                    }
                    e.metadata().map(|m| m.len()).unwrap_or(0) >= min_size
                })
                .map(|e| e.into_path()),
        )
    });

    stream
        .for_each_concurrent(config.parallelism, |path| {
            let deleted = deleted.clone();
            let failed = failed.clone();
            let failed_paths = failed_paths.clone();
            let pb = pb.clone();
            let config = config.clone();

            async move {
                if config.verbose {
                    pb.println(path.display().to_string());
                }

                if !config.dry_run {
                    let path_clone = path.clone();
                    let success = if config.use_trash {
                        tokio::task::spawn_blocking(move || trash_delete(&path_clone))
                            .await
                            .map(|r| r.is_ok())
                            .unwrap_or(false)
                    } else {
                        fs::remove_file(&path).await.is_ok()
                    };

                    if success {
                        deleted.fetch_add(1, Ordering::Relaxed);
                    } else {
                        failed.fetch_add(1, Ordering::Relaxed);
                        let mut paths = failed_paths.lock().unwrap();
                        paths.push(path.clone());
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

    pb.finish();

    let failed_count = failed.load(Ordering::Relaxed);
    if failed_count > 0 {
        let paths = failed_paths.lock().unwrap().clone();
        Ok((deleted.load(Ordering::Relaxed), failed_count, paths))
    } else {
        Ok((deleted.load(Ordering::Relaxed), 0, Vec::new()))
    }
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

async fn collect_paths(input_paths: &[PathBuf]) -> Result<Vec<PathBuf>, DeleterError> {
    let mut all_paths = Vec::new();
    let mut seen = HashSet::new();

    for path in input_paths {
        let metadata = fs::metadata(path).await?;
        if metadata.is_file() {
            let content = fs::read_to_string(path).await?;
            for p in parse_paths_from_content(&content) {
                if seen.insert(p.clone()) {
                    all_paths.push(p);
                }
            }
        } else if metadata.is_dir() {
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

    for path in &all_paths {
        if !fs::metadata(path).await?.is_dir() {
            return Err(DeleterError::JobDir(path.clone()));
        }
    }

    Ok(all_paths)
}

//
// ──────────────────────────────────────────────────────────
// Main run
// ──────────────────────────────────────────────────────────
//

async fn run(cli: Cli) -> Result<(), DeleterError> {
    let all_paths = collect_paths(&cli.paths).await?;
    let (globset, exclude_glob) = build_globset(cli.glob.as_deref(), &cli.exclude)?;

    println!("🔍 Scanning...");

    let config = DeleteConfig {
        use_trash: cli.trash,
        dry_run: cli.dry_run,
        parallelism: cli.parallelism,
        min_size: cli.min_size,
        verbose: cli.verbose,
    };

    let (files, bytes, preview) = scan_only(
        all_paths.clone(),
        globset.clone(),
        exclude_glob.clone(),
        &config,
    )
    .await?;

    if files == 0 {
        println!("Nothing matched.");
        return Ok(());
    }

    let glob_pattern = cli.glob.as_deref().unwrap_or("**/*");
    let mode = if cli.trash { "TRASH" } else { "PERMANENT" };
    println!(
        "ALL '{}' files in dir {} will be {} deleted!",
        glob_pattern,
        format_dirs(&all_paths),
        mode
    );

    println!("Files: {}, Size: {}", files, format_size(bytes));

    if cli.verbose {
        for p in &preview {
            println!("  {}", p.display());
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
    let pb = mp.add(ProgressBar::new(files));

    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.red} [{elapsed_precise}] [{bar:40}] {pos}/{len}")
            .map_err(|e| DeleterError::ProgressBar(e.to_string()))?,
    );

    println!("🗑️  Processing...");

    let (deleted, failed, failed_paths) = delete_streaming(all_paths, globset, exclude_glob, config, pb).await?;

    if cli.dry_run {
        println!("Preview complete.");
    } else {
        if failed > 0 {
            eprintln!();
            eprintln!("⚠️  {} file(s) failed to delete:", failed);
            for path in &failed_paths {
                eprintln!("  - {}", path.display());
            }
            eprintln!("  (Check file permissions)");
        }
        println!("✅ Removed {} files, freed {}", deleted, format_size(bytes));
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), DeleterError> {
    let cli = Cli::parse();
    run(cli).await
}
#[cfg(test)]
pub mod test;
