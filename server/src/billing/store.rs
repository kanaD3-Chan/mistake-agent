//! 计费数据库读写（ADR-0047 决策 6/7，修订 R2/R6）。
//!
//! 并发正确性的关键在 [`reserve`]：它在**同一个事务**里先取该用户的 advisory lock
//! （`pg_advisory_xact_lock`，按命名空间隔离），再重读权益与三窗口、跑一次纯函数裁决、
//! 写预留行。于是同一用户的并发请求被串行化，不会出现两个请求同时读到"还差 1 次"
//! 并双双放行——这是 new-api 预扣机制在滑动窗口上的对应物。
//!
//! 计费规则本身在 [`super::quota`]（纯函数），本模块只负责"读状态 → 调规则 → 落状态"。
//! 行结构体只在本模块出现，出模块一律是 [`super::model`] 的领域类型。

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{
    Entitlement, EntitlementSource, EntitlementView, Plan, PlanKind, PlanView, QuotaDecision,
    QuotaDenial, QuotaView, Settlement, UsageStatus, WindowUsage, WindowView,
};
use super::{ladder, quota};

/// advisory lock 的命名空间：与其他用途的锁互不干扰。
const LOCK_NAMESPACE: i32 = 0x4D41_0001;

/// 回合续跑的有效窗口：学生一次提问里的工具往返/辅助调用都在这段时间内（ADR-0047 修订 R14）。
const TURN_WINDOW_MINUTES: i32 = 15;

/// 单回合允许的**免费**往返上限：超过就重新按正常路径计费，防"把一切都塞进一个回合"。
const TURN_MAX_REQUESTS: i64 = 12;

/// 预留结果：放行（带流水 id 与承担计费的权益）或拒绝（带可对用户解释的原因）。
#[derive(Debug, Clone)]
pub enum ReserveOutcome {
    Granted {
        event_id: i64,
        /// 承担本回合计费的权益（免费往返也记在它名下，便于对账）
        entitlement_id: Uuid,
        /// `false` = 本回合的续跑（工具往返/辅助调用），**不扣次**
        charged: bool,
    },
    Denied(QuotaDenial),
}

// ---------- 行结构 ----------

#[derive(sqlx::FromRow)]
struct BillingRow {
    id: Uuid,
    plan_id: Uuid,
    source: String,
    expires_at: DateTime<Utc>,
    total_uses: Option<i32>,
    used_uses: i32,
    p_id: Uuid,
    p_code: String,
    p_name: String,
    p_kind: String,
    p_total_uses: Option<i32>,
    p_limit_5h: Option<i32>,
    p_limit_week: Option<i32>,
    p_limit_month: Option<i32>,
}

impl BillingRow {
    fn split(self) -> Result<(Entitlement, Plan), sqlx::Error> {
        let source = match self.source.as_str() {
            "redeem" => EntitlementSource::Redeem,
            "grant" => EntitlementSource::Grant,
            "payment" => EntitlementSource::Payment,
            other => return Err(decode_error(format!("entitlements.source 非法：{other}"))),
        };
        let kind = PlanKind::parse(&self.p_kind)
            .ok_or_else(|| decode_error(format!("plans.kind 非法：{}", self.p_kind)))?;

        let entitlement = Entitlement {
            id: self.id,
            plan_id: self.plan_id,
            source,
            expires_at: self.expires_at,
            total_uses: self.total_uses,
            used_uses: self.used_uses,
        };
        let plan = Plan {
            id: self.p_id,
            code: self.p_code,
            name: self.p_name,
            kind,
            total_uses: self.p_total_uses,
            limit_5h: self.p_limit_5h,
            limit_week: self.p_limit_week,
            limit_month: self.p_limit_month,
        };
        Ok((entitlement, plan))
    }
}

#[derive(sqlx::FromRow)]
struct WindowRow {
    used_5h: i64,
    used_week: i64,
    used_month: i64,
}

fn decode_error(message: String) -> sqlx::Error {
    sqlx::Error::Decode(message.into())
}

// ---------- 读 ----------

