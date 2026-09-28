# 设置与构建环境

[日本語](../setup.md) · [English](../en/setup.md) · **简体中文**

[README](../../README.zh-CN.md)介绍已发布版本的安装，[开发者指南（日语）](../developer-guide.md)介绍代码。本页区分运行要求与源码构建要求。

## 1. 运行不需要 Rust

使用 [GitHub Releases](https://github.com/automationjp/HyperDiskUsage/releases) 的 Windows / Linux 二进制文件、`.deb` 或 Scoop 时，不需要 Rust 工具链。Windows 二进制文件也不需要安装 Visual Studio 才能运行。

```powershell
scoop bucket add hyperdu https://github.com/automationjp/HyperDiskUsage
scoop install hyperdu
hyperdu --version
```

Linux 请根据环境选择 x86_64 glibc、x86_64 musl 或 aarch64 glibc。GUI 需要 Windows 桌面会话或 Linux X11 / Wayland 环境；headless server 请使用 CLI。macOS 不在当前 Release 和 CI 范围内，也不提供 GUI 发行版。

## 2. 声明的 Rust 版本不等于已验证的构建保证

| Crate | 当前 manifest 的 `rust-version` | 注意事项 |
|---|---:|---|
| `hyperdu-core` | 1.75 | 声明值，不是整个依赖图最低版本的证明 |
| `hyperdu`（CLI + MCP） | 1.88 | 包含 MCP，不应使用低于此版本的编译器构建 workspace |
| `hyperdu-gui` | 1.75 | **与依赖条件冲突，不能视为支持保证** |

GUI 使用的 egui 系列在 [0.32.0 manifest](https://github.com/emilk/egui/blob/0.32.0/Cargo.toml) 中要求 Rust 1.85。因此不能宣传 GUI 支持 Rust 1.75。这也不表示已经证明1.85或1.88足以构建所有解析后的依赖。最低版本声明与专用 CI 的协调仍待完成。

**开发请使用较新的 stable Rust。** 当前 CI 也使用 stable。

```bash
rustup toolchain install stable
rustup default stable
rustup component add rustfmt clippy
rustc --version
cargo --version
```

## 3. 各 OS 的源码构建准备

### Windows

建议使用 MSVC 工具链。除 Rust 外，需要 Visual Studio / Build Tools 的 C++ 构建工具、MSVC linker 和 Windows SDK。常用 target 为 `x86_64-pc-windows-msvc`。

```powershell
rustup show
rustc -vV
```

通过 MFT 验证真实卷与普通构建不同，需要 NTFS 卷根目录、管理员权限和受支持参数；不符合条件时回退到普通枚举。普通构建成功不能证明真实卷行为已经验证。

### Linux

Ubuntu / Debian 可安装基本 C 构建环境：

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config
```

GUI 还需要 `egui` / `eframe` / `winit` 使用的 X11 / Wayland 开发库。包名因发行版而异；出现 native library error 时添加对应的开发包。

打包和交叉编译与普通 CLI 构建不同。Release workflow 还使用 `musl-tools`、`gcc-aarch64-linux-gnu`、`mingw-w64`、`zip`、`jq`、`rpm` 等工具。

### macOS

存在 `getattrlistbulk` 实现，但当前 CI 不验证该平台。原生 CLI 性能和兼容性尚未验证，GUI 也不是 Release 目标。

## 4. 构建、运行与安装开发 checkout

```bash
git clone https://github.com/automationjp/HyperDiskUsage.git
cd HyperDiskUsage
cargo build --release --locked -p hyperdu
cargo test --locked -p hyperdu-core -p hyperdu
./target/release/hyperdu --version
./target/release/hyperdu . --top 20
```

Windows PowerShell 中改用 `.\target\release\hyperdu.exe`。

```bash
cargo build --release --locked -p hyperdu-gui
cargo run --release -p hyperdu-gui
cargo install --locked --path hyperdu
```

`--path` 安装当前开发 checkout；已发布版本请使用 README 的明确 `--version` 命令。

## 5. MCP

`hyperdu` 构建中包含 MCP，无需另装服务 crate。

```bash
hyperdu mcp
claude mcp add --transport stdio hyperdu -- hyperdu mcp
codex mcp add hyperdu -- hyperdu mcp
```

`hyperdu mcp` 是等待 stdio 协议输入的服务，不是交互式命令行提示符。请通过 MCP 客户端验证启动与连接。[Plugin / Skill / MCP](../../plugin/README.zh-CN.md)

## 6. 开发验证

```bash
cargo check --workspace --locked
cargo test --workspace --locked
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --locked --features hyperdu-core/prof-tracy
cargo check --workspace --locked --features hyperdu-core/prof-puffin
cargo test --workspace --locked --features hyperdu-core/rayon-par,hyperdu-core/rayon-inner,hyperdu-core/simd-prefetch
```

Tracy 和 Puffin 不能同时启用，`--all-features` 不是有效的统一验证方式。修改文档或分发流程时，请安装 PyYAML，并执行：

```bash
python3 scripts/lint/test_docs.py
python3 scripts/package/test_supply_chain.py
python3 scripts/package/test_registry.py
```

## 7. 性能与安装检查

性能比较使用 release build。针对本地 CPU 优化时记录条件：

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release --locked -p hyperdu
```

不要将本地 CPU 结果当作 generic 预构建二进制文件的性能。先通过[基准测试正确性检查](benchmarks.md)，记录 OS、文件系统、Rust 版本、profile、CPU、存储和命令。

```bash
hyperdu --version
hyperdu --help
hyperdu . --top 10
# 从开发 checkout 检查帮助：
cargo run -p hyperdu -- --help
```

[性能](performance.md) · [基准测试](benchmarks.md) · [架构](architecture.md) · [文档目录](README.md)
