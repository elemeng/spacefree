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
}

//
// ──────────────────────────────────────────────────────────
// CLI
// ──────────────────────────────────────────────────────────
//

#[derive(Parser, Debug)]
#[command(
    name = "deleter",
    about = "⚠️ Ultra-fast safe file deletion tool (supports trash)",
    version
)]
struct Cli {
    /// Job directories to scan (space separated, e.g., J12 J13).
    /// Can also be CSV/TXT files containing paths (comma/space/newline separated).
    #[arg(required = true, value_name = "PATHS")]
    paths: Vec<PathBuf>,

    /// Glob pattern for files to delete
    #[arg(short, long, default_value = "**/*.mrc", value_name = "PATTERN")]
    glob: String,

    /// Glob pattern to exclude
    #[arg(long, value_name = "PATTERN")]
    exclude: Option<String>,

    /// Minimum file size in bytes
    #[arg(long, value_name = "BYTES", default_value_t = 0)]
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

fn build_globset(include: &str, exclude: &Option<String>) -> Result<GlobSet, DeleterError> {
    let mut builder = GlobSetBuilder::new();

    builder.add(Glob::new(include).map_err(|e| DeleterError::Glob(e.to_string()))?);

    if let Some(ex) = exclude {
        builder.add(Glob::new(ex).map_err(|e| DeleterError::Glob(e.to_string()))?);
    }

    builder
        .build()
        .map_err(|e| DeleterError::Glob(e.to_string()))
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
                let mut preview = Vec::new();

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

                    if preview.len() < 10 {
                        preview.push(entry.path().to_path_buf());
                    }
                }

                (files, bytes, preview)
            })
        })
        .buffer_unordered(parallelism)
        .collect::<Vec<_>>()
        .await;

    let mut total_files = 0;
    let mut total_bytes = 0;
    let mut preview_all = Vec::new();

    for r in results {
        let (f, b, p) = r.map_err(|_| DeleterError::Join)?;
        total_files += f;
        total_bytes += b;

        for x in p {
            if preview_all.len() < 10 {
                preview_all.push(x);
            }
        }
    }

    Ok((total_files, total_bytes, preview_all))
}

//
// ──────────────────────────────────────────────────────────
// Delete / Trash phase (true streaming)
// ──────────────────────────────────────────────────────────
//

async fn delete_streaming(
    job_paths: Vec<PathBuf>,
    globset: GlobSet,
    dry_run: bool,
    use_trash: bool,
    parallelism: usize,
    min_size: u64,
    pb: ProgressBar,
) -> Result<u64, DeleterError> {
    let deleted = Arc::new(AtomicU64::new(0));

    let stream = stream::iter(job_paths).flat_map(|root| {
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
// Confirm
// ──────────────────────────────────────────────────────────
//

fn confirm(files: u64, bytes: u64, preview: &[PathBuf], trash: bool) -> Result<(), DeleterError> {
    println!("\n⚠️  DANGER");
    println!("Files : {files}");
    println!("Size  : {}", format_size(bytes));
    println!(
        "Mode  : {}",
        if trash { "TRASH" } else { "PERMANENT DELETE" }
    );

    for p in preview {
        println!("  {}", p.display());
    }

    print!("\nType YES to continue: ");
    io::stdout().flush()?;

    let mut s = String::new();
    io::stdin().read_line(&mut s)?;

    if s.trim() == "YES" {
        Ok(())
    } else {
        Err(DeleterError::Cancelled)
    }
}

//
// ──────────────────────────────────────────────────────────
// MAIN
// ──────────────────────────────────────────────────────────
//

/// Parse paths from file content (comma, space, or newline separated)
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

#[tokio::main]
async fn main() -> Result<(), DeleterError> {
    let cli = Cli::parse();

    // Collect all paths (direct dirs or from files)
    let mut all_paths: Vec<PathBuf> = Vec::new();
    let mut seen = HashSet::new();

    for path in &cli.paths {
        let metadata = fs::metadata(path).await?;
        if metadata.is_file() {
            // Read file and parse paths
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

    // Validate all are directories
    for path in &all_paths {
        if !fs::metadata(path).await?.is_dir() {
            return Err(DeleterError::JobDir(path.clone()));
        }
    }

    let globset = build_globset(&cli.glob, &cli.exclude)?;

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

    println!("Found {files} files ({}).", format_size(bytes));

    if !cli.dry_run && !cli.yes {
        confirm(files, bytes, &preview, cli.trash)?;
    }

    let mp = MultiProgress::new();
    let pb = mp.add(ProgressBar::new(files));

    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.red} [{elapsed_precise}] [{bar:40}] {pos}/{len}")
            .unwrap(),
    );

    println!("🗑️  Processing...");

    let deleted = delete_streaming(
        all_paths,
        globset,
        cli.dry_run,
        cli.trash,
        cli.parallelism,
        cli.min_size,
        pb,
    )
    .await?;

    if cli.dry_run {
        println!("Preview complete.");
    } else {
        println!("✅ Removed {deleted} files, freed {}", format_size(bytes));
    }

    Ok(())
}
