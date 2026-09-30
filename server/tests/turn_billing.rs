//! 回合制计费（ADR-0047 修订 R14）：**一次学生提问只扣一次**。
//!
//! 实测背景：只发一句 "hello"，服务端却产生 3 条流水——主回合、工具轮之后的第二次往返、
//! 以及会话标题生成（`session::title` 是一次独立的模型请求）。若每条各扣一次，学生眼里
//! "问一句"却掉 3 次。这里钉住修复后的口径：
//!
//! - 回合首个请求按裁决扣 1 次；同回合的后续往返**不扣次**，只记 token；
//! - 阶梯按**回合累计 token** 决定 1/2/3 次，记费固定落在回合的第一条流水上；
//! - 没有回合标识（老客户端/第三方）时退回"一请求一回合"，行为与改动前一致。
//!
//! 需要可写 PostgreSQL，连接方式见 `tests/common/mod.rs`。

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use mistake_agent_server::billing::{
    ReserveOutcome, Settlement, TokenUsage, UsageStatus, reserve, settle,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct-horse-battery";
/// 与 `BILLING_LADDER_TOKENS` 默认值一致。
const LADDER: [u64; 2] = [32 * 1024, 64 * 1024];

// ---------- HTTP 夹具（与 auth.rs / quota.rs 同形） ----------

async fn call(app: Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = match body {
        Some(value) => Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .expect("请求构造失败"),
        None => Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .expect("请求构造失败"),
    };
    let res = app.oneshot(request).await.expect("路由调用失败");
    let status = res.status();
    let bytes = res
        .into_body()
        .collect()
        .await
        .expect("读取响应体失败")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn register_one(app: &Router) -> String {
    let email = common::unique_email();
    let (status, _) = call(
        app.clone(),
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "注册应当成功");
    email
}

async fn user_id_of(pool: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(pool)
        .await
        .expect("查用户 id 失败")
}

/// 发一张自定义月卡：窗口给足，只测回合口径。
async fn grant_plan(pool: &PgPool, email: &str) {
    let plan_id: Uuid = sqlx::query_scalar(
        "INSERT INTO plans (code, name, kind, price_cents, duration_days, total_uses,
                            limit_5h, limit_week, limit_month)
         VALUES ($1, '回合测试月卡', 'monthly', 0, 30, NULL, 999, 9999, 99999)
         RETURNING id",
    )
    .bind(format!("turn-plan-{}", Uuid::new_v4().simple()))
    .fetch_one(pool)
    .await
    .expect("插入套餐失败");

    sqlx::query(
        "INSERT INTO entitlements (user_id, plan_id, source, expires_at)
         SELECT u.id, $2, 'grant', now() + interval '30 days' FROM users u WHERE u.email = $1",
    )
    .bind(email)
    .bind(plan_id)
    .execute(pool)
    .await
    .expect("发放权益失败");
}

async fn used_uses(pool: &PgPool, email: &str) -> i32 {
    sqlx::query_scalar(
        "SELECT e.used_uses FROM entitlements e JOIN users u ON u.id = e.user_id
          WHERE u.email = $1 ORDER BY e.created_at DESC LIMIT 1",
    )
    .bind(email)
    .fetch_one(pool)
    .await
    .expect("查权益失败")
}

/// 整回合被记为多少"次"（应当恒等于对外扣次）。
async fn turn_billed(pool: &PgPool, email: &str, turn: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(e.billed_uses), 0)::bigint FROM usage_events e
           JOIN users u ON u.id = e.user_id
          WHERE u.email = $1 AND e.turn_id = $2",
    )
    .bind(email)
    .bind(turn)
    .fetch_one(pool)
    .await
    .expect("回合汇总失败")
}

/// 一次预留（要求放行），返回 (流水 id, 权益 id, 是否扣次)。
async fn reserve_one(
    pool: &PgPool,
    user_id: Uuid,
    request_id: &str,
    turn_id: Option<&str>,
) -> (i64, Uuid, bool) {
    match reserve(
        pool,
        user_id,
        Uuid::new_v4(),
        request_id,
        turn_id,
        "responses",
        "deepseek-flash",
    )
    .await
    .expect("预留调用失败")
    {
        ReserveOutcome::Granted {
            event_id,
            entitlement_id,
            charged,
        } => (event_id, entitlement_id, charged),
        ReserveOutcome::Denied(denial) => panic!("应当放行，却被拒：{denial}"),
    }
}

/// 结算一条流水，返回该回合的最终扣次。
async fn finish(
    pool: &PgPool,
    event_id: i64,
    entitlement_id: Uuid,
    input: u64,
    output: u64,
    status: UsageStatus,
) -> i32 {
    let settlement = Settlement {
        status,
        billed_uses: 1,
        usage: TokenUsage {
            input_total: input,
            cached: 0,
            output,
            reasoning: 0,
        },
        latency_ms: 5,
    };
    settle(pool, event_id, entitlement_id, settlement, &LADDER)
        .await
        .expect("结算失败")
        .expect("应当结算成功")
}

