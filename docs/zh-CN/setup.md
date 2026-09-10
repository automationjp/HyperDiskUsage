# 设置与构建环境

[日本語](../setup.md) | [English](../en/setup.md) | [简体中文](../zh-CN/setup.md)

本文将**仅运行 HyperDU**与从 Rust 编译和开发 HyperDU 的要求分开说明。

## 1. 仅运行

如果使用预编译二进制文件、Scoop 或 `.deb`，**不需要 Rust 工具链**。

### Windows

可以使用 GitHub Release 中的 Windows x86_64 二进制文件，也可以使用 Scoop。

```powershell
scoop bucket add hyperdu https://github.com/automationjp/HyperDiskUsage
scoop install hyperdu
hyperdu --version
```

直接获取二进制文件同样不需要安装 Rust 或 Visual Studio。

### Linux

发布物包含 glibc、musl 和 aarch64 构建版本。请选择与环境匹配的版本。

- x86_64 + glibc：普通 Ubuntu / Debian / Fedora 系统的候选版本
- x86_64 + musl：需要使用静态构建时
- aarch64 + glibc：ARM64 Linux

使用 `.deb` 时同样不需要 Rust。

### GUI 运行时

`hyperdu-gui` 需要桌面图形栈。

- Windows：普通桌面会话
- Linux：X11 或 Wayland 桌面环境
- macOS：目前 GUI 不属于发布目标

## 2. 从 Rust 构建 CLI

### Rust 版本

各 crate 的最低 Rust 版本如下。

| Crate | Minimum Rust |
|---|---:|
| `hyperdu-core` | 1.75+ |
| `hyperdu`（CLI + MCP） | 1.88+ |
| `hyperdu-gui` | 1.75+ |

**一次构建或测试整个 workspace 需要 Rust 1.88+，因为其中包含 CLI 的 MCP 实现。**

开发环境建议使用 stable toolchain。

```bash
rustup toolchain install stable
rustup default stable
rustup component add rustfmt clippy
rustc --version
cargo --version
```

### 克隆

```bash
git clone https://github.com/automationjp/HyperDiskUsage.git
cd HyperDiskUsage
```

### 仅 CLI

```bash
cargo build --release -p hyperdu
```

运行：

```bash
./target/release/hyperdu --version
./target/release/hyperdu . --top 20
```

Windows PowerShell：

```powershell
.\target\release\hyperdu.exe --version
.\target\release\hyperdu.exe . --top 20
```

## 3. 各操作系统的编译环境

### Windows — 推荐的开发路径

推荐使用 **MSVC toolchain**。

所需组件：

1. Rust stable（`rustup`）
2. Visual Studio 2022 或 Visual Studio Build Tools
3. C++ build tools / MSVC linker
4. Windows SDK

确认 Rust target：

```powershell
rustup show
rustc -vV
```

使用常规 rustup Windows installer 选择 MSVC target 时，target 的形式如下：

```text
x86_64-pc-windows-msvc
```

构建：

```powershell
cargo build --release -p hyperdu
cargo test -p hyperdu-core -p hyperdu
```

使用 `--mft` 进行真实 volume 验证与普通构建不同。它需要 NTFS volume root 和管理员权限；不满足条件时，HyperDU 会回退到普通 directory enumeration。

### Linux — CLI / core

除 Rust 外，还需要能够编译 native dependency 的基本 C build environment。

Ubuntu / Debian 示例：

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config
```

然后：

```bash
cargo build --release -p hyperdu
cargo test -p hyperdu-core -p hyperdu
```

进行 release packaging 或 cross build 时需要额外的 toolchain。CI 的 release workflow 还会根据用途使用 `musl-tools`、`gcc-aarch64-linux-gnu`、`mingw-w64`、`zip`、`jq`、`rpm` 等工具。

### Linux — GUI

除 CLI/core 的 build environment 外，还需要 `egui` / `eframe` / `winit` 使用的 **X11 或 Wayland development libraries**。

不同发行版的 package 名称不同。如果 GUI build 报告 native library error，请添加该发行版对应的 X11 / Wayland development package。

```bash
cargo build --release -p hyperdu-gui
cargo run --release -p hyperdu-gui
```

在 headless server 上，建议使用 CLI 而不是 GUI。

### macOS

CLI core 提供 `getattrlistbulk` fast path，但目前尚未验证 macOS CLI 在真实设备上的 performance / compatibility。

目前 GUI 不包含在 release target 中。

## 4. 构建 MCP server

由于 `rmcp` 的要求，`hyperdu mcp` 需要 **Rust 1.88+**。

```bash
rustup toolchain install stable
cargo build --release -p hyperdu
cargo install --path hyperdu
hyperdu mcp
```

`hyperdu mcp` 不是普通的交互式 CLI 命令，而是**通过 stdio 接受 MCP client 连接的 server**。直接启动时，它会等待 protocol input。

注册示例：

```bash
claude mcp add --transport stdio hyperdu -- hyperdu mcp
codex mcp add hyperdu -- hyperdu mcp
```

请从所使用的 MCP client 侧确认 server 能够启动并建立连接，以验证其工作状态。

## 5. 检查开发环境

修改 workspace 时进行以下基本检查。

```bash
cargo check --workspace
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

整个 workspace 请使用 Rust 1.88+。

### 可选 feature 配置

`profiling` backend 不会同时启用 Tracy 和 Puffin。`--all-features` 不是有效的验证方法。

分别检查需要的 feature：

```bash
cargo check --workspace --features hyperdu-core/prof-tracy
cargo check --workspace --features hyperdu-core/prof-puffin
```

检查 parallel configuration：

```bash
cargo test --workspace \
  --features hyperdu-core/rayon-par,hyperdu-core/rayon-inner,hyperdu-core/simd-prefetch
```

## 6. Performance build

普通比较必须使用 release build。

```bash
cargo build --release -p hyperdu
```

只有在进行针对本地 CPU 的比较时才使用 native optimization，并记录条件。

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release -p hyperdu
```

不要将 `target-cpu=native` 的结果与 generic prebuilt binary 的性能值混为一谈。

发布性能值前，请按照 [Benchmark plan](benchmarks.md) 中的环境记录与 correctness gate 执行。

## 7. Installation check

完成设置后至少执行以下检查：

```bash
hyperdu --version
hyperdu --help
hyperdu . --top 10
```

在 development checkout 中：

```bash
cargo run -p hyperdu -- --help
```

如果问题可能与 performance path 有关，请同时记录 OS、filesystem、Rust version、build profile、CPU、storage 和执行 command。

## 相关文档

- [Performance design](performance.md)
- [Benchmark plan](benchmarks.md)
- [Architecture](architecture.md)
- [Documentation index](README.md)
