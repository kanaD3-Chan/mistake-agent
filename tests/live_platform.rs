//! 平台账号端到端走查（补 ADR-0048 修订 R9 的那条空白；步骤见 docs/testing.md §5）。
//!
//! 运行（PowerShell）：
//! ```text
//! $env:USERPROFILE = "$PWD\.tmp-live-platform"     # 隔离数据根，不动真 settings.json
//! $env:PLATFORM_SERVER_URL = "http://127.0.0.1:8080"
//! cargo test --test live_platform -- --ignored --nocapture
//! ```
//!
//! 两个刻意的设计（都是为了"验得可信"）：
//!
//! 1. **隔离数据根**：`Settings::data_root()` 优先读 `USERPROFILE`，所以把它指向临时目录后，
//!    登录/登出触发的 `Settings::save()` 只写在那里——真实
//!    `~/Documents/.mistake-agent/settings.json` 一个字节都不会动（比"先备份再跑"更干净）。
//! 2. **假 key + 死地址**：`main_model` 填一个必然失败的假 key、地址指向 `127.0.0.1:1`。
//!    于是"模型答出来了"这件事本身**就是**证据——请求走的是平台令牌；要是令牌没生效，
//!    DeepSeek 会拿这个假 key 回 401，测试必然失败。

use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use mistake_agent::kernel::account::AccountService;
use mistake_agent::kernel::agent::session::InterruptBus;
use mistake_agent::kernel::audit::{Auditor, MemoryAuditSink};
use mistake_agent::kernel::events::MemoryEventSink;
use mistake_agent::kernel::logger::Level;
use mistake_agent::kernel::message::Message;
use mistake_agent::kernel::plugin::model::LiveSettingsModelService;
use mistake_agent::kernel::plugin::services::{
    AbortSignal, ModelError, ModelRequest, ModelService,
};
use mistake_agent::kernel::settings::{AccountConfig, ModelConfig, Settings};

