//! 内核模块入口（Linux 内核组织风格）。
//!
//! 布局：
//! - `agent/`：Agent 核心调度层（loop/dispatch/会话调度/RPC，见 agent/mod.rs）；
//! - `plugin/`：内核插件层（storage/memory/compute/model/session，一插件一文件夹，
//!   入口 mod.rs 承载插件 info，经 `KernelPlugin` 两段式契约注册，ADR-0035）；
//! - 根目录：公共设施与契约（registry/contract/context/events/audit/logger/message/prompt/settings）。
//!   用户插件见 `crate::plugin/`（每个插件一个目录，入口 mod.rs 承载插件 info）。

pub mod account;
pub mod agent;
pub mod audit;
pub mod bootstrap;
pub mod context;
pub mod contract;
pub mod events;
pub mod logger;
pub mod message;
pub mod plugin;
pub mod prompt;
pub mod registry;
pub mod settings;
