# 后续计划：Agent core 剥离为 `so-lite-agent` crate

> 状态：M1-M4 已落地（2026-08-11），M5 待办。决策记录见 [ADR-0037](../adr/0037-so-lite-agent-crate-extraction.md)。

## 目标

让第三方 `cargo add so-lite-agent` 后即可上手开发自己的 Agent——**开箱即用是首要目标**：模型 Provider、Agent loop、工具注册、会话、通用 RPC 全部随 crate 提供；内核插件与用户插件由使用方自行编写（本仓库即参考实现）。

## 定位（参考 Pi Agent）

参考 [earendil-works/pi](https://github.com/earendil-works/pi) 的分层：

| Pi 包 | 职责 | so-lite-agent 对应 |
|---|---|---|
| `pi-ai` | 统一 LLM Provider 抽象、模型注册表、内置 OpenAI/Anthropic 适配器、`registerProvider()` | 内置 `model` 模块：`ModelService` 抽象 + OpenAI 兼容/Anthropic 兼容/自定义端点适配器 + `register_provider()` |
| `pi-agent-core` | Agent loop、AgentTool（JSON schema）、AgentEvent 事件流、SessionStore、compaction | 内置 `agent` 模块：loop / registry / dispatch / session / message / events / audit |
| `pi-coding-agent` | 领域层：会话树、内置编码工具 | 不进 crate——由使用方按业务写内核插件/用户插件 |
| TUI / RPC | JSON-RPC 2.0 无头嵌入 | 内置通用 RPC：通用 `Method` 子集 + `custom` 兜底 + `RpcExtension` |

## 仓库形态

- **新独立仓库承载 crate** `so-lite-agent`（单 crate，`cargo add` 即用；Pi 式分层体现在模块边界，不拆多 crate）。
- M2 已在本仓库创建本地独立 crate 目录 `so-lite-agent/` 作为脚手架（不参与本仓库 build.rs 插件发现）；正式迁到独立仓库后，本目录可作为源。
- mistake-agent 本仓库保持现状，新仓库发布后（或开发期经 path/git 依赖）作为消费方与参考实现。
- 插件开发手册与参考模板（`docs/plugin-dev/`）最终随新仓库走——使用方写内核/用户插件时以它们为教材。

## crate 内容

**进 crate（通用运行时）**：

- Agent core：`agent loop`（loop_mod 通用化）、工具注册与调度（registry/dispatch）、会话生命周期（session 通用化）、消息树（message）、事件流（events）、审计（audit）、契约（contract/context）。
- 模型 Provider 层：`ModelService` 抽象 + 内置适配器（OpenAI 兼容端点覆盖 DeepSeek；Anthropic 兼容；自定义 URL）+ `register_provider()`；流式事件归一化（ModelChunk）。
- 通用 RPC：`RpcRequest/RpcFrame` + 通用 `Method` 子集（send_user_message / trigger_command / edit_message / switch_branch / abort / get_state / list_sessions / read_session / list_tools）+ `custom { method, params }` 兜底 + `RpcExtension` trait；`send_user_message` 附件字段用中性 `attachments`（路径+名称），不固化本仓库的暂存/白名单语义。
- 默认服务（非插件、不注册工具、不占 namespace）：`InMemorySessionStore`（内存会话存储）、`MockModelService`（固定文本模型桩，链路自检/测试）。
- `KernelBuilder` 装配入口：

```rust
let kernel = KernelBuilder::new()
    .event_sink(events)
    .service_handles(handles)          // 使用方构造：with_storage / with_model / ...
    .register_kernel_plugin(storage_plugin())
    .register_plugin(my_business_plugin())
    .system_prompt(|| agent_system_prompt())  // 人格注入，替代 loop 直接调用
    .build()?;
```

**留使用方（不随 crate）**：

- 内核插件实现（存储/记忆/验算等业务服务；`plugin/services/storage.rs` 里 `MistakeStore`/`Mistake` 等错题领域 trait 不进 crate）。
- 用户插件（业务工具）。
- Settings、人格 prompt、具体 GUI 协议（本仓库的 settings/balance/cache/compute 方法走 `RpcExtension`）。

## 剥离前需解耦的点（M1，行为不变）

- `agent/loop_mod/`：`agent_system_prompt()` 直调 → 改为注入的 `system_prompt` provider。
- `agent/loop_mod/` / `agent/session/`：`Interrupt::SettingsChanged` → 通用 `ConfigChanged`（或去掉）。
- `plugin/services/storage.rs`：错题领域类型（MistakeStore 等）移到 app 侧；`ModelService`/`SessionStore`/`AbortSignal`/`ModelChunk`/`ToolSchema` 等通用 trait 进 crate。
- `agent/rpc/`：`Method` 拆通用子集 + `custom` 兜底；`Kernel::new` 的硬编码装配（FileStorage/LiveSettingsModelService…）改为 `KernelBuilder`。

## 里程碑（后续落地顺序）

| 阶段 | 内容 | 验收 |
|---|---|---|
| M1 | 本仓库解耦准备（上表清单） | ✅ 已落地：行为不变，123 单测 + clippy -D warnings 全绿（live_api 为 ignored，未跑真实 API） |
| M2 | 新仓库骨架：搬通用模块 + 默认服务 | ✅ 已落地：`so-lite-agent/` 独立 crate，`cargo test` 1 个集成测试 + `cargo run --example hello` 跑通 mock 回合 |
| M3 | Provider 层：内置适配器 + register_provider | ✅ 已落地：`register_provider()` + `openai/responses/anthropic` 适配器；本地 SSE 测试通过；真实 API 测试为 ignored（需 `SO_LITE_API_*`） |
| M4 | 通用 RPC + KernelBuilder 定型；插件手册/参考模板迁移到新仓库 | ✅ 已落地：插件手册随 crate（`so-lite-agent/docs/plugin-dev/`）；内核 + 用户插件双注册跑通测试 |
| M5 | 发布 crates.io（0.x）；mistake-agent 切到新 crate 消费，删除本仓库重复代码 | ⏳ 待办：`cargo package` 已通过；crates.io 上传需网络/凭据，mistake-agent 切换是大手术，需单独排期 |

## 风险与约定

- 大手术回归：每阶段保持 mistake-agent 行为不变，靠现有单测 + 真实 API 回归兜底。
- crate API 未定型前不发布 1.0（0.x 语义化版本）。
- 本仓库 AGENTS.md「单 crate 不拆分」红线已在 M1 落地时同步修订（由 ADR-0037 supersede，mistake-agent 本体在 M5 前仍保持单 crate，`so-lite-agent/` 是 ADR-0037 允许的独立 crate 骨架）。