fn server_url() -> String {
    std::env::var("PLATFORM_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into())
}

/// 自备模型配置刻意是**假的**：key 假、地址死。登录后这两个都必须被平台令牌覆盖。
fn base_settings(url: &str) -> Settings {
    Settings {
        log_level: Level::Info,
        english_mode: false,
        nickname: "走查".into(),
        main_model: ModelConfig {
            api_url: "http://127.0.0.1:1".into(),
            api_key: "sk-self-should-be-ignored".into(),
            model: Some("deepseek-flash".into()),
            transport: None,
        },
        vision_model: ModelConfig {
            api_url: String::new(),
            api_key: String::new(),
            model: None,
            transport: None,
        },
        account: AccountConfig {
            server_url: url.into(),
            token: String::new(),
            email: String::new(),
            role: String::new(),
            sync_enabled: false,
        },
    }
}

/// 账号服务 + 它持有的模型服务（同一实例，才能观察到登录后的 `refresh()` 生效）。
fn harness(settings: Arc<RwLock<Settings>>) -> (AccountService, Arc<LiveSettingsModelService>) {
    let model = Arc::new(LiveSettingsModelService::new(settings.clone()));
    let service = AccountService::new(
        settings.clone(),
        model.clone(),
        InterruptBus::new(),
        Auditor::new(Arc::new(MemoryAuditSink::default())),
        Arc::new(MemoryEventSink::default()),
    )
    .expect("账号服务构造不应失败");
    (service, model)
}

/// 关掉思考模式：走查只关心"链路通不通"，不关心推理内容。
fn one_shot_request(text: &str) -> ModelRequest {
    let mut req = ModelRequest::chat(vec![Message::user(text)]);
    req.reasoning_effort = Some("none".into());
    req
}

/// §5 步骤 3：注册 → 登录 → 令牌落盘。顺带确认公开视图不泄漏令牌。
#[tokio::test]
#[ignore]
async fn walkthrough_register_and_login_persist_token() {
    let url = server_url();
    std::fs::create_dir_all(Settings::data_root()).expect("建隔离数据根失败");
    let settings = Arc::new(RwLock::new(base_settings(&url)));
    let (service, _model) = harness(settings.clone());

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时间异常")
        .as_secs();
    let email = format!("walkthrough-{stamp}@example.test");
    let password = "walkthrough-pw-123";

    println!("注册：{email}");
    let registered = service
        .register(&email, password, Some("走查同学"))
        .await
        .expect("注册应当成功（201）");
    println!("注册返回：{registered}");

    let view = service.login(&email, password).await.expect("登录应当成功");
    println!("登录返回：{view}");

    {
        let s = settings.read().expect("settings 读锁");
        assert_eq!(
            s.account.email, email,
            "登录必须把邮箱落到 settings.account"
        );
        assert_eq!(s.account.role, "user", "新注册账号的角色应当是 user");
        assert!(
            s.account.token.starts_with("mka_"),
            "令牌应当是不透明的 mka_ 串"
        );
        // 回归红线（§5 步骤 7 的一半）：不登录时行为不变——这里确认令牌没被塞进 main_model。
        assert_eq!(
            s.main_model.api_key, "sk-self-should-be-ignored",
            "令牌绝不能写进 main_model.api_key（两处必须分开存）"
        );
    }
    assert!(
        !view
            .to_string()
            .contains(&settings.read().unwrap().account.token),
        "公开视图不得包含令牌明文"
    );

    // §5 步骤 6：登出 → 令牌清空、server_url 保留
    service.logout().await.expect("登出应当成功");
    let s = settings.read().expect("settings 读锁");
    assert!(s.account.token.is_empty(), "登出必须清空令牌");
    assert_eq!(s.account.server_url, url, "登出不得清掉服务端地址");
}

/// §5 步骤 4：**零配置走平台**。demo 账号已发月卡 Pro（server/README.md 的手工 SQL）。
#[tokio::test]
#[ignore]
async fn walkthrough_platform_turn_without_any_self_key() {
    let url = server_url();
    let email = std::env::var("PLATFORM_DEMO_EMAIL").unwrap_or_else(|_| "demo@example.test".into());
    let password =
        std::env::var("PLATFORM_DEMO_PASSWORD").unwrap_or_else(|_| "demo-password-123".into());

    std::fs::create_dir_all(Settings::data_root()).expect("建隔离数据根失败");
    let settings = Arc::new(RwLock::new(base_settings(&url)));
    let (service, model) = harness(settings.clone());

    service
        .login(&email, &password)
        .await
        .unwrap_or_else(|e| panic!("demo 账号登录失败：{e:?}"));

    // 自备 key 是假的、地址是死的：能答出来就说明用的是平台令牌。
    let response = model
        .complete(
            &one_shot_request("用一句话说明什么是错题本。"),
            &AbortSignal::new(),
        )
        .await
        .expect("平台中转应当返回（若这里报 401，说明请求用了自备假 key）");

    println!("模型回答：{}", response.text);
    assert!(!response.text.trim().is_empty(), "回答不得为空");
    assert!(
        response.usage.is_some(),
        "平台应当回传 usage（服务端结算与阶梯扣次的依据）"
    );
}

/// §5 步骤 4 的**反向**：新账号没有权益时，平台必须明确拒绝（402），
/// 而不是悄悄退回自备 key——那会把"学生花了谁的钱"这件事搞乱。
#[tokio::test]
#[ignore]
async fn walkthrough_platform_rejects_account_without_entitlement() {
    let url = server_url();
    let settings = Arc::new(RwLock::new(base_settings(&url)));
    let (service, model) = harness(settings.clone());

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时间异常")
        .as_secs();
    let email = format!("no-entitlement-{stamp}@example.test");
    let password = "walkthrough-pw-123";
    service
        .register(&email, password, None)
        .await
        .expect("注册应当成功");
    service.login(&email, password).await.expect("登录应当成功");

    let err = model
        .complete(
            &one_shot_request("用一句话说明什么是错题本。"),
            &AbortSignal::new(),
        )
        .await
        .expect_err("新账号没有权益，必须被平台拒绝");
    println!("无权益调用被拒：{err:?}");
    assert!(
        matches!(err, ModelError::QuotaExceeded(_)),
        "应当是 402 配额拒绝（no_entitlement），实际：{err:?}"
    );
}

/// 额度卡片的数据源：客户端能把服务端的「三滑动窗口」契约解析出来
/// （ADR-0047 修订 R13 + ADR-0048 修订 R10）。
#[tokio::test]
#[ignore]
async fn walkthrough_quota_view_has_three_windows() {
    let url = server_url();
    let email = std::env::var("PLATFORM_DEMO_EMAIL").unwrap_or_else(|_| "demo@example.test".into());
    let password =
        std::env::var("PLATFORM_DEMO_PASSWORD").unwrap_or_else(|_| "demo-password-123".into());

    std::fs::create_dir_all(Settings::data_root()).expect("建隔离数据根失败");
    let settings = Arc::new(RwLock::new(base_settings(&url)));
    let (service, _model) = harness(settings);

    service
        .login(&email, &password)
        .await
        .unwrap_or_else(|e| panic!("demo 账号登录失败：{e:?}"));

    let view = service.quota().await.expect("查询额度失败");
    println!("额度视图：{view}");
    assert!(view.get("reason").is_none(), "不该是错误态：{view}");
    assert_eq!(view["has_entitlement"], true, "demo 账号应当有权益：{view}");

    let windows = view["windows"].as_array().expect("windows 应当是数组");
    assert_eq!(windows.len(), 3, "应当是三个滑动窗口：{view}");
    for w in windows {
        assert!(w["key"].is_string(), "窗口要有 key：{w}");
        assert!(w["used"].is_number(), "窗口要有 used：{w}");
        assert!(w["limit"].is_number(), "月卡三个窗口都该有上限：{w}");
        assert!(w["remaining"].is_number(), "窗口要有 remaining：{w}");
    }
}
