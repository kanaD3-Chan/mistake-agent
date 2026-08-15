//! so-lite-agent：可复用的通用 Agent 运行时。
//!
//! 参考 [docs/plan/so-lite-agent.md](../../docs/plan/so-lite-agent.md) 的 M2 骨架：
//! 模型 Provider 抽象、Agent loop、工具注册/调度、消息树、事件流、通用 RPC
//! 与默认服务随包提供；业务内核插件/用户插件由使用方编写。

pub mod agent;
pub mod audit;
pub mod context;
pub mod contract;
pub mod defaults;
pub mod dispatch;
pub mod events;
pub mod message;
pub mod model;
pub mod registry;
pub mod rpc;
pub mod services;