// ---------- 用例 ----------

/// 学生发一句 "hello"，Agent loop 里有工具往返、外加标题生成 —— **总共只扣 1 次**。
#[tokio::test]
async fn one_turn_with_several_requests_charges_once() {
    let pool = common::test_pool().await;
    let app = common::app(pool.clone());
    let email = register_one(&app).await;
    grant_plan(&pool, &email).await;
    let user_id = user_id_of(&pool, &email).await;
    let turn = "turn-hello";

    // 1) 回合首个请求：扣 1 次
    let (event_id, entitlement_id, charged) =
        reserve_one(&pool, user_id, "req-1", Some(turn)).await;
    assert!(charged, "回合首个请求应当扣次");
    finish(&pool, event_id, entitlement_id, 4_275, 145, UsageStatus::Ok).await;
    assert_eq!(used_uses(&pool, &email).await, 1);

    // 2) 工具轮之后的第二次往返：不扣次
    // 3) 会话标题生成：不扣次
    for (seq, (input, output)) in [(4_442u64, 306u64), (520, 2)].into_iter().enumerate() {
        let (event_id, entitlement_id, charged) =
            reserve_one(&pool, user_id, &format!("req-{}", seq + 2), Some(turn)).await;
        assert!(!charged, "同回合的续跑不该扣次（第 {} 条）", seq + 2);
        finish(
            &pool,
            event_id,
            entitlement_id,
            input,
            output,
            UsageStatus::Ok,
        )
        .await;
        assert_eq!(
            used_uses(&pool, &email).await,
            1,
            "续跑结算之后仍应只扣 1 次"
        );
    }

    // 整回合只记 1 次，且三条流水合计为 1（记费落在第一条上）
    assert_eq!(turn_billed(&pool, &email, turn).await, 1);
    assert_eq!(used_uses(&pool, &email).await, 1);
}

/// 阶梯按**回合累计 token** 生效：一个回合累计超过 32k → 收到 2 次。
#[tokio::test]
async fn heavy_turn_upgrades_charge_by_turn_total() {
    let pool = common::test_pool().await;
    let app = common::app(pool.clone());
    let email = register_one(&app).await;
    grant_plan(&pool, &email).await;
    let user_id = user_id_of(&pool, &email).await;
    let turn = "turn-heavy";

    let (event_id, entitlement_id, _) = reserve_one(&pool, user_id, "req-1", Some(turn)).await;
    finish(&pool, event_id, entitlement_id, 20_000, 0, UsageStatus::Ok).await;
    assert_eq!(
        used_uses(&pool, &email).await,
        1,
        "20k 还没过阈值，先记 1 次"
    );

    let (event_id, entitlement_id, charged) =
        reserve_one(&pool, user_id, "req-2", Some(turn)).await;
    assert!(!charged, "续跑本身不扣次");
    let final_uses = finish(&pool, event_id, entitlement_id, 20_000, 0, UsageStatus::Ok).await;
    assert_eq!(final_uses, 2, "回合累计 40k 跨过 32k 阈值 → 2 次");
    assert_eq!(used_uses(&pool, &email).await, 2);
    assert_eq!(turn_billed(&pool, &email, turn).await, 2);
}

/// 没有回合标识时退回旧行为：每个请求各自 1 次（兼容老客户端/第三方）。
#[tokio::test]
async fn without_turn_id_each_request_charges_separately() {
    let pool = common::test_pool().await;
    let app = common::app(pool.clone());
    let email = register_one(&app).await;
    grant_plan(&pool, &email).await;
    let user_id = user_id_of(&pool, &email).await;

    for seq in 1..=3 {
        let (event_id, entitlement_id, charged) =
            reserve_one(&pool, user_id, &format!("legacy-{seq}"), None).await;
        assert!(charged, "没有回合标识时每次都扣次");
        finish(&pool, event_id, entitlement_id, 1_000, 100, UsageStatus::Ok).await;
    }
    assert_eq!(used_uses(&pool, &email).await, 3);
}

/// 防滥用：一个回合里的免费往返有上限，超过就重新扣次。
#[tokio::test]
async fn free_requests_within_a_turn_are_capped() {
    let pool = common::test_pool().await;
    let app = common::app(pool.clone());
    let email = register_one(&app).await;
    grant_plan(&pool, &email).await;
    let user_id = user_id_of(&pool, &email).await;
    let turn = "turn-cap";

    let mut charged_flags = Vec::new();
    for seq in 1..=13 {
        let (_, _, charged) = reserve_one(&pool, user_id, &format!("cap-{seq}"), Some(turn)).await;
        charged_flags.push(charged);
    }
    assert!(charged_flags[0], "第一条扣次");
    assert!(
        charged_flags[1..12].iter().all(|c| !c),
        "第 2–12 条免费：{charged_flags:?}"
    );
    assert!(
        charged_flags[12],
        "第 13 条超出上限，应重新扣次：{charged_flags:?}"
    );
}