/// 生效权益：**最早到期优先**（先花掉快过期的），跳过已用尽的体验包与已停用的套餐。
async fn find_entitlement_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Option<(Entitlement, Plan)>, sqlx::Error> {
    let row = sqlx::query_as::<_, BillingRow>(
        "SELECT e.id, e.plan_id, e.source, e.expires_at, e.total_uses, e.used_uses,
                p.id AS p_id, p.code AS p_code, p.name AS p_name, p.kind AS p_kind,
                p.total_uses AS p_total_uses, p.limit_5h AS p_limit_5h,
                p.limit_week AS p_limit_week, p.limit_month AS p_limit_month
         FROM entitlements e
         JOIN plans p ON p.id = e.plan_id
         WHERE e.user_id = $1
           AND e.status = 'active'
           AND e.expires_at > now()
           AND p.enabled
           AND (e.total_uses IS NULL OR e.used_uses < e.total_uses)
         ORDER BY e.expires_at ASC
         LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(BillingRow::split).transpose()
}

/// 三窗口已用次数。
///
/// **包含 `reserved` 行**——预留即计入，这正是并发下不超额的原因；
/// 上游失败的行 `billed_uses` 会被结算为 0，自然不计入。
async fn window_usage_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<WindowUsage, sqlx::Error> {
    let row = sqlx::query_as::<_, WindowRow>(
        "SELECT
             COALESCE(SUM(billed_uses) FILTER (WHERE created_at > now() - interval '5 hours'), 0)::bigint  AS used_5h,
             COALESCE(SUM(billed_uses) FILTER (WHERE created_at > now() - interval '7 days'),  0)::bigint  AS used_week,
             COALESCE(SUM(billed_uses) FILTER (WHERE created_at > now() - interval '30 days'), 0)::bigint  AS used_month
         FROM usage_events
         WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(WindowUsage {
        used_5h: row.used_5h,
        used_week: row.used_week,
        used_month: row.used_month,
    })
}

// ---------- 预留 / 结算 ----------

/// 预留（R2 + R14 回合制）：裁决通过则写 `reserved` 流水，否则返回拒绝原因。
///
/// **一个用户回合对外只算一次提问**：回合的首个请求按裁决扣 1 次；同回合的后续往返
/// （Agent loop 的工具轮、会话标题等辅助调用）**不扣次**，只写一条 `billed_uses = 0`
/// 的流水记 token。阶梯会按该回合累计 token 把最终扣次上调到 2/3 次——见
/// [`recompute_turn_charge`]。
///
/// 防滥用：免费往返受 [`TURN_WINDOW_MINUTES`] 与 [`TURN_MAX_REQUESTS`] 双重约束，
/// 超出即回到正常路径重新扣次。
///
/// 裁决与预留必须原子——所以整段在一个事务里，且事务开头就取用户级 advisory lock。
pub async fn reserve(
    pool: &PgPool,
    user_id: Uuid,
    token_id: Uuid,
    request_id: &str,
    turn_id: Option<&str>,
    protocol: &str,
    model: &str,
) -> Result<ReserveOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
        .bind(LOCK_NAMESPACE)
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await?;

    // ---------- 同一回合的续跑：不扣次 ----------
    if let Some(turn) = turn_id {
        #[derive(sqlx::FromRow)]
        struct TurnHead {
            first_event_id: Option<i64>,
            entitlement_id: Option<Uuid>,
            seen: i64,
        }
        let head = sqlx::query_as::<_, TurnHead>(
            // 取**第一条**流水的权益：记费落在它头上，续跑也跟着它走
            // （PostgreSQL 没有 max(uuid) 聚合，所以用 array_agg 排序取首元素）。
            "SELECT min(id) AS first_event_id,
                    (array_agg(entitlement_id ORDER BY id))[1] AS entitlement_id,
                    count(*) AS seen
               FROM usage_events
              WHERE user_id = $1 AND turn_id = $2
                AND created_at > now() - make_interval(mins => $3)",
        )
        .bind(user_id)
        .bind(turn)
        .bind(TURN_WINDOW_MINUTES)
        .fetch_one(&mut *tx)
        .await?;

        if head.first_event_id.is_some()
            && head.seen < TURN_MAX_REQUESTS
            && let Some(entitlement_id) = head.entitlement_id
        {
            let event_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO usage_events
                     (user_id, entitlement_id, token_id, request_id, turn_id, protocol, model,
                      billed_uses, status)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, 0, 'reserved')
                 RETURNING id",
            )
            .bind(user_id)
            .bind(entitlement_id)
            .bind(token_id)
            .bind(request_id)
            .bind(turn)
            .bind(protocol)
            .bind(model)
            .fetch_one(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(ReserveOutcome::Granted {
                event_id,
                entitlement_id,
                charged: false,
            });
        }
    }

    // ---------- 回合首个请求（或没有回合标识）：正常裁决并扣 1 次 ----------
    let Some((entitlement, plan)) = find_entitlement_tx(&mut tx, user_id).await? else {
        // 没有生效权益：事务随 drop 回滚，锁随之释放
        return Ok(ReserveOutcome::Denied(QuotaDenial::NoEntitlement));
    };
    let windows = window_usage_tx(&mut tx, user_id).await?;

    let grant = match quota::decide(&plan, &entitlement, windows) {
        QuotaDecision::Allowed(grant) => grant,
        QuotaDecision::Denied(denial) => return Ok(ReserveOutcome::Denied(denial)),
    };

    sqlx::query("UPDATE entitlements SET used_uses = used_uses + 1 WHERE id = $1")
        .bind(grant.entitlement.id)
        .execute(&mut *tx)
        .await?;

    let event_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO usage_events
             (user_id, entitlement_id, token_id, request_id, turn_id, protocol, model,
              billed_uses, status)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 1, 'reserved')
         RETURNING id",
    )
    .bind(user_id)
    .bind(grant.entitlement.id)
    .bind(token_id)
    .bind(request_id)
    .bind(turn_id)
    .bind(protocol)
    .bind(model)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(ReserveOutcome::Granted {
        event_id,
        entitlement_id: grant.entitlement.id,
        charged: true,
    })
}

