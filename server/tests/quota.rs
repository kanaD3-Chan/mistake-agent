//! 平台额度视图（ADR-0047 修订 R13）：`GET /api/v1/me/quota`。
//!
//! 最要紧的断言是**同源**：卡片上的 `used` 必须与限流判定出自同一段计算
//! （`store::window_usage_tx`，含在飞的 `reserved` 行）。否则会出现
//! 「卡片显示还有额度、网关却回 402」——这正是 R13 第 1 条要防的坑。
//!
//! 这里的验证方式是**两头对撞**：先断言视图给出的数字，再调一次真正的
//! [`mistake_agent_server::billing::reserve`]（它只读、拒绝路径不写库）看网关是否
//! 给出同样的结论。数字对不上或口径分叉，这条就会红。
//!
//! 需要可写 PostgreSQL，连接方式见 `tests/common/mod.rs`。

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct-horse-battery";

// ---------- 请求辅助 ----------

async fn call(
    app: Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = match body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .expect("请求构造失败"),
        None => builder.body(Body::empty()).expect("请求构造失败"),
    };
    let res = app.oneshot(request).await.expect("路由调用失败");
    let status = res.status();
    let bytes = res
        .into_body()
        .collect()
        .await
        .expect("读取响应体失败")
        .to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn register_and_login(app: &Router) -> (String, String) {
    let email = common::unique_email();
    let (status, _) = call(
        app.clone(),
        "POST",
        "/api/v1/auth/register",
        None,
        Some(json!({"email": email, "password": PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "注册应当成功");
    let (status, body) = call(
        app.clone(),
        "POST",
        "/api/v1/auth/login",
        None,
        Some(json!({"email": email, "password": PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "登录应当成功");
    let token = body["token"]
        .as_str()
        .expect("登录响应里应当有令牌")
        .to_string();
    (email, token)
}

// ---------- 造数 ----------

async fn user_id_of(pool: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(pool)
        .await
        .expect("查用户 id 失败")
}

/// 发一张自定义月卡：窗口阈值取小值（3 / 5 / 9），方便精确断言。
///
/// S4 之前没有发码接口，测试用 SQL 直接造——与 `server/README.md` 的手工办法一致。
async fn grant_custom_plan(pool: &PgPool, email: &str) {
    let plan_id: Uuid = sqlx::query_scalar(
        "INSERT INTO plans (code, name, kind, price_cents, duration_days, total_uses,
                            limit_5h, limit_week, limit_month)
         VALUES ($1, '测试月卡', 'monthly', 0, 30, NULL, 3, 5, 9)
         RETURNING id",
    )
    .bind(format!("test-plan-{}", Uuid::new_v4().simple()))
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

/// 造一条用量流水；`minutes_ago` 决定它落在哪个窗口里。
async fn insert_usage(pool: &PgPool, email: &str, minutes_ago: i32, billed: i32, status: &str) {
    sqlx::query(
        "INSERT INTO usage_events
             (user_id, request_id, protocol, model, billed_uses, status, created_at)
         SELECT u.id, $2, 'responses', 'deepseek-flash', $3, $4,
                now() - make_interval(mins => $5)
         FROM users u WHERE u.email = $1",
    )
    .bind(email)
    .bind(Uuid::new_v4().to_string())
    .bind(billed)
    .bind(status)
    .bind(minutes_ago)
    .execute(pool)
    .await
    .expect("插入用量失败");
}

/// 从视图里取某个窗口的对象。
fn window<'a>(body: &'a Value, key: &str) -> &'a Value {
    body["windows"]
        .as_array()
        .expect("windows 应当是数组")
        .iter()
        .find(|w| w["key"] == key)
        .unwrap_or_else(|| panic!("找不到窗口 {key}：{body}"))
}

// ---------- 用例 ----------

#[tokio::test]
async fn quota_requires_a_token() {
    let pool = common::test_pool().await;
    let app = common::app(pool);
    let (status, body) = call(app, "GET", "/api/v1/me/quota", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "missing_token");
}

/// 没有生效权益**不是错误**：200 + 空窗口，客户端据此显示兑换引导（R13 第 3 条）。
#[tokio::test]
async fn quota_without_entitlement_is_200_with_empty_windows() {
    let pool = common::test_pool().await;
    let app = common::app(pool);
    let (_email, token) = register_and_login(&app).await;

    let (status, body) = call(app, "GET", "/api/v1/me/quota", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "查额度不该是错误：{body}");
    assert_eq!(body["has_entitlement"], false);
    assert!(body["plan"].is_null());
    assert!(body["entitlement"].is_null());
    assert_eq!(
        body["windows"].as_array().expect("windows 存在").len(),
        0,
        "无权益时不该凭空造出窗口"
    );
}

/// 三窗口口径 + 重置时刻 + 与网关裁决同源。
#[tokio::test]
async fn quota_windows_match_enforcement_and_count_reserved_rows() {
    let pool = common::test_pool().await;
    let app = common::app(pool.clone());
    let (email, token) = register_and_login(&app).await;
    grant_custom_plan(&pool, &email).await;

    insert_usage(&pool, &email, 10, 1, "ok").await; // 5 小时窗口内
    insert_usage(&pool, &email, 200, 1, "ok").await; // 5 小时窗口内（最早的一条）
    // **在飞的预留行也必须计入**——这是并发下不超额的根据，卡片口径必须一致
    insert_usage(&pool, &email, 60, 1, "reserved").await;
    // 上游失败被结算为 0 次：留档但不得影响任何窗口
    insert_usage(&pool, &email, 30, 0, "upstream_error").await;
    insert_usage(&pool, &email, 8 * 24 * 60, 1, "ok").await; // 8 天前：只在月窗口
    insert_usage(&pool, &email, 40 * 24 * 60, 1, "ok").await; // 40 天前：哪个窗口都不算

    let (status, body) = call(app, "GET", "/api/v1/me/quota", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["has_entitlement"], true);
    assert_eq!(body["plan"]["name"], "测试月卡");
    assert_eq!(body["plan"]["kind"], "monthly");

    let five = window(&body, "five_hour");
    assert_eq!(five["limit"], 3);
    assert_eq!(
        five["used"], 3,
        "5 小时内应计入 3 次（含 reserved）：{five}"
    );
    assert_eq!(five["remaining"], 0, "已用满即剩余 0（不显示负数）：{five}");

    let week = window(&body, "week");
    assert_eq!(week["limit"], 5);
    assert_eq!(week["used"], 3);
    assert_eq!(week["remaining"], 2);

    let month = window(&body, "month");
    assert_eq!(month["limit"], 9);
    assert_eq!(month["used"], 4, "8 天前那条只进月窗口：{month}");
    assert_eq!(month["remaining"], 5);

    // 重置时刻 = 最早一条仍被计入的用量 + 窗口长度（滑动窗口没有固定重置点）
    let resets = five["resets_at"]
        .as_str()
        .unwrap_or_else(|| panic!("5 小时窗口应当给出 resets_at：{five}"))
        .parse::<chrono::DateTime<chrono::Utc>>()
        .expect("resets_at 应当是 RFC3339");
    let expected = chrono::Utc::now() + chrono::Duration::minutes(100); // 200 分钟前 + 5 小时
    let drift = (resets - expected).num_seconds().abs();
    assert!(
        drift < 120,
        "resets_at 应约等于 100 分钟后（实际 {resets}，偏差 {drift}s）"
    );

    // **同源对撞**：卡片说 5 小时窗口已用满，网关必须给出同样的结论。
    // 用真正的 reserve 走一遍裁决——它只读，拒绝路径不写库。
    let user_id = user_id_of(&pool, &email).await;
    let outcome = mistake_agent_server::billing::reserve(
        &pool,
        user_id,
        Uuid::new_v4(),
        "quota-consistency-probe",
        "responses",
        "deepseek-flash",
    )
    .await
    .expect("预留调用失败");
    match outcome {
        mistake_agent_server::billing::ReserveOutcome::Denied(denial) => {
            assert_eq!(
                denial.code(),
                "window_5h_exceeded",
                "卡片与网关必须得出同一个结论（否则「卡片说还有额度、网关回 402」）：{denial}"
            );
        }
        mistake_agent_server::billing::ReserveOutcome::Granted { .. } => {
            panic!("卡片已显示 5 小时窗口用尽，网关却放行了——口径已经分叉");
        }
    }
}
