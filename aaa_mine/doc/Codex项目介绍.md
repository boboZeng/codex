# Codex 项目介绍

## 一句话定位

这是 OpenAI 开源的 **Codex 本地编程代理**。它不是训练或托管模型的仓库，而是一套在开发者电脑上运行的代理客户端和运行时：接收自然语言任务，调用远端模型进行推理，并在受控权限内读取代码、执行命令、修改文件、运行测试，以及连接外部工具。

项目根目录的 `README.md` 将其定位为“运行在本机上的 coding agent”。

## 它解决什么问题

传统的聊天式模型只能给出代码建议；Codex 将模型接入真实的工程环境，让它能够完成一个连续的开发任务：

1. 理解用户需求和仓库中的项目说明（如 `AGENTS.md`）。
2. 检索文件、阅读代码，建立与当前任务相关的上下文。
3. 请求模型生成下一步操作。
4. 在沙箱和审批策略的约束下执行命令、编辑文件或调用工具。
5. 读取执行结果并继续迭代，直到任务完成或需要用户决策。

因此，它更像“可操作本地开发环境的代理运行时”，而不只是一个命令行聊天程序。

## 整体架构

```text
用户 / IDE / SDK
        |
        v
CLI、终端 TUI、App Server
        |
        v
Codex 核心代理
  - 会话与上下文
  - 模型请求与流式响应
  - 工具调用编排
  - 审批与权限决策
        |
        +---------------------+-------------------+
        |                     |                   |
        v                     v                   v
沙箱命令与文件执行        MCP / 插件 / Skills      Git、文件系统与外部服务
```

模型推理服务位于仓库之外；本仓库负责客户端体验、代理编排、本机执行、安全隔离，以及扩展接口。

## 核心目录与职责

| 路径 | 职责 |
| --- | --- |
| `codex-rs/` | 主体代码，采用 Rust workspace 组织。 |
| `codex-rs/cli/` | `codex` 命令行程序的入口和子命令分发。 |
| `codex-rs/tui/` | 终端交互界面，负责渲染对话和执行过程。 |
| `codex-rs/core/` | 代理的核心业务逻辑：上下文、会话、模型调用、工具调用、配置和安全策略。 |
| `codex-rs/protocol/` | 核心逻辑、TUI 和服务端之间共用的协议类型。 |
| `codex-rs/app-server/` | 面向 VS Code、桌面端等客户端的 JSON-RPC 服务。 |
| `codex-rs/exec-server/` | 可独立部署的命令和文件执行服务，支持远程执行环境。 |
| `codex-rs/sandboxing/`、`linux-sandbox/`、`windows-sandbox-rs/` | 跨平台沙箱与权限隔离实现。 |
| `codex-rs/codex-mcp/`、`mcp-server/` | MCP（Model Context Protocol）客户端和服务端能力。 |
| `codex-rs/plugin/`、`skills/` | 插件和 Skills 的发现、加载与运行支持。 |
| `sdk/typescript/`、`sdk/python/` | 用 TypeScript 或 Python 程序化使用 Codex 的 SDK。 |
| `codex-cli/` | npm 发布包装，提供 `@openai/codex` 命令；核心实现仍主要在 Rust。 |
| `docs/` | CLI 配置、认证、沙箱、Skills 与非交互模式等文档。 |

## 三种主要使用入口

### 1. 命令行与终端界面

默认运行 `codex` 会进入终端交互体验；`codex exec` 用于非交互式调用。CLI 还包含代码审查、登录、会话恢复、MCP 管理、插件管理、沙箱诊断等子命令。

### 2. IDE 与桌面端

`codex app-server` 是 IDE 或桌面客户端连接 Codex 的服务层。它通过双向 JSON-RPC 流式传输事件，客户端据此显示模型输出、命令执行、文件变更和审批请求。

App Server 将一次代理交互建模为：

- **Thread**：一段持续的会话。
- **Turn**：用户的一次输入到代理完成回复的过程。
- **Item**：Turn 内的具体事件，例如用户消息、模型消息、推理、命令执行或文件编辑。

### 3. SDK

项目提供 TypeScript 与 Python SDK，供其他应用将 Codex 作为编程代理能力嵌入自己的工作流。

## 关键能力

- **上下文管理**：组合用户输入、会话历史、项目指令、环境信息和工具结果，再发送给模型。
- **工具执行**：执行 shell 命令、读写文件、应用补丁、处理 Git 信息等。
- **安全控制**：通过沙箱、工作区写入范围、网络策略和用户审批来限制副作用。
- **可扩展性**：通过 MCP 接入第三方工具；通过插件和 Skills 提供可复用的领域工作流与说明。
- **会话持久化**：支持恢复、归档、删除或分叉历史会话。
- **多终端支持**：同一套核心运行时可被 CLI、TUI、IDE、桌面端和 SDK 复用。

## 技术栈与工程形态

- 主实现语言：Rust（`codex-rs` 是一个大型 Cargo workspace）。
- 终端 UI：Rust TUI。
- 对外进程/IDE 协议：JSON-RPC，并包含 MCP 集成。
- 辅助发布与工具：Node.js/pnpm、Python，以及 Bazel 构建与跨平台 CI。
- 许可证：Apache-2.0。

根目录的 `justfile` 封装了常用开发命令；常见开发入口是进入 `codex-rs` 后执行 `cargo run --bin codex -- <prompt>`，或在仓库根目录运行 `just codex <prompt>`。

## 建议的阅读顺序

1. `README.md`：产品定位、安装与基本使用方式。
2. `docs/install.md`：本地构建、运行与测试流程。
3. `codex-rs/cli/src/main.rs`：`codex` 命令行的子命令和入口分发。
4. `codex-rs/core/README.md` 与 `codex-rs/core/src/`：代理会话、上下文、模型和工具编排。
5. `codex-rs/app-server/README.md`：IDE/桌面端如何驱动代理。
6. `codex-rs/exec-server/README.md`、沙箱相关 crate：命令执行与安全边界。
7. MCP、插件、Skills 模块：理解项目如何扩展外部能力。

## 总结

Codex 是一套将大模型能力、安全执行环境和多种客户端界面组合在一起的本地编程代理平台。它的价值不在于训练模型，而在于把模型的建议转化为可审计、可控制、可扩展的工程操作。
