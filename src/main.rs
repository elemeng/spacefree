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
/// Supports: B (bytes, default), K/KB (kilobytes), M/MB (megabytes), 
/// G/GB (gigabytes), T/TB (terabytes). Case insensitive.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(0);
    }

    // Find where the number ends and unit begins
    let (num_part, unit_part) = s.find(|c: char| !c.is_ascii_digit())
        .map(|i| s.split_at(i))
        .unwrap_or((s, ""));

    let num: u64 = num_part.parse().map_err(|_| {
        format!("invalid number: {}", num_part)
    })?;

    let unit = unit_part.trim().to_uppercase();

    let multiplier = match unit.as_str() {
        "" | "B" => 1u64,
        "K" | "KB" => 1024u64,
        "M" | "MB" => 1024u64 * 1024,
        "G" | "GB" => 1024u64 * 1024 * 1024,
        "T" | "TB" => 1024u64 * 1024 * 1024 * 1024,
        _ => return Err(format!("invalid unit: {}", unit_part)),
    };

    num.checked_mul(multiplier)
        .ok_or_else(|| "size overflow".to_string())
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

fn confirm<R: io::BufRead>(
    files: u64,
    bytes: u64,
    preview: &[PathBuf],
    trash: bool,
    mut reader: R,
) -> Result<(), DeleterError> {
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
    reader.read_line(&mut s)?;

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

/// Collect paths from CLI arguments (directories or files containing paths)
async fn collect_paths(input_paths: &[PathBuf]) -> Result<Vec<PathBuf>, DeleterError> {
    let mut all_paths: Vec<PathBuf> = Vec::new();
    let mut seen = HashSet::new();

    for path in input_paths {
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

    Ok(all_paths)
}

async fn run(cli: Cli) -> Result<(), DeleterError> {
    let all_paths = collect_paths(&cli.paths).await?;

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
        confirm(files, bytes, &preview, cli.trash, io::stdin().lock())?;
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

#[tokio::main]
async fn main() -> Result<(), DeleterError> {
    let cli = Cli::parse();
    run(cli).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    // ========== format_size tests ==========
    #[test]
    fn test_format_size_bytes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(100), "100 B");
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn test_format_size_kb() {
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(1024 * 1024 - 1), "1024.00 KB");
    }

    #[test]
    fn test_format_size_mb() {
        assert_eq!(format_size(1024 * 1024), "1.00 MB");
        assert_eq!(format_size(1024 * 1024 * 512), "512.00 MB");
    }

    #[test]
    fn test_format_size_gb() {
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
        assert_eq!(format_size(1024u64.pow(3) * 2), "2.00 GB");
    }

    #[test]
    fn test_format_size_tb() {
        assert_eq!(format_size(1024u64.pow(4)), "1.00 TB");
    }

    // ========== parse_paths_from_content tests ==========
    #[test]
    fn test_parse_paths_empty() {
        assert!(parse_paths_from_content("").is_empty());
        assert!(parse_paths_from_content("   ").is_empty());
        assert!(parse_paths_from_content("\n\n").is_empty());
    }

    #[test]
    fn test_parse_paths_single() {
        let paths = parse_paths_from_content("J12");
        assert_eq!(paths, vec![PathBuf::from("J12")]);
    }

    #[test]
    fn test_parse_paths_space_separated() {
        let paths = parse_paths_from_content("J12 J13 J14");
        assert_eq!(
            paths,
            vec![
                PathBuf::from("J12"),
                PathBuf::from("J13"),
                PathBuf::from("J14"),
            ]
        );
    }

    #[test]
    fn test_parse_paths_comma_separated() {
        let paths = parse_paths_from_content("J12,J13,J14");
        assert_eq!(
            paths,
            vec![
                PathBuf::from("J12"),
                PathBuf::from("J13"),
                PathBuf::from("J14"),
            ]
        );
    }

    #[test]
    fn test_parse_paths_mixed_separators() {
        let paths = parse_paths_from_content("J12, J13 J14\tJ15");
        assert_eq!(
            paths,
            vec![
                PathBuf::from("J12"),
                PathBuf::from("J13"),
                PathBuf::from("J14"),
                PathBuf::from("J15"),
            ]
        );
    }

    #[test]
    fn test_parse_paths_newline_separated() {
        let paths = parse_paths_from_content("J12\nJ13\nJ14");
        assert_eq!(
            paths,
            vec![
                PathBuf::from("J12"),
                PathBuf::from("J13"),
                PathBuf::from("J14"),
            ]
        );
    }

    #[test]
    fn test_parse_paths_dedup() {
        let paths = parse_paths_from_content("J12 J12 J13");
        assert_eq!(paths, vec![PathBuf::from("J12"), PathBuf::from("J13"),]);
    }

    #[test]
    fn test_parse_paths_with_extra_whitespace() {
        let paths = parse_paths_from_content("  J12  ,  J13  \n  J14  ");
        assert_eq!(
            paths,
            vec![
                PathBuf::from("J12"),
                PathBuf::from("J13"),
                PathBuf::from("J14"),
            ]
        );
    }

    // ========== build_globset tests ==========
    #[test]
    fn test_build_globset_simple() {
        let gs = build_globset("*.txt", &None).unwrap();
        assert!(gs.is_match("file.txt"));
        assert!(gs.is_match("test.txt"));
        assert!(!gs.is_match("file.md"));
    }

    #[test]
    fn test_build_globset_with_exclude() {
        let gs = build_globset("**/*.mrc", &Some("**/*.txt".to_string())).unwrap();
        assert!(gs.is_match("data/file.mrc"));
        assert!(gs.is_match("file.txt")); // exclude pattern is also in the globset
    }

    #[test]
    fn test_build_globset_invalid_pattern() {
        let result = build_globset("[invalid", &None);
        assert!(matches!(result, Err(DeleterError::Glob(_))));
    }

    #[test]
    fn test_build_globset_invalid_exclude() {
        let result = build_globset("*.txt", &Some("[invalid".to_string()));
        assert!(matches!(result, Err(DeleterError::Glob(_))));
    }

    // ========== DeleterError tests ==========
    #[test]
    fn test_error_from_io() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let err: DeleterError = io_err.into();
        assert!(matches!(err, DeleterError::Io(_)));
        assert!(err.to_string().contains("IO error"));
    }

    #[test]
    fn test_error_display() {
        let err = DeleterError::JobDir(PathBuf::from("/bad/path"));
        assert!(err.to_string().contains("Invalid job directory"));
        assert!(err.to_string().contains("/bad/path"));

        let err = DeleterError::NoValidPaths;
        assert_eq!(err.to_string(), "No valid paths provided");

        let err = DeleterError::Cancelled;
        assert_eq!(err.to_string(), "User cancelled");

        let err = DeleterError::Join;
        assert_eq!(err.to_string(), "Task join error");

        let err = DeleterError::Glob("bad pattern".to_string());
        assert!(err.to_string().contains("Invalid glob"));
        assert!(err.to_string().contains("bad pattern"));
    }

    // ========== Cli tests (parse validation) ==========
    #[test]
    fn test_cli_parse_minimal() {
        let cli = Cli::parse_from(["spacefree", "J12"]);
        assert_eq!(cli.paths, vec![PathBuf::from("J12")]);
        assert_eq!(cli.glob, "**/*.mrc");
        assert_eq!(cli.min_size, 0);
        assert!(!cli.trash);
        assert!(!cli.dry_run);
        assert!(!cli.yes);
    }

    #[test]
    fn test_cli_parse_multiple_paths() {
        let cli = Cli::parse_from(["spacefree", "J12", "J13", "J14"]);
        assert_eq!(
            cli.paths,
            vec![
                PathBuf::from("J12"),
                PathBuf::from("J13"),
                PathBuf::from("J14"),
            ]
        );
    }

    #[test]
    fn test_cli_parse_all_options() {
        let cli = Cli::parse_from([
            "spacefree",
            "-g",
            "*.txt",
            "--exclude",
            "*.log",
            "--min-size",
            "100",
            "--trash",
            "--dry-run",
            "-y",
            "-p",
            "8",
            "J12",
        ]);
        assert_eq!(cli.glob, "*.txt");
        assert_eq!(cli.exclude, Some("*.log".to_string()));
        assert_eq!(cli.min_size, 100);
        assert!(cli.trash);
        assert!(cli.dry_run);
        assert!(cli.yes);
        assert_eq!(cli.parallelism, 8);
    }

    // ========== Async function tests ==========
    #[tokio::test]
    async fn test_scan_only_empty_dir() {
        let temp = TempDir::new().unwrap();
        let gs = build_globset("*.txt", &None).unwrap();

        let (files, bytes, preview) = scan_only(vec![temp.path().to_path_buf()], gs, 0, 4)
            .await
            .unwrap();

        assert_eq!(files, 0);
        assert_eq!(bytes, 0);
        assert!(preview.is_empty());
    }

    #[tokio::test]
    async fn test_scan_only_with_files() {
        let temp = TempDir::new().unwrap();

        // Create test files
        fs::write(temp.path().join("file1.txt"), "hello")
            .await
            .unwrap();
        fs::write(temp.path().join("file2.txt"), "world!")
            .await
            .unwrap();
        fs::write(temp.path().join("file.md"), "markdown")
            .await
            .unwrap();

        let gs = build_globset("*.txt", &None).unwrap();

        let (files, bytes, preview) = scan_only(vec![temp.path().to_path_buf()], gs, 0, 4)
            .await
            .unwrap();

        assert_eq!(files, 2);
        assert_eq!(bytes, 11); // "hello" (5) + "world!" (6)
        assert_eq!(preview.len(), 2);
    }

    #[tokio::test]
    async fn test_scan_only_with_min_size() {
        let temp = TempDir::new().unwrap();

        fs::write(temp.path().join("small.txt"), "hi")
            .await
            .unwrap();
        fs::write(temp.path().join("large.txt"), "this is a large file")
            .await
            .unwrap();

        let gs = build_globset("*.txt", &None).unwrap();

        let (files, _bytes, _) = scan_only(
            vec![temp.path().to_path_buf()],
            gs,
            10, // min_size
            4,
        )
        .await
        .unwrap();

        assert_eq!(files, 1); // only large.txt
    }

    #[tokio::test]
    async fn test_scan_only_multiple_dirs() {
        let temp1 = TempDir::new().unwrap();
        let temp2 = TempDir::new().unwrap();

        fs::write(temp1.path().join("a.txt"), "aaa").await.unwrap();
        fs::write(temp2.path().join("b.txt"), "bbbb").await.unwrap();

        let gs = build_globset("*.txt", &None).unwrap();

        let (files, bytes, _) = scan_only(
            vec![temp1.path().to_path_buf(), temp2.path().to_path_buf()],
            gs,
            0,
            4,
        )
        .await
        .unwrap();

        assert_eq!(files, 2);
        assert_eq!(bytes, 7);
    }

    #[tokio::test]
    async fn test_delete_streaming_dry_run() {
        let temp = TempDir::new().unwrap();

        fs::write(temp.path().join("file.txt"), "content")
            .await
            .unwrap();

        let gs = build_globset("*.txt", &None).unwrap();
        let pb = ProgressBar::hidden();

        let deleted = delete_streaming(
            vec![temp.path().to_path_buf()],
            gs,
            true, // dry_run
            false,
            4,
            0,
            pb,
        )
        .await
        .unwrap();

        // File should still exist in dry_run mode
        assert!(temp.path().join("file.txt").exists());

        // But deleted counter is still 0 in dry_run
        assert_eq!(deleted, 0);
    }

    #[tokio::test]
    async fn test_delete_streaming_actual_delete() {
        let temp = TempDir::new().unwrap();

        fs::write(temp.path().join("file.txt"), "content")
            .await
            .unwrap();

        let gs = build_globset("*.txt", &None).unwrap();
        let pb = ProgressBar::hidden();

        let deleted = delete_streaming(
            vec![temp.path().to_path_buf()],
            gs,
            false, // actual delete
            false,
            4,
            0,
            pb,
        )
        .await
        .unwrap();

        assert_eq!(deleted, 1);
        assert!(!temp.path().join("file.txt").exists());
    }

    #[tokio::test]
    async fn test_delete_streaming_with_min_size() {
        let temp = TempDir::new().unwrap();

        fs::write(temp.path().join("small.txt"), "x").await.unwrap();
        fs::write(temp.path().join("large.txt"), "this is large")
            .await
            .unwrap();

        let gs = build_globset("*.txt", &None).unwrap();
        let pb = ProgressBar::hidden();

        let deleted = delete_streaming(
            vec![temp.path().to_path_buf()],
            gs,
            false,
            false,
            4,
            5, // min_size
            pb,
        )
        .await
        .unwrap();

        assert_eq!(deleted, 1); // only large.txt
        assert!(temp.path().join("small.txt").exists());
        assert!(!temp.path().join("large.txt").exists());
    }

    // ========== confirm tests ==========
    #[test]
    fn test_confirm_yes() {
        let input = b"YES\n";
        let result = confirm(10, 1024, &[], false, &input[..]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_confirm_no() {
        let input = b"no\n";
        let result = confirm(10, 1024, &[], false, &input[..]);
        assert!(matches!(result, Err(DeleterError::Cancelled)));
    }

    #[test]
    fn test_confirm_empty() {
        let input = b"\n";
        let result = confirm(10, 1024, &[], false, &input[..]);
        assert!(matches!(result, Err(DeleterError::Cancelled)));
    }

    #[test]
    fn test_confirm_with_preview() {
        let preview = vec![
            PathBuf::from("/tmp/file1.txt"),
            PathBuf::from("/tmp/file2.txt"),
        ];
        let input = b"YES\n";
        let result = confirm(2, 2048, &preview, true, &input[..]);
        assert!(result.is_ok());
    }

    // ========== collect_paths tests ==========
    #[tokio::test]
    async fn test_collect_paths_single_dir() {
        let temp = TempDir::new().unwrap();
        let paths = collect_paths(&[temp.path().to_path_buf()]).await.unwrap();
        assert_eq!(paths, vec![temp.path().to_path_buf()]);
    }

    #[tokio::test]
    async fn test_collect_paths_multiple_dirs() {
        let temp1 = TempDir::new().unwrap();
        let temp2 = TempDir::new().unwrap();

        let paths = collect_paths(&[temp1.path().to_path_buf(), temp2.path().to_path_buf()])
            .await
            .unwrap();

        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&temp1.path().to_path_buf()));
        assert!(paths.contains(&temp2.path().to_path_buf()));
    }

    #[tokio::test]
    async fn test_collect_paths_from_file() {
        let temp = TempDir::new().unwrap();
        let job1 = TempDir::new().unwrap();
        let job2 = TempDir::new().unwrap();

        // Create a file containing paths
        let list_file = temp.path().join("jobs.txt");
        let content = format!("{}\n{}\n", job1.path().display(), job2.path().display());
        fs::write(&list_file, content).await.unwrap();

        let paths = collect_paths(&[list_file]).await.unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&job1.path().to_path_buf()));
        assert!(paths.contains(&job2.path().to_path_buf()));
    }

    #[tokio::test]
    async fn test_collect_paths_empty() {
        let temp = TempDir::new().unwrap();
        let empty_file = temp.path().join("empty.txt");
        fs::write(&empty_file, "").await.unwrap();

        let result = collect_paths(&[empty_file]).await;
        assert!(matches!(result, Err(DeleterError::NoValidPaths)));
    }

    #[tokio::test]
    async fn test_collect_paths_dedup() {
        let temp = TempDir::new().unwrap();

        // Same directory twice
        let paths = collect_paths(&[temp.path().to_path_buf(), temp.path().to_path_buf()])
            .await
            .unwrap();

        assert_eq!(paths.len(), 1);
    }

    #[tokio::test]
    async fn test_collect_paths_file_not_found() {
        let result = collect_paths(&[PathBuf::from("/nonexistent/path")]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_collect_paths_mixed_dirs_and_files() {
        let temp = TempDir::new().unwrap();
        let job_dir = TempDir::new().unwrap();

        // Create a file containing a path
        let list_file = temp.path().join("jobs.txt");
        fs::write(&list_file, format!("{}\n", job_dir.path().display()))
            .await
            .unwrap();

        // Mix of dir and file
        let paths = collect_paths(&[
            temp.path().to_path_buf(), // directory
            list_file,                 // file containing paths
        ])
        .await
        .unwrap();

        assert!(paths.contains(&temp.path().to_path_buf()));
        assert!(paths.contains(&job_dir.path().to_path_buf()));
    }

    // ========== scan_only preview limit tests ==========
    #[tokio::test]
    async fn test_scan_only_preview_limit() {
        let temp = TempDir::new().unwrap();

        // Create more than 10 files
        for i in 0..15 {
            fs::write(temp.path().join(format!("file{i}.txt")), "content")
                .await
                .unwrap();
        }

        let gs = build_globset("*.txt", &None).unwrap();

        let (_files, _bytes, preview) = scan_only(vec![temp.path().to_path_buf()], gs, 0, 4)
            .await
            .unwrap();

        // Preview should be limited to 10 items
        assert_eq!(preview.len(), 10);
    }

    // ========== globset exclude pattern tests ==========
    #[test]
    fn test_build_globset_exclude_matches() {
        let gs = build_globset("**/*.txt", &Some("**/exclude*.txt".to_string())).unwrap();
        assert!(gs.is_match("file.txt"));
        assert!(gs.is_match("exclude_me.txt")); // patterns are ORed in GlobSet
    }

    #[tokio::test]
    async fn test_scan_only_with_glob_pattern() {
        let temp = TempDir::new().unwrap();

        fs::write(temp.path().join("file.txt"), "content")
            .await
            .unwrap();
        fs::write(temp.path().join("file.md"), "content")
            .await
            .unwrap();
        fs::write(temp.path().join("file.rs"), "content")
            .await
            .unwrap();

        let gs = build_globset("*.txt", &None).unwrap();

        let (files, _bytes, _) = scan_only(vec![temp.path().to_path_buf()], gs, 0, 4)
            .await
            .unwrap();

        assert_eq!(files, 1); // only .txt files
    }

    // ========== DeleterError Debug tests ==========
    #[test]
    fn test_error_debug() {
        let err = DeleterError::NoValidPaths;
        let debug = format!("{:?}", err);
        assert!(debug.contains("NoValidPaths"));
    }

    // ========== run() tests ==========
    #[tokio::test]
    async fn test_run_no_matches() {
        let temp = TempDir::new().unwrap();

        let cli = Cli {
            paths: vec![temp.path().to_path_buf()],
            glob: "*.nonexistent".to_string(),
            exclude: None,
            min_size: 0,
            trash: false,
            dry_run: false,
            yes: true,
            parallelism: 4,
        };

        let result = run(cli).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_run_dry_run() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.txt"), "content")
            .await
            .unwrap();

        let cli = Cli {
            paths: vec![temp.path().to_path_buf()],
            glob: "*.txt".to_string(),
            exclude: None,
            min_size: 0,
            trash: false,
            dry_run: true,
            yes: false,
            parallelism: 4,
        };

        let result = run(cli).await;
        assert!(result.is_ok());
        // File should still exist after dry run
        assert!(temp.path().join("test.txt").exists());
    }

    #[tokio::test]
    async fn test_run_with_files_auto_confirm() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.txt"), "content")
            .await
            .unwrap();

        let cli = Cli {
            paths: vec![temp.path().to_path_buf()],
            glob: "*.txt".to_string(),
            exclude: None,
            min_size: 0,
            trash: false,
            dry_run: false,
            yes: true, // auto confirm
            parallelism: 4,
        };

        let result = run(cli).await;
        assert!(result.is_ok());
        // File should be deleted
        assert!(!temp.path().join("test.txt").exists());
    }

    #[tokio::test]
    async fn test_run_with_exclude() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("include.txt"), "content")
            .await
            .unwrap();
        fs::write(temp.path().join("exclude.log"), "log content")
            .await
            .unwrap();

        let cli = Cli {
            paths: vec![temp.path().to_path_buf()],
            glob: "*.*".to_string(),
            exclude: Some("*.log".to_string()),
            min_size: 0,
            trash: false,
            dry_run: true,
            yes: true,
            parallelism: 4,
        };

        let result = run(cli).await;
        assert!(result.is_ok());
        // Both files should still exist in dry run
        assert!(temp.path().join("include.txt").exists());
        assert!(temp.path().join("exclude.log").exists());
    }

    #[tokio::test]
    async fn test_run_with_min_size_filter() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("small.txt"), "x").await.unwrap();
        fs::write(temp.path().join("large.txt"), "this is large content")
            .await
            .unwrap();

        let cli = Cli {
            paths: vec![temp.path().to_path_buf()],
            glob: "*.txt".to_string(),
            exclude: None,
            min_size: 10, // Only files >= 10 bytes
            trash: false,
            dry_run: true,
            yes: true,
            parallelism: 4,
        };

        let result = run(cli).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_run_trash_mode_dry_run() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.txt"), "content")
            .await
            .unwrap();

        let cli = Cli {
            paths: vec![temp.path().to_path_buf()],
            glob: "*.txt".to_string(),
            exclude: None,
            min_size: 0,
            trash: true, // trash mode
            dry_run: true,
            yes: true,
            parallelism: 4,
        };

        let result = run(cli).await;
        assert!(result.is_ok());
        // File should still exist in dry run
        assert!(temp.path().join("test.txt").exists());
    }

    #[tokio::test]
    async fn test_run_multiple_paths() {
        let temp1 = TempDir::new().unwrap();
        let temp2 = TempDir::new().unwrap();

        fs::write(temp1.path().join("a.txt"), "aaa").await.unwrap();
        fs::write(temp2.path().join("b.txt"), "bbbb").await.unwrap();

        let cli = Cli {
            paths: vec![temp1.path().to_path_buf(), temp2.path().to_path_buf()],
            glob: "*.txt".to_string(),
            exclude: None,
            min_size: 0,
            trash: false,
            dry_run: false,
            yes: true,
            parallelism: 4,
        };

        let result = run(cli).await;
        assert!(result.is_ok());
        assert!(!temp1.path().join("a.txt").exists());
        assert!(!temp2.path().join("b.txt").exists());
    }

    // ========== Edge case tests for error paths ==========
    #[tokio::test]
    async fn test_collect_paths_nested_dir_validation() {
        let temp = TempDir::new().unwrap();

        // Create a file (not a dir) in the list file
        let fake_file = temp.path().join("not_a_dir.txt");
        fs::write(&fake_file, "this is not a directory")
            .await
            .unwrap();

        let list_file = temp.path().join("jobs.txt");
        fs::write(&list_file, format!("{}\n", fake_file.display()))
            .await
            .unwrap();

        // Should fail because fake_file is not a directory
        let result = collect_paths(&[list_file]).await;
        assert!(matches!(result, Err(DeleterError::JobDir(_))));
    }

    #[tokio::test]
    async fn test_scan_only_nested_dirs() {
        let temp = TempDir::new().unwrap();

        // Create nested structure
        let nested = temp.path().join("level1/level2");
        fs::create_dir_all(&nested).await.unwrap();
        fs::write(nested.join("deep.txt"), "deep content")
            .await
            .unwrap();
        fs::write(temp.path().join("shallow.txt"), "shallow")
            .await
            .unwrap();

        let gs = build_globset("**/*.txt", &None).unwrap();

        let (files, bytes, _) = scan_only(vec![temp.path().to_path_buf()], gs, 0, 4)
            .await
            .unwrap();

        assert_eq!(files, 2);
        assert_eq!(bytes, 19); // "deep content" (12) + "shallow" (7) + newline
    }

    #[tokio::test]
    async fn test_scan_only_large_parallelism() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("test.txt"), "x").await.unwrap();

        let gs = build_globset("*.txt", &None).unwrap();

        // Test with high parallelism value
        let (files, _, _) = scan_only(
            vec![temp.path().to_path_buf()],
            gs,
            0,
            100, // high parallelism
        )
        .await
        .unwrap();

        assert_eq!(files, 1);
    }

    #[test]
    fn test_parse_paths_with_tabs() {
        let paths = parse_paths_from_content("J12\tJ13\tJ14");
        assert_eq!(
            paths,
            vec![
                PathBuf::from("J12"),
                PathBuf::from("J13"),
                PathBuf::from("J14"),
            ]
        );
    }

    #[test]
    fn test_parse_paths_multiple_commas() {
        let paths = parse_paths_from_content("J12,,,J13");
        assert_eq!(paths, vec![PathBuf::from("J12"), PathBuf::from("J13"),]);
    }

    // ========== parse_size tests ==========
    #[test]
    fn test_parse_size_bytes_only() {
        assert_eq!(parse_size("0").unwrap(), 0);
        assert_eq!(parse_size("100").unwrap(), 100);
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("0B").unwrap(), 0);
        assert_eq!(parse_size("100b").unwrap(), 100);
    }

    #[test]
    fn test_parse_size_kilobytes() {
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("1k").unwrap(), 1024);
        assert_eq!(parse_size("1KB").unwrap(), 1024);
        assert_eq!(parse_size("1kb").unwrap(), 1024);
        assert_eq!(parse_size("10K").unwrap(), 10 * 1024);
        assert_eq!(parse_size("512kB").unwrap(), 512 * 1024);
    }

    #[test]
    fn test_parse_size_megabytes() {
        assert_eq!(parse_size("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("1m").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("1MB").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("1mb").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("100M").unwrap(), 100 * 1024 * 1024);
    }

    #[test]
    fn test_parse_size_gigabytes() {
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("1g").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("1GB").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("2G").unwrap(), 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn test_parse_size_terabytes() {
        assert_eq!(parse_size("1T").unwrap(), 1024u64 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("1t").unwrap(), 1024u64 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("1TB").unwrap(), 1024u64 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("1tb").unwrap(), 1024u64 * 1024 * 1024 * 1024);
    }

    #[test]
    fn test_parse_size_with_whitespace() {
        assert_eq!(parse_size("  100  ").unwrap(), 100);
        assert_eq!(parse_size("  10K  ").unwrap(), 10 * 1024);
    }

    #[test]
    fn test_parse_size_empty() {
        assert_eq!(parse_size("").unwrap(), 0);
        assert_eq!(parse_size("   ").unwrap(), 0);
    }

    #[test]
    fn test_parse_size_invalid() {
        assert!(parse_size("abc").is_err());
        assert!(parse_size("10X").is_err());
        assert!(parse_size("10KBX").is_err());
    }

    #[test]
    fn test_parse_size_overflow() {
        // A very large number that would overflow when multiplied
        // Number part too big for u64
        let result = parse_size("99999999999999999999T");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid number"));
        
        // Number that would overflow with unit
        let result = parse_size("18446744073709551615K"); // u64::MAX * 1024 would overflow
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("overflow"));
    }

    #[test]
    fn test_cli_parse_with_size_units() {
        let cli = Cli::parse_from(["spacefree", "--min-size", "10M", "J12"]);
        assert_eq!(cli.min_size, 10 * 1024 * 1024);

        let cli = Cli::parse_from(["spacefree", "--min-size", "1G", "J12"]);
        assert_eq!(cli.min_size, 1024 * 1024 * 1024);

        let cli = Cli::parse_from(["spacefree", "--min-size", "512K", "J12"]);
        assert_eq!(cli.min_size, 512 * 1024);
    }
}
