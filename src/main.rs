use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use std::{
    io::Write,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::{fs, signal};
use tracing::{debug, error, info, warn};
use walkdir::WalkDir;

// Module declarations
mod cli;
mod config;
mod delete;
mod error;
mod log;
mod scan;
mod storage;

// Re-exports for convenience
use cli::{Cli, build_globset, format_dirs, format_size, is_root_path};
use config::DeleteConfig;
use delete::run_deletion_pipeline;
use error::DeleterError;
use log::LogMode;
use scan::collect_paths;
use storage::StorageKind;

/// Global shutdown flag for graceful cancellation
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Check if shutdown has been requested
pub fn is_shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Relaxed)
}

/// Main application logic
async fn run(cli: Cli) -> Result<(), DeleterError> {
    let all_paths = collect_paths(&cli.paths, &cli.path_list_file).await?;
    let (globset, exclude_glob) = build_globset(cli.glob.as_deref(), &cli.exclude)?;

    println!("🔍 Scanning...");

    let glob_pattern = cli.glob.as_deref().unwrap_or("**/*").to_string();

    // Detect storage type and set optimal parallelism
    let (parallelism, storage_kind) = if cli.parallelism == 0 {
        // Auto-detect based on storage type
        let kind = all_paths
            .first()
            .map(|p| StorageKind::from_path(p))
            .unwrap_or(StorageKind::Unknown);
        let optimal = kind.optimal_parallelism();
        println!("  Storage: {:?} → parallelism: {}", kind, optimal);
        (optimal, kind)
    } else {
        // User-specified parallelism, still detect storage for sorting optimization
        let kind = all_paths
            .first()
            .map(|p| StorageKind::from_path(p))
            .unwrap_or(StorageKind::Unknown);
        (cli.parallelism, kind)
    };

    let config = std::sync::Arc::new(DeleteConfig {
        use_trash: cli.trash,
        dry_run: cli.dry_run,
        parallelism,
        min_size: cli.min_size,
        max_size: cli.max_size,
        min_age: cli.min_age,
        max_age: cli.max_age,
        verbose: cli.verbose,
        dirs: cli.dirs,
        no_follow_symlinks: cli.no_follow_symlinks,
        glob_pattern: glob_pattern.clone(),
        glob_matcher: globset,
        exclude_matcher: exclude_glob,
        skip_glob_match: glob_pattern == "**/*",
        storage_kind,
    });

    // Check for root directory and require explicit confirmation
    for path in &all_paths {
        if is_root_path(path) {
            if !cli.delete_root_dir {
                eprintln!("❌ ERROR: Attempting to delete root directory");
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
                eprintln!("❌ ERROR: Deleting root directory requires -y flag");
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
        let walkdir = WalkDir::new(dir).follow_links(!cli.no_follow_symlinks);
        for entry in walkdir.into_iter().filter_map(|e| e.ok()).take(1000) {
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

    println!("🗑️  Processing...");

    let log_mode = LogMode::from_opt(&cli.log);
    let log_path = if cli.dry_run { None } else { log_mode.path() };

    // Create progress bar
    let pb = ProgressBar::new(total_estimate as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
            )
            .unwrap()
            .progress_chars("#>-"),
    );

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

    // Set up Ctrl+C handler for graceful shutdown
    tokio::spawn(async move {
        if let Err(e) = signal::ctrl_c().await {
            warn!("Failed to listen for Ctrl+C: {}", e);
            return;
        }
        println!("\n⚠️  Shutdown requested (Ctrl+C), finishing current operations...");
        SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
    });

    debug!("Starting spacefree with CLI args: {:?}", cli);

    if let Err(e) = run(cli).await {
        error!("Application error: {}", e);
        return Err(e);
    }

    info!("spacefree completed successfully");
    Ok(())
}
