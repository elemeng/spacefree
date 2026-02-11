# spacefree 项目上下文

## 项目概述

**spacefree** 是一个高性能的文件删除工具，支持系统回收站功能。这是一个用 Rust 编写的命令行工具，专注于快速、安全地批量删除文件。

### 主要技术栈
- **语言**: Rust (edition 2024, 最低版本 1.85)
- **运行时**: Tokio 异步运行时 (多线程)
- **CLI 解析**: clap (derive feature)
- **核心依赖**:
  - `futures` - 异步流处理
  - `globset` - Glob 模式匹配
  - `indicatif` - 进度条显示
  - `trash` - 跨平台回收站支持
  - `walkdir` - 目录遍历
  - `thiserror` - 错误处理

### 核心功能
- 并行扫描和删除（可配置工作线程数）
- Glob 模式文件过滤
- 文件大小过滤
- 系统回收站支持（可恢复删除）
- Dry-run 预览模式
- 从文件批量读取路径列表
- 跨平台支持（Linux/macOS/Windows）

## 项目结构

```
spacefree/
├── src/
│   ├── main.rs    # 主程序入口，包含 CLI 定义和核心逻辑
│   └── test.rs    # 测试模块（通过 mod test; 在 main.rs 中引入）
├── Cargo.toml     # 项目配置和依赖管理
├── Cargo.lock     # 依赖版本锁定
├── README.md      # 用户文档
└── LICENSE        # MIT 许可证
```

## 构建和运行

### 构建命令
```bash
# Debug 构建
cargo build

# Release 构建（优化）
cargo build --release

# 安装到本地（生成 spacefree 和 spa 二进制）
cargo install --path .
```

### 运行命令
```bash
# 直接运行
cargo run --bin spacefree -- <args>

# 或使用简短别名
cargo run --bin spa -- <args>

# 使用安装的版本
spacefree <args>
spa <args>
```

### 测试命令
```bash
# 运行所有测试
cargo test

# 运行特定测试
cargo test test_parse_size
cargo test test_format_size

# 显示测试输出
cargo test -- --nocapture

# 运行测试并显示详细信息
cargo test -- --show-output
```

### 代码质量检查
```bash
# Clippy 代码检查
cargo clippy

# 格式化代码
cargo fmt

# 格式化检查
cargo fmt -- --check
```

## 开发约定

### 代码风格
- 使用 Rust 2024 edition 特性
- 遵循标准 Rust 命名约定：
  - 类型/结构体：PascalCase（如 `DeleterError`, `Cli`）
  - 函数/变量：snake_case（如 `format_size`, `parse_size`）
  - 常量：SCREAMING_SNAKE_CASE（如 `UNITS`）
- 使用 `clap` derive 宏进行 CLI 解析
- 错误处理使用 `thiserror` crate

### 架构模式
- **异步优先**: 使用 Tokio 运行时和异步 API
- **流式处理**: 使用 `futures::Stream` 进行并发处理
- **两阶段执行**: 先扫描 (`scan_only`)，后删除 (`delete_streaming`)
- **并行执行**: 使用 `for_each_concurrent` 进行并行删除

### 测试规范
- 测试代码位于 `src/test.rs`
- 使用 `#[cfg(test)]` 模块和 `#[tokio::test]` 进行异步测试
- 使用 `tempfile` crate 创建临时测试目录
- 测试覆盖所有核心功能单元

### CLI 接口
```bash
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
  -v, --verbose            Show all files to be deleted
  -h, --help               Print help
  -V, --version            Print version
```

### 关键函数说明

**src/main.rs:345** - `format_size(size: u64) -> String`
格式化文件大小为人类可读格式

**src/main.rs:358** - `parse_size(s: &str) -> Result<u64, String>`
解析大小字符串（支持 B/KB/MB/GB/TB 单位）

**src/main.rs:384** - `build_globset(include, exclude) -> Result<GlobSet, DeleterError>`
构建 Glob 模式匹配器

**src/main.rs:398** - `scan_only(...) -> Result<(u64, u64, Vec<PathBuf>), DeleterError>`
扫描目录，返回匹配文件数、总字节数和预览列表

**src/main.rs:430** - `delete_streaming(...) -> Result<u64, DeleterError>`
流式删除文件（支持 dry-run 和 trash 模式）

**src/main.rs:469** - `confirm(...) -> Result<(), DeleterError>`
用户确认提示

**src/main.rs:489** - `parse_paths_from_content(content: &str) -> Vec<PathBuf>`
从文件内容解析路径（支持逗号、空格、换行分隔）

**src/main.rs:505** - `collect_paths(input_paths) -> Result<Vec<PathBuf>, DeleterError>`
收集并验证所有路径

**src/main.rs:541** - `run(cli: Cli) -> Result<(), DeleterError>`
主运行逻辑

## 常见开发任务

### 添加新的命令行选项
在 `Cli` 结构体中添加新字段，使用 `#[arg(...)]` 属性配置

### 修改文件过滤逻辑
修改 `scan_only` 函数中的过滤条件

### 添加新的测试
在 `src/test.rs` 中添加测试函数，使用 `#[test]` 或 `#[tokio::test]`

### 发布新版本
1. 更新 `Cargo.toml` 中的 `version` 字段
2. 运行 `cargo test` 确保所有测试通过
3. 运行 `cargo clippy` 和 `cargo fmt` 检查代码质量
4. 提交更改并打 tag