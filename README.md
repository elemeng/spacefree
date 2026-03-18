# 🚀 spacefree

> 🚀 Ultra-fast, storage-aware file deletion CLI tool with trash support

[![Rust](https://img.shields.io/badge/rust-%23000000.svg?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org/) [![Crates.io](https://img.shields.io/crates/v/spacefree?style=for-the-badge)](https://crates.io/crates/spacefree) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg?style=for-the-badge)](https://opensource.org/licenses/MIT)

---

## ✨ Features

| Feature | Description |
|---------|-------------|
| 🚀 **Blazing Fast** | Async parallel scanning & deletion with adaptive storage optimization |
| 🧠 **Storage-Aware** | Auto-detects HDD/SSD and optimizes for sequential or parallel access |
| 🛡️ **Safe** | Trash mode, dry-run preview, symlink protection, root directory guards |
| 🎯 **Flexible** | Glob patterns, size filters, age filters, exclusion rules |
| 📁 **Batch Ready** | Accept directories, files, or path list files |
| 📝 **Logging** | Optional NDJSON log of all deleted items |
| 🧹 **Smart** | Auto-skip non-existent paths, deduplicates, graceful Ctrl+C handling |
| 👁️ **Verbose** | Optional detailed file listing for visibility |

### 🖥️ Platform Support

| Platform | Trash Support | Storage Detection | Status |
|----------|---------------|-------------------|--------|
| 🐧 Linux | ✅ Yes | ✅ HDD/SSD via /sys/block | ✅ Fully supported |
| 🍎 macOS | ✅ Yes | ✅ SSD via diskutil | ✅ Fully supported |
| 🪟 Windows | ✅ Yes | ✅ SSD via WMI/TRIM | ✅ Fully supported |

---

## 📦 Installation

### From crates.io

```bash
cargo install spacefree
```

Two binaries are installed:
- `spacefree` - full name
- `spa` - short alias

### From source

```bash
git clone https://github.com/elemeng/spacefree
cd spacefree
cargo install --path .
```

Or build and run directly:

```bash
cargo build --release
./target/release/spa --help
```

---

## 🎮 Quick Start

### Basic Usage

```bash
# 🗑️  Delete ALL files from directories (be careful!)
$ spa J12 J13 J14

# 👀 Preview before delete (dry run - recommended)
$ spa J12 --dry-run

# 🎯 Delete only specific file types
$ spa J12 -g "*.log"

# ♻️  Move to system trash (safer)
$ spa J12 --trash

# 📋 Log deleted items to file
$ spa J12 -l                    # Auto-named log (spacefree_0001.log)
$ spa J12 -l /path/to/log.json  # Custom log path
```

---

## 📋 Usage Examples

### Filter by File Size

```bash
# Only files >= 10 megabytes
$ spa J12 --min-size 10M

# Files between 10MB and 1GB
$ spa J12 --min-size 10M --max-size 1G

# Supported units: B (bytes), K/KB, M/MB, G/GB, T/TB
$ spa J12 --min-size 1G      # 1 gigabyte
$ spa J12 --min-size 512k    # 512 kilobytes
```

### Filter by File Age

```bash
# Files older than 7 days
$ spa J12 --min-age 7d

# Files newer than 1 hour
$ spa J12 --max-age 1h

# Files between 1 week and 1 month old
$ spa J12 --min-age 1w --max-age 1M

# Supported units: s/sec, m/min, h/hour, d/day, w/week, M/month, y/year
$ spa J12 --min-age 2w       # 2 weeks
$ spa J12 --min-age 3m       # 3 months
$ spa J12 --min-age 1y       # 1 year
```

### File Patterns (Glob)

By default, **all files** (`**/*`) are selected. Use `-g` to filter:

```bash
# Delete only .log files
$ spa J12 -g "*.log"

# Multiple patterns
$ spa J12 -g "**/*.{tmp,cache}"

# Exclude certain patterns
$ spa J12 -g "*.txt" --exclude "**/important.txt"
```

### Delete Directories

```bash
# Delete empty directories as well as files
$ spa J12 --dirs

# Directories are only deleted when empty (after files are removed)
```

### Safety Options

```bash
# Don't follow symbolic links (prevents deleting outside target)
$ spa J12 --no-follow-symlinks

# Delete root directory (requires both flags for safety)
$ spa /path --delete-root-dir -y
```

### Batch Processing from File

Create a `jobs.txt` file:

```
J12
J13, J14
J15
```

Then run:

```bash
spa --path-list-file jobs.txt
```

Or mix directories and path list files:

```bash
spa J12 --path-list-file jobs.txt J20
```

### Storage-Aware Optimization

```bash
# Auto-detect storage type and optimize (default)
$ spa J12
# Output: Storage: Ssd → parallelism: 16

# Force HDD-optimized sequential deletion
$ spa J12 -p 1

# Force high parallelism for SSD
$ spa J12 -p 32
```

### Parallel Workers

```bash
# Use 16 parallel workers (default: auto-detect based on storage)
$ spa J12 -p 16

# Sequential processing (best for HDDs)
$ spa J12 -p 1
```

### Skip Confirmation

```bash
# Auto-confirm deletion (use with caution!)
$ spa J12 --yes
```

### Verbose Mode

```bash
# Show all files to be deleted
$ spa J12 -v --dry-run
```

### Logging

```bash
# Auto-generate log filename (spacefree_0001.log, etc.)
$ spa J12 -l

# Specify custom log path
$ spa J12 -l /var/log/deletions.json

# Log format: NDJSON (one JSON object per line)
# {"path":"/path/to/file","is_dir":false,"deleted_at":1234567890}
```

---

## 🛠️ Command Reference

```
Usage: spa [OPTIONS] <PATHS>...

Arguments:
  <PATHS>...  Paths to scan - directories or files to delete

Options:
  -g, --glob <PATTERN>       Glob pattern for files [default: **/*]
      --exclude <PATTERN>    Glob pattern to exclude
      --min-size <SIZE>      Minimum file size (e.g., 10k, 5M, 1G) [default: 0]
      --max-size <SIZE>      Maximum file size (e.g., 10k, 5M, 1G)
      --min-age <AGE>        Minimum file age (e.g., 1d, 2w, 3m, 1y)
      --max-age <AGE>        Maximum file age (e.g., 1d, 2w, 3m, 1y)
      --trash                Move to system trash instead of permanent delete
      --dry-run              Preview what would be deleted
  -y, --yes                  Skip confirmation prompt
      --delete-root-dir      Allow deleting root directory (requires -y)
  -p, --parallelism <N>      Number of workers (0 = auto-detect) [default: 0]
  -v, --verbose              Show all files to be deleted
      --dirs                 Delete empty directories as well
      --no-follow-symlinks   Don't follow symbolic links
      --path-list-file <FILE>  File containing paths to process
  -l, --log [<PATH>]         Log deleted items (auto-named or specify path)
  -h, --help                 Print help
  -V, --version              Print version
```

---

## 🧠 Storage-Aware Optimization

spacefree automatically detects your storage type and optimizes deletion strategy:

| Storage | Parallelism | Strategy | Why |
|---------|-------------|----------|-----|
| **HDD** | 1 worker | Sorted by path, sequential access | Minimizes seek time |
| **SSD** | num_cpus × 4 | Streaming, high parallelism | Maximizes queue depth |
| **Unknown** | num_cpus × 2 | Balanced | Safe default |

### Detection Methods

- **Linux**: Reads `/sys/block/*/queue/rotational`
- **macOS**: Uses `diskutil info` to check "Solid State" property
- **Windows**: Queries WMI `Win32_DiskDrive` or checks TRIM support

---

## ⚠️ Safety Features

1. **Always use `--dry-run` first** to preview what will be deleted
2. **By default, ALL files are selected** - use `-g` to filter by pattern
3. **Use `--trash`** for safer deletion (can be recovered from system trash)
4. **Symlink protection** - Use `--no-follow-symlinks` to prevent traversing outside target
5. **Root directory guard** - Requires both `--delete-root-dir` and `-y` to delete `/`
6. **Graceful interruption** - Press Ctrl+C to stop safely after current operations
7. **Empty directory check** - Directories only deleted when truly empty

### Confirmation

When deleting without `--yes`, you'll be prompted:

```
Type exactly YES to continue:
```

---

## 🤝 Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

### Development Setup

```bash
# Clone the repository
git clone https://github.com/elemeng/spacefree
cd spacefree

# Run tests
cargo test

# Run with debug output
RUST_LOG=info cargo run --bin spa -- J12 --dry-run

# Build release binary
cargo build --release

# Check code style
cargo clippy

# Format code
cargo fmt
```

### Project Structure

```
src/
├── main.rs      # Entry point & CLI orchestration
├── cli.rs       # CLI parsing & argument definitions
├── config.rs    # DeleteConfig & ScanResult types
├── scan.rs      # Directory scanning & path collection
├── delete.rs    # Deletion pipeline & trash worker
├── storage.rs   # HDD/SSD detection & optimization
├── log.rs       # DeletedItem logging & LogMode
└── error.rs     # DeleterError type
```

### Bug Reports & Feature Requests

Please use [GitHub Issues](https://github.com/elemeng/spacefree/issues) to report bugs or request features.

When reporting bugs, please include:

- Operating system and version
- Rust version (`rustc --version`)
- Steps to reproduce
- Expected vs actual behavior

---

## 📄 License

MIT © [elemeng](https://github.com/elemeng/spacefree/blob/master/LICENSE)

---

<p align="center">Made with ☕ and 🦀</p>