/// 结算预留行（R2 + R14）：写入终态与真实用量，然后**整回合重算扣次**。
///
/// 幂等——同一行只结算一次（`status = 'reserved'` 是前置条件）；扣次的最终归属由
/// [`recompute_turn_charge`] 决定，所以工具轮/辅助调用谁先结算都不影响结果。
///
/// 返回本回合的最终扣次（`None` = 这行已被结算过）。
pub async fn settle(
    pool: &PgPool,
    event_id: i64,
    _entitlement_id: Uuid,
    settlement: Settlement,
    ladder: &[u64],
) -> Result<Option<i32>, sqlx::Error> {
    let mut tx = pool.begin().await?;

    let updated = sqlx::query(
        "UPDATE usage_events SET
             input_tokens = $2, cached_tokens = $3, output_tokens = $4, reasoning_tokens = $5,
             latency_ms = $6, status = $7
         WHERE id = $1 AND status = 'reserved'",
    )
    .bind(event_id)
    .bind(as_i64(settlement.usage.input_total))
    .bind(as_i64(settlement.usage.cached))
    .bind(as_i64(settlement.usage.output))
    .bind(as_i64(settlement.usage.reasoning))
    .bind(settlement.latency_ms)
    .bind(settlement.status.as_str())
    .execute(&mut *tx)
    .await?
    .rows_affected();

    if updated == 0 {
        // 已经被结算过（重复回调/重试）：保持幂等，不再动权益
        return Ok(None);
    }

    let (user_id, turn_id): (Uuid, Option<String>) =
        sqlx::query_as("SELECT user_id, turn_id FROM usage_events WHERE id = $1")
            .bind(event_id)
            .fetch_one(&mut *tx)
            .await?;

    let charged =
        recompute_turn_charge(&mut tx, user_id, turn_id.as_deref(), event_id, ladder).await?;
    tx.commit().await?;
    Ok(Some(charged))
}

/// 整回合重算扣次（ADR-0047 修订 R14）。
///
/// 规则：**一个回合对外只算一次提问**；阶梯按该回合**累计 token** 决定 1/2/3 次，
/// 记费固定落在回合的**第一条**流水上，同回合其余流水 `billed_uses` 归零（token 明细照记）。
///
/// 幂等：每次结算都整回合重算一遍，所以工具往返与辅助调用的先后顺序不影响最终账目。
/// `turn_id` 为 `None` 时该行自成一回合，行为与改动前一致。
async fn recompute_turn_charge(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    turn_id: Option<&str>,
    row_id: i64,
    ladder: &[u64],
) -> Result<i32, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        status: String,
        tokens: i64,
        billed: i32,
        entitlement_id: Option<Uuid>,
    }

    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, status,
                (COALESCE(input_tokens, 0) + COALESCE(output_tokens, 0))::bigint AS tokens,
                billed_uses AS billed, entitlement_id
           FROM usage_events
          WHERE user_id = $1
            AND (($2::text IS NULL AND id = $3) OR ($2::text IS NOT NULL AND turn_id = $2))
          ORDER BY id",
    )
    .bind(user_id)
    .bind(turn_id)
    .bind(row_id)
    .fetch_all(&mut **tx)
    .await?;

    let Some(first) = rows.first() else {
        return Ok(0);
    };
    let in_flight = rows
        .iter()
        .any(|r| r.status == UsageStatus::Reserved.as_str());
    let answered = rows.iter().any(|r| r.status == UsageStatus::Ok.as_str());
    let aborted = rows
        .iter()
        .any(|r| r.status == UsageStatus::Aborted.as_str());
    let total: u64 = rows
        .iter()
        .map(|r| u64::try_from(r.tokens).unwrap_or(0))
        .sum();

    // 回合还在飞：维持已记的 1 次，等最后一条结算再按累计 token 上调。
    // 有答复才算钱；只有中断（客户端断开）时至少 1 次；全失败则 0 次。
    let desired: i32 = if in_flight {
        first.billed.max(1)
    } else if answered {
        i32::try_from(ladder::billed_uses(total, ladder)).unwrap_or(i32::MAX)
    } else if aborted {
        i32::try_from(ladder::billed_uses(total, ladder))
            .unwrap_or(i32::MAX)
            .max(1)
    } else {
        0
    };

    if first.billed != desired {
        sqlx::query("UPDATE usage_events SET billed_uses = $2 WHERE id = $1")
            .bind(first.id)
            .bind(desired)
            .execute(&mut **tx)
            .await?;
        if let Some(entitlement_id) = first.entitlement_id {
            sqlx::query(
                "UPDATE entitlements SET used_uses = GREATEST(used_uses + $2, 0) WHERE id = $1",
            )
            .bind(entitlement_id)
            .bind(desired - first.billed)
            .execute(&mut **tx)
            .await?;
        }
    }

    // 同回合其余流水一律 0 次（token 明细保留）
    sqlx::query(
        "UPDATE usage_events SET billed_uses = 0
          WHERE user_id = $1 AND id <> $2 AND billed_uses <> 0
            AND $3::text IS NOT NULL AND turn_id = $3",
    )
    .bind(user_id)
    .bind(first.id)
    .bind(turn_id)
    .execute(&mut **tx)
    .await?;

    Ok(desired)
}

