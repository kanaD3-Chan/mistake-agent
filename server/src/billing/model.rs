//! 计费领域类型（ADR-0047 决策 6/7，修订 R2/R4）。

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

/// 归一化后的用量四元组（R4）。
///
/// 三个协议面的字段名不同、`input_tokens` 的语义也不同（Responses 已含缓存命中，
/// Anthropic 不含），因此各协议适配器必须先把它们收敛到这里——**计费只认这个类型**，
/// 否则同一个用户换个协议用，账就会漂移。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// **总**输入（含缓存命中）——与 Responses 的 `input_tokens` 同口径
    pub input_total: u64,
    /// 命中上下文缓存的部分（是 `input_total` 的子集）
    pub cached: u64,
    pub output: u64,
    pub reasoning: u64,
}

impl TokenUsage {
    /// 计费口径的总量：输入 + 输出。缓存命中已含在输入里，不重复计。
    pub fn billable_tokens(&self) -> u64 {
        self.input_total.saturating_add(self.output)
    }
}

/// 套餐类型：体验包受总次数约束，月卡受三窗口约束。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanKind {
    Experience,
    Monthly,
}

impl PlanKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PlanKind::Experience => "experience",
            PlanKind::Monthly => "monthly",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "experience" => Some(PlanKind::Experience),
            "monthly" => Some(PlanKind::Monthly),
            _ => None,
        }
    }
}

/// 权益来源（`source` 预留 `payment` 供二期接在线支付）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntitlementSource {
    Redeem,
    Grant,
    Payment,
}

impl EntitlementSource {
    pub fn as_str(self) -> &'static str {
        match self {
            EntitlementSource::Redeem => "redeem",
            EntitlementSource::Grant => "grant",
            EntitlementSource::Payment => "payment",
        }
    }
}

/// 套餐（对外商品定义）。窗口阈值**不缓存**——每次裁决都读实时值，以便"改库即收紧"。
#[derive(Debug, Clone)]
pub struct Plan {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub kind: PlanKind,
    /// 体验包的一次性总次数；月卡为 `None`
    pub total_uses: Option<i32>,
    pub limit_5h: Option<i32>,
    pub limit_week: Option<i32>,
    pub limit_month: Option<i32>,
}

/// 权益：一次兑换/发放产生的可用额度实例。
#[derive(Debug, Clone)]
pub struct Entitlement {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub source: EntitlementSource,
    pub expires_at: DateTime<Utc>,
    /// 发放时的快照（改套餐不回头改已发出的权益）
    pub total_uses: Option<i32>,
    pub used_uses: i32,
}

/// 三滑动窗口的已用次数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowUsage {
    pub used_5h: i64,
    pub used_week: i64,
    pub used_month: i64,
}

/// 裁决通过：带上用于结算与提示的上下文。
#[derive(Debug, Clone)]
pub struct QuotaGrant {
    pub entitlement: Entitlement,
    pub plan: Plan,
    pub windows: WindowUsage,
}

/// 裁决拒绝的原因。对外一律 402，但 `message` 要能让用户知道该做什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaDenial {
    /// 没有任何生效权益：引导兑换
    NoEntitlement,
    /// 体验包次数用尽
    UsesExhausted {
        total: i32,
        used: i32,
    },
    Window5h {
        limit: i32,
        used: i64,
    },
    WindowWeek {
        limit: i32,
        used: i64,
    },
    WindowMonth {
        limit: i32,
        used: i64,
    },
}

