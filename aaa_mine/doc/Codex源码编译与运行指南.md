# 从源码编译并运行 Codex

本文说明如何在 macOS 上编译当前 Codex 仓库，并与已经通过 npm、Homebrew 或官方安装器安装的 Codex 共存。

## 目标与原则

本仓库的主要实现是 Rust。源码编译后会得到一个本地开发版的 `codex` 可执行文件：

```text
/Users/shtexaizengbobo/dev/ai_agent/codex/codex-rs/target/debug/codex
```

不要把它复制到系统 PATH，也不要用它覆盖已有的官方安装。推荐约定：

| 命令 | 含义 |
| --- | --- |
| `codex` | PATH 中已经安装的官方版本。 |
| `codex-dev` | 当前仓库编译的开发版本。 |
| `cargo run ...` | 由 Cargo 编译并直接启动当前仓库的开发版本。 |

这样可以方便地比较官方版本和正在修改的源码版本。

## 1. 准备开发环境

### 安装 Apple 命令行工具

如果尚未安装，执行：

```bash
xcode-select --install
```

### 安装 Rust

如果执行 `rustup` 显示 `command not found`，说明 Rust 工具链尚未安装。执行 Rust 官方安装命令：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

安装完成后，在当前终端加载 Rust 的环境变量：

```bash
source "$HOME/.cargo/env"
```

确认安装成功：

```bash
rustup --version
rustc --version
cargo --version
```

安装本仓库日常开发使用的 Rust 组件和辅助工具：

```bash
rustup component add rustfmt clippy
cargo install --locked just
cargo install --locked dotslash
cargo install --locked cargo-nextest
```

> 只想先完成编译时，`cargo` 是必需项；`just`、`dotslash` 和 `cargo-nextest` 在执行仓库提供的格式化、构建辅助和测试命令时会用到。

## 2. 编译开发版 Codex

进入 Rust workspace：

```bash
cd /Users/shtexaizengbobo/dev/ai_agent/codex/codex-rs
```

构建 CLI 主程序和 Code Mode host：

```bash
cargo clean

cargo build -p codex-cli --bin codex -p codex-code-mode-host --bin codex-code-mode-host

cargo build -p codex-cli --bin codex \
  -p codex-code-mode-host --bin codex-code-mode-host
```

编译产物位于：

```bash
./target/debug/codex
./target/debug/codex-code-mode-host
```

验证开发版可以启动：

```bash
./target/debug/codex --version
./target/debug/codex --help
./target/debug/codex-code-mode-host --help
```

首次构建会下载并编译较多 Rust 依赖，因此耗时和磁盘占用都可能明显增加；后续增量构建通常会快得多。

### Code Mode host 缺失

不要只执行 `cargo build -p codex-cli --bin codex`。该命令只会生成 CLI，不会生成与 CLI 同目录的 `codex-code-mode-host`。如果启用了 Code Mode，首次执行相关任务时会出现类似错误：

```text
Code Mode is unavailable because failed to spawn code-mode host ...
codex-code-mode-host: host executable was not found
```

按本节开头的“构建 CLI 主程序和 Code Mode host”命令重新编译即可。确认下面两个文件都存在后，再启动 `./target/debug/codex`：

```bash
ls -lh ./target/debug/codex ./target/debug/codex-code-mode-host
```

在 macOS 上，`codex-code-mode-host` 会依赖 V8。若下载 V8 预编译库失败（例如 HTTP 404），先安装完整 Xcode，并让系统选择它：

```bash
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
sudo xcodebuild -license accept
brew install llvm
```

然后使用源码构建 V8 与 host：

```bash
V8_FROM_SOURCE=1 PYTHON=/usr/bin/python3 \
LIBCLANG_PATH="$(brew --prefix llvm)/lib" \
cargo build -p codex-cli --bin codex \
  -p codex-code-mode-host --bin codex-code-mode-host
```

这一步会编译 V8，首次执行可能需要 30 分钟或更久，并占用数 GB 磁盘空间。`LIBCLANG_PATH` 必须指向 Homebrew LLVM 的 `lib` 目录；否则 bindgen 可能会误用 Xcode 自带的旧版 libclang，并报出 libc++ 模板或 `__builtin_popcountg` 相关错误。

## 3. 运行开发版

### 方式 A：显式运行编译产物（推荐）

```bash
cd /Users/shtexaizengbobo/dev/ai_agent/codex/codex-rs
./target/debug/codex
```

传入一个初始任务：

