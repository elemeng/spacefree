# spacefree

⚠️ high-performance file deletion cli tool with trash support.

## Features

- **Fast**: Parallel scanning and deletion
- **Safe**: Move to system trash or dry-run mode
- **Flexible**: Support for glob patterns and size filters
- **Convenient**: Accept directories or files containing paths

## Installation

```bash
cargo install spacefree
```

## Usage

```bash
# Delete files from job directories
spacefree J12 J13 J14

# Dry run (preview only)
spacefree J12 --dry-run

# Move to trash instead of permanent delete
spacefree J12 --trash

# Filter by file size
spacefree J12 --min-size 10M

# Custom glob pattern
spacefree J12 -g "*.log"
```

## Alias

The binary `spa` is provided as a shorthand alias for `spacefree`:

```bash
spa J12 --dry-run
```

## License

MIT
