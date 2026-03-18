use crate::config::{DeleteConfig, ScanResult};
use crate::error::DeleterError;
use crate::log::DeletedItem;
use crate::scan::{scan_files_direct, scan_to_channel};
use indicatif::ProgressBar;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::{fs, io::AsyncWriteExt, sync::mpsc, task::spawn_blocking};
use tracing::{debug, error, info};
use trash::delete as trash_delete;

/// Run the deletion pipeline with streaming scan and delete
pub async fn run_deletion_pipeline(
    directories: Vec<PathBuf>,
    individual_files: Vec<PathBuf>,
    config: Arc<DeleteConfig>,
    pb: ProgressBar,
    log_path: Option<PathBuf>,
) -> Result<(u64, u64, u64, Vec<PathBuf>), DeleterError> {
    // Channels for streaming pipeline - size tuned based on parallelism
    let channel_capacity = (config.parallelism * 8).max(64);
    let (scan_tx, mut scan_rx) = mpsc::channel::<ScanResult>(channel_capacity);
    let (deleted_tx, mut deleted_rx) = mpsc::channel::<DeletedItem>(channel_capacity);
    let (trash_tx, mut trash_rx) = mpsc::channel::<PathBuf>(channel_capacity);
    let (fail_tx, mut fail_rx) = mpsc::channel::<PathBuf>((config.parallelism * 2).max(16));
    let fail_tx = Arc::new(fail_tx);

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
                        deleted_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
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

    // Delete consumer with proper concurrency control using for_each_concurrent
    let delete_count = deleted_count.clone();
    let fail_count = failed_count.clone();
    let total_bytes = bytes_freed.clone();
    let fail_tx_for_tasks = fail_tx.clone();
    let pb_clone = pb.clone();
    let delete_handle = tokio::spawn(async move {
        use futures::stream::{Stream, StreamExt, TryStreamExt};

        // HDD optimization: collect and sort by path for sequential access
        // SSD optimization: stream directly for high parallelism
        let stream: std::pin::Pin<Box<dyn Stream<Item = ScanResult> + Send>> =
            if config.storage_kind.should_sort() {
                // HDD: collect all, sort by path, then yield
                let mut results: Vec<ScanResult> = Vec::new();
                while let Some(result) = scan_rx.recv().await {
                    if crate::is_shutdown_requested() {
                        break;
                    }
                    results.push(result);
                }
                // Sort by path for sequential disk access
                results.sort_by(|a, b| a.path.cmp(&b.path));
                Box::pin(async_stream::stream! {
                    for result in results {
                        yield result;
                    }
                })
            } else {
                // SSD: stream directly
                Box::pin(async_stream::stream! {
                    while let Some(result) = scan_rx.recv().await {
                        // Check for shutdown request
                        if crate::is_shutdown_requested() {
                            info!("Shutdown requested, stopping deletion stream");
                            break;
                        }
                        yield result;
                    }
                })
            };

        stream
            .map(|result| {
                let deleted_tx = deleted_tx.clone();
                let fail_tx = fail_tx_for_tasks.clone();
                let trash_tx = trash_tx.clone();
                let pb = pb_clone.clone();
                let config = config.clone();
                let delete_count = delete_count.clone();
                let fail_count = fail_count.clone();
                let total_bytes = total_bytes.clone();

                async move {
                    if config.verbose {
                        pb.println(result.path.display().to_string());
                    }

                    let success = if !config.dry_run {
                        if result.is_dir {
                            // Safe directory deletion: only delete if directory is empty
                            // Never use remove_dir_all as it would ignore glob patterns
                            let is_empty = match fs::read_dir(&result.path).await {
                                Ok(mut entries) => entries
                                    .next_entry()
                                    .await
                                    .map(|e| e.is_none())
                                    .unwrap_or(false),
                                Err(_) => false,
                            };
                            if is_empty {
                                match fs::remove_dir(&result.path).await {
                                    Ok(_) => true,
                                    Err(_e) => {
                                        // Check if directory still exists - if not, it was deleted despite the error
                                        let mut still_exists = true;
                                        for _ in 0..3 {
                                            if fs::metadata(&result.path).await.is_err() {
                                                still_exists = false;
                                                break;
                                            }
                                            tokio::time::sleep(tokio::time::Duration::from_millis(
                                                10,
                                            ))
                                            .await;
                                        }
                                        if !still_exists {
                                            info!(
                                                "Directory deleted despite error: {}",
                                                result.path.display()
                                            );
                                            true
                                        } else {
                                            error!(
                                                "Failed to delete directory: {}",
                                                result.path.display()
                                            );
                                            false
                                        }
                                    }
                                }
                            } else {
                                // Directory not empty - skip it (files inside will be handled by their own scan entries)
                                debug!("Skipping non-empty directory: {}", result.path.display());
                                false
                            }
                        } else {
                            if config.use_trash {
                                // Queue for trash - actual success/failure counted by trash worker
                                let _ = trash_tx.send(result.path.clone()).await;
                                pb.inc(1);
                                return Ok::<(), ()>(());
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
                        }
                    } else {
                        true // Dry run: pretend everything succeeded
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
                                deleted_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .expect("System time went backwards")
                                    .as_secs(),
                            })
                            .await
                            .ok();
                    } else if !result.is_dir {
                        fail_count.fetch_add(1, Ordering::Relaxed);
                        fail_tx.send(result.path).await.ok();
                    }

                    pb.inc(1);
                    Ok::<(), ()>(())
                }
            })
            .buffer_unordered(config.parallelism)
            .try_collect::<Vec<_>>()
            .await
            .ok();
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