```bash
./target/debug/codex "explain this codebase to me"
```

由于命令带有 `./target/debug/`，它一定是本仓库编译出的开发版本，不会调用官方安装版本。

### 方式 B：使用 Cargo 启动

Cargo 会确保启动当前 workspace 中的目标程序：

```bash
cd /Users/shtexaizengbobo/dev/ai_agent/codex/codex-rs
cargo run -p codex-cli --bin codex -- --help
cargo run -p codex-cli --bin codex
```

`--` 左侧的参数交给 Cargo，右侧的参数交给 Codex。例如：

```bash
cargo run -p codex-cli --bin codex -- "explain this codebase to me"
```

## 4. 与官方安装版本共存

### 先确认官方命令的位置

在未定义 `codex` 开发版别名的前提下，执行：

```bash
whence -p codex
type -a codex
```

常见输出可能是：

```text
/opt/homebrew/bin/codex
```

或 npm 全局安装目录下的路径。这个路径是 shell 默认在执行 `codex` 时找到的官方版本入口。

### 推荐：只为开发版增加别名

编辑 zsh 配置文件：

```bash
nano ~/.zshrc
```

增加下面这一行：

```bash
alias codex-dev='/Users/shtexaizengbobo/dev/ai_agent/codex/codex-rs/target/debug/codex'
```

保存后使配置立即生效：

```bash
source ~/.zshrc
```

此后：

```bash
codex --version       # 官方安装版本
codex-dev --version   # 当前仓库编译的开发版本
```

当源码重新编译时，`codex-dev` 会自动使用同一路径上的最新编译产物，无需修改别名。

### 可选：为官方版本也增加显式别名

只有在需要经常并排对比时才建议这样做。先用 `whence -p codex` 得到实际路径，再把该路径原样写入 `~/.zshrc`。例如：

```bash
alias codex-official='/opt/homebrew/bin/codex'
alias codex-dev='/Users/shtexaizengbobo/dev/ai_agent/codex/codex-rs/target/debug/codex'
```

其中 `/opt/homebrew/bin/codex` 只是示例，必须替换为自己终端中 `whence -p codex` 的输出；不要保留任何“实际查到的路径”之类的占位文字。

加载配置后可并排检查：

```bash
codex-official --version
codex-dev --version
```

## 5. 常见使用方式

### 交互模式

```bash
codex-dev
```

启动后在终端直接输入编程任务。首次真正调用代理时，需要登录 ChatGPT 账户或配置可用的 API 凭据。

### 带初始任务启动

```bash
codex-dev "review the current changes"
```

### 非交互模式

```bash
codex-dev exec "explain the architecture of this project"
```

可先通过帮助命令查看当前源码版本支持的完整参数：

```bash
codex-dev --help
codex-dev exec --help
```

### 更新并重新编译

每次修改 Rust 源码后，在 `codex-rs` 目录执行：

```bash
cargo clean
cargo build -p codex-cli --bin codex
```

然后重新运行 `codex-dev` 即可。

## 6. 开发时的格式化与测试

本项目将常用开发命令封装在根目录的 `justfile` 中。修改 Rust 代码后，可在仓库根目录执行：

```bash
cd /Users/shtexaizengbobo/dev/ai_agent/codex
just fmt
```

针对 CLI 的测试使用：

```bash
just test -p codex-cli
```

不要直接用 `cargo test` 替代仓库约定的 `just test`。完整测试套件耗时较长，通常只在需要进行全仓验证时运行。

## 7. 排错

| 现象 | 处理方式 |
| --- | --- |
| `rustup: command not found` | 安装 Rust，并执行 `source "$HOME/.cargo/env"` 后重新打开或加载终端环境。 |
| `cargo` 找不到 | 与上项相同；确认 `cargo --version` 能正常运行。 |
| 编译提示缺少 C/C++ 工具 | 执行 `xcode-select --install`。 |
| 不确定当前运行哪个版本 | 分别运行 `codex --version`、`codex-dev --version`，并用 `type -a codex` 检查 PATH。 |
| `codex-dev: command not found` | 确认已编译成功、别名已写入 `~/.zshrc`，并执行 `source ~/.zshrc`。 |
| 首次编译很慢 | Rust 正在下载和编译依赖；等待完成即可，之后通常是增量编译。 |

## 总结

保留 `codex` 指向官方安装版本，并使用 `./target/debug/codex`、`cargo run` 或 `codex-dev` 启动源码开发版本，是最清晰且不容易误操作的方式。
