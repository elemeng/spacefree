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
    let (gs, exclude) = build_globset(Some("*.txt"), &None).unwrap();
    assert!(gs.is_match("file.txt"));
    assert!(gs.is_match("test.txt"));
    assert!(!gs.is_match("file.md"));
    assert!(exclude.is_none());
}

#[test]
fn test_build_globset_with_exclude() {
    let (gs, exclude) = build_globset(Some("**/*.mrc"), &Some("**/*.txt".to_string())).unwrap();
    assert!(gs.is_match("data/file.mrc"));
    assert!(!gs.is_match("file.txt")); // exclude pattern is now separate
    assert!(exclude.is_some());
    assert!(exclude.unwrap().is_match("file.txt"));
}

#[test]
fn test_build_globset_invalid_pattern() {
    let result = build_globset(Some("[invalid"), &None);
    assert!(matches!(result, Err(DeleterError::Glob(_))));
}

#[test]
fn test_build_globset_invalid_exclude() {
    let result = build_globset(Some("*.txt"), &Some("[invalid".to_string()));
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
    assert_eq!(cli.glob, None);
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
    assert_eq!(cli.glob, Some("*.txt".to_string()));
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
    let (gs, exclude) = build_globset(Some("*.txt"), &None).unwrap();

    let (files, bytes, preview) = scan_only(vec![temp.path().to_path_buf()], gs, exclude, 0, 4)
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

    let (gs, exclude) = build_globset(Some("*.txt"), &None).unwrap();

    let (files, bytes, preview) = scan_only(vec![temp.path().to_path_buf()], gs, exclude, 0, 4)
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

    let (gs, exclude) = build_globset(Some("*.txt"), &None).unwrap();

    let (files, _bytes, _) = scan_only(
        vec![temp.path().to_path_buf()],
        gs,
        exclude,
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

    let (gs, exclude) = build_globset(Some("*.txt"), &None).unwrap();

    let (files, bytes, _) = scan_only(
        vec![temp1.path().to_path_buf(), temp2.path().to_path_buf()],
        gs,
        exclude,
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

    let (gs, exclude) = build_globset(Some("*.txt"), &None).unwrap();
    let pb = ProgressBar::hidden();

    let deleted = delete_streaming(
        vec![temp.path().to_path_buf()],
        gs,
        exclude,
        true, // dry_run
        false,
        4,
        0,
        false,
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

    let (gs, exclude) = build_globset(Some("*.txt"), &None).unwrap();
    let pb = ProgressBar::hidden();

    let deleted = delete_streaming(
        vec![temp.path().to_path_buf()],
        gs,
        exclude,
        false, // actual delete
        false,
        4,
        0,
        false,
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

    let (gs, exclude) = build_globset(Some("*.txt"), &None).unwrap();
    let pb = ProgressBar::hidden();

    let deleted = delete_streaming(
        vec![temp.path().to_path_buf()],
        gs,
        exclude,
        false,
        false,
        4,
        5, // min_size
        false,
        pb,
    )
    .await
    .unwrap();

    assert_eq!(deleted, 1); // only large.txt
    assert!(temp.path().join("small.txt").exists());
    assert!(!temp.path().join("large.txt").exists());
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

// ========== scan_only all files tests ==========
#[tokio::test]
async fn test_scan_only_returns_all_files() {
    let temp = TempDir::new().unwrap();

    // Create more than 10 files
    for i in 0..15 {
        fs::write(temp.path().join(format!("file{i}.txt")), "content")
            .await
            .unwrap();
    }

    let (gs, exclude) = build_globset(Some("*.txt"), &None).unwrap();

    let (files, _bytes, all_files) = scan_only(vec![temp.path().to_path_buf()], gs, exclude, 0, 4)
        .await
        .unwrap();

    // Should return all files, not limited to 10
    assert_eq!(files, 15);
    assert_eq!(all_files.len(), 15);
}

// ========== globset exclude pattern tests ==========
#[test]
fn test_build_globset_exclude_matches() {
    let (gs, exclude) = build_globset(Some("**/*.txt"), &Some("**/exclude*.txt".to_string())).unwrap();
    assert!(gs.is_match("file.txt"));
    assert!(gs.is_match("exclude_me.txt")); // still matches globset
    assert!(exclude.unwrap().is_match("exclude_me.txt")); // but excluded separately
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

    let (gs, exclude) = build_globset(Some("*.txt"), &None).unwrap();

    let (files, _bytes, _) = scan_only(vec![temp.path().to_path_buf()], gs, exclude, 0, 4)
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
        glob: Some("*.nonexistent".to_string()),
        exclude: None,
        min_size: 0,
        trash: false,
        dry_run: false,
        yes: true,
        parallelism: 4,
        verbose: false,
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
        glob: Some("*.txt".to_string()),
        exclude: None,
        min_size: 0,
        trash: false,
        dry_run: true,
        yes: false,
        parallelism: 4,
        verbose: false,
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
        glob: Some("*.txt".to_string()),
        exclude: None,
        min_size: 0,
        trash: false,
        dry_run: false,
        yes: true, // auto confirm
        parallelism: 4,
        verbose: false,
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
        glob: Some("*.*".to_string()),
        exclude: Some("*.log".to_string()),
        min_size: 0,
        trash: false,
        dry_run: true,
        yes: true,
        parallelism: 4,
        verbose: false,
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
        glob: Some("*.txt".to_string()),
        exclude: None,
        min_size: 10, // Only files >= 10 bytes
        trash: false,
        dry_run: true,
        yes: true,
        parallelism: 4,
        verbose: false,
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
        glob: Some("*.txt".to_string()),
        exclude: None,
        min_size: 0,
        trash: true, // trash mode
        dry_run: true,
        yes: true,
        parallelism: 4,
        verbose: false,
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
        glob: Some("*.txt".to_string()),
        exclude: None,
        min_size: 0,
        trash: false,
        dry_run: false,
        yes: true,
        parallelism: 4,
        verbose: false,
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

    let (gs, exclude) = build_globset(Some("**/*.txt"), &None).unwrap();

    let (files, bytes, _) = scan_only(vec![temp.path().to_path_buf()], gs, exclude, 0, 4)
        .await
        .unwrap();

    assert_eq!(files, 2);
    assert_eq!(bytes, 19); // "deep content" (12) + "shallow" (7) + newline
}

#[tokio::test]
async fn test_scan_only_large_parallelism() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("test.txt"), "x").await.unwrap();

    let (gs, exclude) = build_globset(Some("*.txt"), &None).unwrap();

    // Test with high parallelism value
    let (files, _, _) = scan_only(
        vec![temp.path().to_path_buf()],
        gs,
        exclude,
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