/// u64 → i64：token 数量不可能接近 i64 上限，饱和转换只为"任何输入都不 panic"。
fn as_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

// ---------- 额度视图（只读，ADR-0047 修订 R13） ----------

#[derive(sqlx::FromRow)]
struct ResetRow {
    first_5h: Option<DateTime<Utc>>,
    first_week: Option<DateTime<Utc>>,
    first_month: Option<DateTime<Utc>>,
}

/// 各窗口「最早一条仍被计入的用量」。
///
/// 只认 `billed_uses > 0` 的行——上游失败被结算为 0 次的流水不该凭空造出一个重置时刻。
/// **interval 必须与 [`window_usage_tx`] 逐字对应**，否则"已用"与"何时下降"会错位。
async fn window_resets_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<ResetRow, sqlx::Error> {
    sqlx::query_as::<_, ResetRow>(
        "SELECT
             MIN(created_at) FILTER (WHERE created_at > now() - interval '5 hours')  AS first_5h,
             MIN(created_at) FILTER (WHERE created_at > now() - interval '7 days')   AS first_week,
             MIN(created_at) FILTER (WHERE created_at > now() - interval '30 days')  AS first_month
         FROM usage_events
         WHERE user_id = $1 AND billed_uses > 0",
    )
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await
}

/// 组装对外视图：上限（实时读库）+ 已用（同一事务）+ 重置时刻。
fn build_view(
    effective: Option<(Entitlement, Plan)>,
    windows: WindowUsage,
    resets: ResetRow,
) -> QuotaView {
    let Some((entitlement, plan)) = effective else {
        return QuotaView {
            has_entitlement: false,
            plan: None,
            entitlement: None,
            windows: Vec::new(),
        };
    };

    // (key, 上限, 已用, 最早计入时刻, 窗口长度)
    let rows = [
        (
            "five_hour",
            plan.limit_5h,
            windows.used_5h,
            resets.first_5h,
            chrono::Duration::hours(5),
        ),
        (
            "week",
            plan.limit_week,
            windows.used_week,
            resets.first_week,
            chrono::Duration::days(7),
        ),
        (
            "month",
            plan.limit_month,
            windows.used_month,
            resets.first_month,
            chrono::Duration::days(30),
        ),
    ];
    let windows = rows
        .into_iter()
        .map(|(key, limit, used, first, span)| WindowView {
            key,
            limit,
            used,
            // 已超也封底 0：卡片不该显示负数剩余
            remaining: limit.map(|l| (i64::from(l) - used).max(0)),
            resets_at: first.map(|t| t + span),
        })
        .collect();

    QuotaView {
        has_entitlement: true,
        plan: Some(PlanView {
            code: plan.code,
            name: plan.name,
            kind: plan.kind.as_str(),
        }),
        entitlement: Some(EntitlementView {
            total_uses: entitlement.total_uses,
            used_uses: entitlement.used_uses,
            expires_at: entitlement.expires_at,
        }),
        windows,
    }
}

/// 读平台额度视图（`GET /api/v1/me/quota` 的数据源）。
///
/// **刻意复用** `find_entitlement_tx` / `window_usage_tx`：R13 要求卡片数字与限流判定同源，
/// 所以这里不是"再写一份计数 SQL"，而是在同一个事务里走同一段代码。
pub async fn quota_view(pool: &PgPool, user_id: Uuid) -> Result<QuotaView, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let effective = find_entitlement_tx(&mut tx, user_id).await?;
    let windows = window_usage_tx(&mut tx, user_id).await?;
    let resets = window_resets_tx(&mut tx, user_id).await?;
    tx.commit().await?;
    Ok(build_view(effective, windows, resets))
}
