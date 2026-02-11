# 🚀 spacefree

> ⚡ High-performance file deletion tool with trash support

[![Rust](https://img.shields.io/badge/rust-%23000000.svg?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org/) [![Crates.io](https://img.shields.io/crates/v/spacefree?style=for-the-badge)](https://crates.io/crates/spacefree) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg?style=for-the-badge)](https://opensource.org/licenses/MIT)

---

## ✨ Features

| Feature | Description |
|---------|-------------|
| 🚀 **Blazing Fast** | Parallel scanning & deletion with configurable workers |
| 🛡️ **Safe** | Optional trash mode, dry-run preview, flexible confirmation |
| 🎯 **Flexible** | Glob patterns, size filters, exclusion rules |
| 📁 **Batch Ready** | Accept directories or path list files (CSV/TXT) |
| 🧹 **Smart** | Automatically skips non-existent paths, deduplicates |

### 🖥️ Platform Support

| Platform | Trash Support | Status |
|----------|---------------|--------|
| 🐧 Linux | ✅ Yes | GTK, KDE, XDG compatible |
| 🍎 macOS | ✅ Yes | Native Finder trash |
| 🪟 Windows | ✅ Yes | Recycle Bin |

---

## 📦 Installation

### From crates.io

```bash
cargo install spacefree
```

### From source

```bash
git clone https://github.com/elemeng/spacefree
cd spacefree
cargo install --path .
```

---

## 🎮 Quick Start

### Basic Usage

```bash
# 🗑️  Delete ALL files from directories (be careful!)
$ spacefree J12 J13 J14

# 👀 Preview before delete (dry run - recommended)
$ spacefree J12 --dry-run

# 🎯 Delete only specific file types
$ spacefree J12 -g "*.log"

# ♻️  Move to system trash (safer)
$ spacefree J12 --trash
```

### 🏷️  Shorthand Alias

The binary `spa` is provided as a convenient alias:

```bash
spa J12 --dry-run
```

---

## 📋 Usage Examples

### Filter by File Size

```bash
# Only files >= 10 megabytes
$ spacefree J12 --min-size 10M

# Supported units: B (bytes), K/KB, M/MB, G/GB, T/TB
$ spacefree J12 --min-size 1G      # 1 gigabyte
$ spacefree J12 --min-size 512k    # 512 kilobytes
```

### File Patterns (Glob)

By default, **all files** are selected. Use `-g` to filter:

```bash
# Delete only .log files
$ spacefree J12 -g "*.log"

# Multiple patterns
$ spacefree J12 -g "**/*.{tmp,cache}"

# Exclude certain patterns
$ spacefree J12 -g "*.txt" --exclude "**/important.txt"
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
spacefree jobs.txt
```

Or mix directories and files:

```bash
spacefree J12 jobs.txt J20
```

### Skip Confirmation

```bash
# Auto-confirm deletion (use with caution!)
$ spacefree J12 --yes
```

### Parallel Workers

```bash
# Use 16 parallel workers (default: num_cpus * 4)
$ spacefree J12 -p 16
```

---

## 🛠️ Command Reference

```
Usage: spacefree [OPTIONS] <PATHS>...

Arguments:
  <PATHS>...  Job directories or path list files

Options:
  -g, --glob <PATTERN>     Glob pattern for files [default: **/*.mrc]
      --exclude <PATTERN>  Glob pattern to exclude
      --min-size <SIZE>    Minimum file size (e.g., 10M, 1G) [default: 0]
      --trash              Move to system trash instead of permanent delete
      --dry-run            Preview what would be deleted
  -y, --yes                Skip confirmation prompt
  -p, --parallelism <N>    Number of parallel workers
  -h, --help               Print help
  -V, --version            Print version
```

---

## ⚠️ Safety First

1. **Always use `--dry-run` first** to preview what will be deleted
2. **By default, ALL files are selected** - use `-g` to filter by pattern
3. **Use `--trash`** for safer deletion (can be recovered from system trash)
4. **By default, deletion is PERMANENT** - files are not recoverable
5. **Double-check your paths** before running without `--dry-run`

### Confirmation

When deleting without `--yes`, you'll be prompted:

```
Type YES to continue:
```

Accepted responses: `YES`, `Yes`, `Y`, `y` (case insensitive)

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
cargo run --bin spacefree -- J12 --dry-run

# Check code style
cargo clippy

# Format code
cargo fmt
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
