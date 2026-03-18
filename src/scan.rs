use crate::config::{DeleteConfig, ScanResult};
use crate::error::DeleterError;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::{fs, sync::mpsc};
use tracing::{info, warn};
use walkdir::WalkDir;

/// Parse paths from file content (comma/space/newline separated)
pub fn parse_paths_from_content(content: &str) -> Vec<PathBuf> {
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

/// Collect and validate all paths from input and path list files
pub async fn collect_paths(
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

/// Scan a directory tree and send matching files to the channel
pub async fn scan_to_channel(
    root: PathBuf,
    file_tx: mpsc::Sender<ScanResult>,
    config: Arc<DeleteConfig>,
) -> Result<(), DeleterError> {
    tokio::task::spawn_blocking(move || {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("System time went backwards")
            .as_secs();

        let mut scan_dirs = Vec::new();

        let walkdir = WalkDir::new(&root).follow_links(!config.no_follow_symlinks);
        for entry in walkdir.into_iter().filter_map(|e| e.ok()) {
            // Check for shutdown request
            if crate::is_shutdown_requested() {
                info!("Shutdown requested, stopping scan early");
                break;
            }

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
                        .duration_since(std::time::UNIX_EPOCH)
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

                // Check glob pattern for files (skip if using default "**/*" pattern)
                if !config.skip_glob_match && !config.glob_matcher.is_match(path) {
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

/// Scan individual files directly
pub async fn scan_files_direct(
    paths: Vec<PathBuf>,
    file_tx: mpsc::Sender<ScanResult>,
    config: Arc<DeleteConfig>,
) -> Result<(), DeleterError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
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
                .duration_since(std::time::UNIX_EPOCH)
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
