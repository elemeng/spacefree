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
    about = "⚠️ Ultra-fast safe file deletion tool (supports trash)",
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

fn build_globset(include: Option<&str>, exclude: &Option<String>) -> Result<GlobSet, DeleterError> {
    let mut builder = GlobSetBuilder::new();

    let pattern = include.unwrap_or("**/*");
    builder.add(Glob::new(pattern).map_err(|e| DeleterError::Glob(e.to_string()))?);

    if let Some(ex) = exclude {
        builder.add(Glob::new(ex).map_err(|e| DeleterError::Glob(e.to_string()))?);
    }

    builder
        .build()
        .map_err(|e| DeleterError::Glob(e.to_string()))
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
// Scan phase
// ──────────────────────────────────────────────────────────
//

async fn scan_only(
    job_paths: Vec<PathBuf>,
    globset: GlobSet,
    min_size: u64,
    parallelism: usize,
) -> Result<(u64, u64, Vec<PathBuf>), DeleterError> {
    let results = stream::iter(job_paths)
        .map(|root| {
            let globset = globset.clone();

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
        .buffer_unordered(parallelism)
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
    dry_run: bool,
    use_trash: bool,
    parallelism: usize,
    min_size: u64,
    verbose: bool,
    pb: ProgressBar,
) -> Result<u64, DeleterError> {
    let deleted = Arc::new(AtomicU64::new(0));

    let stream = stream::iter(roots).flat_map(|root| {
        let globset = globset.clone();
        stream::iter(
            WalkDir::new(root)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(move |e| {
                    e.file_type().is_file()
                        && globset.is_match(e.path())
                        && e.metadata().map(|m| m.len()).unwrap_or(0) >= min_size
                })
                .map(|e| e.into_path()),
        )
    });

    stream
        .for_each_concurrent(parallelism, |path| {
            let deleted = deleted.clone();
            let pb = pb.clone();

            async move {
                if verbose {
                    pb.println(path.display().to_string());
                }

                if !dry_run {
                    if use_trash {
                        let _ = tokio::task::spawn_blocking(move || trash_delete(&path)).await;
                    } else {
                        let _ = fs::remove_file(&path).await;
                    }
                    deleted.fetch_add(1, Ordering::Relaxed);
                }

                pb.inc(1);
            }
        })
        .await;

    pb.finish();
    Ok(deleted.load(Ordering::Relaxed))
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
    let globset = build_globset(cli.glob.as_deref(), &cli.exclude)?;

    println!("🔍 Scanning...");

    let (files, bytes, preview) = scan_only(
        all_paths.clone(),
        globset.clone(),
        cli.min_size,
        cli.parallelism,
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
        print!("\nType YES to continue: ");
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

    let deleted = delete_streaming(
        all_paths,
        globset,
        cli.dry_run,
        cli.trash,
        cli.parallelism,
        cli.min_size,
        cli.verbose,
        pb,
    )
    .await?;

    if cli.dry_run {
        println!("Preview complete.");
    } else {
        println!("✅ Removed {} files, freed {}", deleted, format_size(bytes));
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), DeleterError> {
    let cli = Cli::parse();
    run(cli).await
}
