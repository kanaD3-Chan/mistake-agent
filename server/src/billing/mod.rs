//! 计费（ADR-0047 决策 6/7，修订 R2/R4/R6）：套餐、权益、用量与限额裁决。
//!
//! 分工：
//! - `model` —— 领域类型（套餐/权益/用量四元组/裁决结果/结算状态）
//! - `ladder` —— 阶梯扣次（纯函数：token 数 → 对外次数）
//! - `quota` —— 限额裁决规则（纯函数：权益 + 窗口用量 → 放行或拒绝）
//! - `store` —— 数据库读写与**预留/结算事务**（并发正确性所在）
//!
//! 对外原则：**门面按次数、内账按 token**。所有对外判断（能不能用、还剩多少）
//! 都只用「次数」；token 明细只用于成本核算与限额校准，不对外露出。
//!
//! 规则与并发刻意分开：规则是纯函数（可被单测钉死边界），并发靠数据库
//! advisory lock 串行化同一用户的请求（见 `store::reserve`）。

mod ladder;
mod model;
mod quota;
mod store;

pub use ladder::{billed_uses, total_tokens};
pub use model::{
    Entitlement, EntitlementSource, EntitlementView, Plan, PlanKind, PlanView, QuotaDecision,
    QuotaDenial, QuotaGrant, QuotaView, Settlement, TokenUsage, UsageStatus, WindowUsage,
    WindowView,
};
pub use store::{ReserveOutcome, quota_view, reserve, settle};
