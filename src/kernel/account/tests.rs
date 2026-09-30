//! 账号模块单测：**全部离线**。
//!
//! 覆盖的都是不打网络、不落盘的路径：`endpoint` 拼接、空入参拦截、未登录时的
//! `logout` 幂等、`status(false)` 的纯本地读。
//!
//! 登录/登出真正落盘那条链路会经 `Settings::save()` 写**真实数据根目录**
//! （`~/Documents/.mistake-agent/settings.json`）——单测里跑一遍就把用户配置覆盖了，
//! 所以那条链只在真机走查里验证（docs/testing.md §5），这里不碰。

use std::sync::{Arc, RwLock};

use super::client::endpoint;
use super::service::AccountService;

use crate::kernel::agent::session::InterruptBus;
use crate::kernel::audit::{Auditor, MemoryAuditSink};
use crate::kernel::events::MemoryEventSink;
use crate::kernel::plugin::model::LiveSettingsModelService;
use crate::kernel::settings::{
    AccountConfig, DEFAULT_SERVER_URL, ModelConfig, Settings, Transport,
};

struct Harness {
    service: AccountService,
    events: Arc<MemoryEventSink>,
    audits: Arc<MemoryAuditSink>,
}

/// 令牌非空即"已登录"（`AccountConfig::logged_in`），这里据此造两种初始状态。
fn account(token: &str) -> AccountConfig {
    AccountConfig {
        server_url: DEFAULT_SERVER_URL.into(),
        token: token.into(),
        email: if token.is_empty() {
            String::new()
        } else {
            "demo@example.test".into()
        },
        role: if token.is_empty() {
            String::new()
        } else {
            "user".into()
        },
        sync_enabled: false,
    }
}

fn harness(account: AccountConfig) -> Harness {
    let settings = Arc::new(RwLock::new(Settings {
        log_level: crate::kernel::logger::Level::Info,
        english_mode: false,
        nickname: "小明".into(),
        main_model: ModelConfig {
            api_url: "https://api.deepseek.com".into(),
            api_key: "sk-secret".into(),
            model: Some("deepseek-flash".into()),
            transport: Some(Transport::Responses),
        },
        vision_model: ModelConfig {
            api_url: String::new(),
            api_key: String::new(),
            model: None,
            transport: None,
        },
        account,
    }));
    let events = Arc::new(MemoryEventSink::default());
    let audits = Arc::new(MemoryAuditSink::default());
    let service = AccountService::new(
        settings.clone(),
        Arc::new(LiveSettingsModelService::new(settings)),
        InterruptBus::new(),
        Auditor::new(audits.clone()),
        events.clone(),
    )
    .expect("账号服务构造不应失败");
    Harness {
        service,
        events,
        audits,
    }
}

#[test]
fn endpoint_joins_without_double_slash() {
    assert_eq!(
        endpoint("https://api.example.com", "/api/v1/auth/login"),
        "https://api.example.com/api/v1/auth/login"
    );
    // 旧 settings.json 里可能存着带尾斜杠的地址（归一化是后加的），兜住。
    assert_eq!(
        endpoint("https://api.example.com/", "/api/v1/me"),
        "https://api.example.com/api/v1/me"
    );
    assert_eq!(
        endpoint("http://127.0.0.1:8080///", "/api/v1/me"),
        "http://127.0.0.1:8080/api/v1/me"
    );
}

/// 空邮箱/空口令在本地就挡掉：不打网络、不落盘、不留审计——省一次注定失败的往返。
#[tokio::test]
async fn empty_credentials_are_rejected_locally() {
    let h = harness(account(""));

    for result in [
        h.service.login("", "pw123456").await,
        h.service.login("   ", "pw123456").await,
        h.service.login("a@b.com", "").await,
        h.service.register("", "pw123456", None).await,
        h.service.register("a@b.com", "", None).await,
    ] {
        let err = result.expect_err("空入参必须被本地拦下");
        assert_eq!(err.code(), "invalid_params");
    }

    assert!(h.audits.take().is_empty(), "被拦下的请求不该留审计");
    assert!(h.events.take().is_empty(), "被拦下的请求不该发事件");
}

/// 本来就没登录时登出是幂等的：不动服务端、不报错、也不留审计（什么都没发生）。
#[tokio::test]
async fn logout_without_token_is_idempotent() {
    let h = harness(account(""));

    let view = h.service.logout().await.expect("未登录登出不应报错");
    assert_eq!(view["logged_in"], false);
    assert_eq!(view["server_url"], DEFAULT_SERVER_URL);
    assert!(h.audits.take().is_empty());
    assert!(h.events.take().is_empty());
}

/// **安全红线**：账号公开视图里绝不能出现令牌明文。
#[tokio::test]
async fn account_view_never_exposes_token() {
    let h = harness(account("mka_deadbeef"));

    let view = h.service.status(false).await.expect("纯本地读不应失败");
    assert_eq!(view["logged_in"], true);
    assert_eq!(view["email"], "demo@example.test");
    assert_eq!(view["role"], "user");
    assert!(
        !view.to_string().contains("mka_deadbeef"),
        "公开视图里泄漏了令牌：{view}"
    );
    // 不 revalidate 就不打网络，也就没有任何事件要发。
    assert!(h.events.take().is_empty());
}

/// 服务端地址改过之后，`status(false)` 必须回**当前**地址而不是默认值。
#[tokio::test]
async fn status_reports_current_server_url() {
    let mut cfg = account("");
    cfg.server_url = "https://platform.example.com".into();
    let h = harness(cfg);

    let view = h.service.status(false).await.expect("纯本地读不应失败");
    assert_eq!(view["server_url"], "https://platform.example.com");
    assert_eq!(view["logged_in"], false);
}