impl QuotaDenial {
    /// 机器可读的原因码（客户端按码分支，比解析文案稳）。
    pub fn code(&self) -> &'static str {
        match self {
            QuotaDenial::NoEntitlement => "no_entitlement",
            QuotaDenial::UsesExhausted { .. } => "uses_exhausted",
            QuotaDenial::Window5h { .. } => "window_5h_exceeded",
            QuotaDenial::WindowWeek { .. } => "window_week_exceeded",
            QuotaDenial::WindowMonth { .. } => "window_month_exceeded",
        }
    }

    /// 面向用户的中文说明（面向中学生与家长，不说"token""窗口"这类术语）。
    pub fn message(&self) -> String {
        match self {
            QuotaDenial::NoEntitlement => "还没有可用的服务包，请先在设置里兑换".into(),
            QuotaDenial::UsesExhausted { total, used } => {
                format!("体验包已用完（共 {total} 次，已用 {used} 次），可兑换月卡继续使用")
            }
            QuotaDenial::Window5h { limit, .. } => {
                format!("近 5 小时用量已达上限（{limit} 次），请稍后再试")
            }
            QuotaDenial::WindowWeek { limit, .. } => {
                format!("本周用量已达上限（{limit} 次），下周额度会自然恢复")
            }
            QuotaDenial::WindowMonth { limit, .. } => {
                format!("本月用量已达上限（{limit} 次），可升级套餐")
            }
        }
    }
}

/// 拒绝原因直接可当作用户可见文案（错误类型 `#[error("{0}")]` 依赖这一点）。
impl std::fmt::Display for QuotaDenial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

/// 裁决结果。
#[derive(Debug, Clone)]
pub enum QuotaDecision {
    Allowed(QuotaGrant),
    Denied(QuotaDenial),
}

/// 用量记录的生命周期（R2 的"预扣 → 结算"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageStatus {
    /// 已预留（请求在飞）：已计入窗口，防止并发挤过边界
    Reserved,
    /// 正常结束
    Ok,
    /// 上游报错：不计次
    UpstreamError,
    /// 客户端中断：流已开始就至少计 1 次
    Aborted,
}

impl UsageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageStatus::Reserved => "reserved",
            UsageStatus::Ok => "ok",
            UsageStatus::UpstreamError => "upstream_error",
            UsageStatus::Aborted => "aborted",
        }
    }
}

/// 一次请求的最终结算（把预留行改写为终态）。
#[derive(Debug, Clone, Copy)]
pub struct Settlement {
    pub status: UsageStatus,
    /// 最终扣次（预留时先记 1，结算时按阶梯上调或归零）
    pub billed_uses: u32,
    pub usage: TokenUsage,
    pub latency_ms: i32,
}

// ---------- 对外额度视图（ADR-0047 修订 R13） ----------

/// 账户卡片用的滑动窗口视图。
///
/// `used` **与限流判定同源**：它由 [`super::store::quota_view`] 在同一个事务里
/// 用与放行裁决完全相同的查询读出（含在飞的 `reserved` 行），所以卡片上的数字与
/// 网关放行/拒绝的依据必然一致——不会出现「卡片说还有额度、请求却回 402」。
#[derive(Debug, Clone, Serialize)]
pub struct WindowView {
    /// 窗口标识：`five_hour` / `week` / `month`
    pub key: &'static str,
    /// 上限；`None` = 该窗口不限制（体验包只有总次数）
    pub limit: Option<i32>,
    pub used: i64,
    /// 剩余次数；不限时为 `None`。**不会为负**（已超也在 0 封底）。
    pub remaining: Option<i64>,
    /// 已用次数**首次下降**的时刻（最早一条仍被计入的用量 + 窗口长度）。
    ///
    /// 滑动窗口没有固定重置点，所以前端不要按自然日/整点渲染；无用量时为 `None`。
    pub resets_at: Option<DateTime<Utc>>,
}

/// 套餐摘要。
#[derive(Debug, Clone, Serialize)]
pub struct PlanView {
    pub code: String,
    pub name: String,
    pub kind: &'static str,
}

/// 权益摘要（体验包的一次性总次数在这里最直观）。
#[derive(Debug, Clone, Serialize)]
pub struct EntitlementView {
    pub total_uses: Option<i32>,
    pub used_uses: i32,
    pub expires_at: DateTime<Utc>,
}

/// 平台额度视图：客户端登录态那张卡片的数据源。
#[derive(Debug, Clone, Serialize)]
pub struct QuotaView {
    /// 是否有生效权益。`false` 时后面都是 `None` / 空表——客户端据此显示**兑换引导**，
    /// 这不是错误态（R13）：查自己有没有额度，本来就是一次正常查询。
    pub has_entitlement: bool,
    pub plan: Option<PlanView>,
    pub entitlement: Option<EntitlementView>,
    pub windows: Vec<WindowView>,
}
