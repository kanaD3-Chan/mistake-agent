# Agent core 剥离为独立 crate `so-lite-agent`（参考 Pi 分层）

决策：把可复用的通用 Agent 运行时（Agent loop、工具注册与调度、会话生命周期、模型 Provider 抽象、通用 RPC、默认内存服务）从 mistake-agent 剥离，放入**独立仓库的独立 crate `so-lite-agent`**；内核插件与用户插件不随 crate 分发，由使用方自行编写；本仓库保持现状，作为参考实现与消费方。crate 定位参考 earendil-works/pi 的分层：模型 Provider 层（pi-ai 等价物）+ Agent core 层（pi-agent-core 等价物）内置随包，领域层留给使用方。

本决策 supersede 架构红线「单 crate：src/kernel/ 与 src/plugin/ 分区，不新增 crate 拆分」（AGENTS.md / PROJECT.md §4）——该红线服务于 v2 单应用的简单性；跨应用复用的目标使拆分成为必要。

**状态更新（2026-08-19）**：M1-M4 已落地并从 mistake-agent 提取迁出至独立 `so-lite-agent` 仓库；mistake-agent 本仓库自 v0.1.0 起不再包含 `so-lite-agent/` 子目录（见 [CHANGELOG.md](../../CHANGELOG.md) "Removed" 节）。M5（crates.io 发布）待办，在新仓库推进。原始进度记录与实施顺序见 [docs/plan/so-lite-agent.md](../plan/so-lite-agent.md)（历史归档）。

考虑过的替代方案：本仓库改 workspace（开发期 path 依赖、同源文档）——被否，原因是剥离是大手术，独立仓库让 crate 边界物理强制、双方演进互不阻塞；代价是文档需随 crate 迁移，由 M4 阶段显式处理。
