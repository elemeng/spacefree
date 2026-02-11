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

    #[error("Invalid parent directory: {0}")]
    ParentDir(PathBuf),

    #[error("No valid job numbers provided")]
    NoValidJobs,

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
    #[arg(short, long)]
    dir: PathBuf,

    #[arg(short, long, num_args = 1..)]
    job: Vec<String>,

    #[arg(short, long, default_value = "**/*.mrc")]
    glob: String,

    #[arg(long)]
    exclude: Option<String>,

    #[arg(short, long)]
    dry_run: bool,

    #[arg(short, long)]
    yes: bool,

    /// Move to system trash instead of permanent delete
    #[arg(long)]
    trash: bool,

    #[arg(short, long, default_value_t = num_cpus::get() * 4)]
    parallelism: usize,

    #[arg(long, default_value_t = 0)]
    min_size: u64,
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

fn parse_jobs(raw: &[String]) -> Result<Vec<String>, DeleterError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for r in raw {
        for part in r.split(|c| c == ',' || c == ' ') {
            let digits: String = part.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                continue;
            }

            let j = format!("J{}", digits);

            if seen.insert(j.clone()) {
                out.push(j);
            }
        }
    }

    if out.is_empty() {
        return Err(DeleterError::NoValidJobs);
    }

    Ok(out)
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

#[tokio::main]
async fn main() -> Result<(), DeleterError> {
    let cli = Cli::parse();

    if !fs::metadata(&cli.dir).await?.is_dir() {
        return Err(DeleterError::ParentDir(cli.dir));
    }

    let jobs = parse_jobs(&cli.job)?;
    let job_paths: Vec<PathBuf> = jobs.iter().map(|j| cli.dir.join(j)).collect();

    let globset = build_globset(&cli.glob, &cli.exclude)?;

    println!("🔍 Scanning...");

    let (files, bytes, preview) = scan_only(
        job_paths.clone(),
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
        job_paths,
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
