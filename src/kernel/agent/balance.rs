//! 余额查询（账户外部 API 轻量封装，2026-08-04 实测通过；ADR-0045 收敛为 DeepSeek 单账户）：
//! - DeepSeek：`GET {base}/user/balance`，返回 `balance_infos`（可能多币种，取第一条）。
//!
//! 只读账户信息，绝不把 api_key 放进返回值或审计；未配置时给出结构化占位。

use std::time::Duration;

use reqwest::Client;
use serde::Serialize;
use serde_json::{Value, json};

use crate::kernel::settings::Settings;

#[derive(Debug, Clone, Serialize)]
pub struct BalanceReport {
    /// DeepSeek（单模型）。
    pub main: ProviderBalance,
    /// 登录平台账号时为 `true`：`main` 是**平台模式占位**，绝不来自自备 Key
    /// （ADR-0048 修订 R10）。窗口用量的百分比待 `GET /api/v1/me/quota` 落地后填充。
    pub platform: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderBalance {
    /// 该服务是否已配置 api_key。
    pub configured: bool,
    /// 查询是否成功；未配置时 ok=false 且 error=None（前端按 configured 展示）。
    pub ok: bool,
    pub error: Option<String>,
    /// 提炼后的展示字段（各提供商结构不同，字段名保持原文方便前端直接取用）。
    pub data: Option<Value>,
}

/// 查询 DeepSeek 余额（一次真实 HTTP 请求；5xx 临时错误重试一次）。
pub async fn check_balance(settings: &Settings) -> BalanceReport {
    // ADR-0048 修订 R10：登录平台账号后**一律不用自备 Key 查余额**。
    //
    // 这条 return 刻意放在函数最前面（连 HTTP client 都不构建），是**结构性保证**：
    // 下面的代码根本不会执行，自备 key 也就没有任何机会离开进程。
    // 之前这里直接读 `${settings.main_model}`，于是"登录后忽略自备 Key"这条承诺
    // 在余额这条路径上是落空的——登录用户看到的是自己的余额，与实际由平台结算相矛盾。
    if settings.account.logged_in() {
        return BalanceReport {
            platform: true,
            main: ProviderBalance {
                configured: false,
                ok: false,
                error: None,
                data: None,
            },
        };
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        // 与模型服务一致：无 IPv6 环境下强制走 IPv4，避免解析到 v6 后连接立即失败。
        .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
        .build()
        .expect("reqwest client 构建失败");

    BalanceReport {
        platform: false,
        main: provider_balance(
            &client,
            &settings.main_model,
            deepseek_url(&settings.main_model.api_url),
            deepseek_display,
        )
        .await,
    }
}

async fn provider_balance(
    client: &Client,
    cfg: &crate::kernel::settings::ModelConfig,
    url: String,
    display: fn(&Value) -> Value,
) -> ProviderBalance {
    let key = cfg.api_key.trim();
    if key.is_empty() {
        return ProviderBalance {
            configured: false,
            ok: false,
            error: None,
            data: None,
        };
    }
    match fetch_json(client, &url, key).await {
        Ok(raw) => ProviderBalance {
            configured: true,
            ok: true,
            error: None,
            data: Some(display(&raw)),
        },
        Err(e) => ProviderBalance {
            configured: true,
            ok: false,
            error: Some(e),
            data: None,
        },
    }
}

/// GET + Bearer 鉴权；5xx（含 503）重试一次，其余错误直接返回。
async fn fetch_json(client: &Client, url: &str, key: &str) -> Result<Value, String> {
    let mut last_error: Option<String> = None;
    for attempt in 0..2 {
        let resp = client
            .get(url)
            .bearer_auth(key)
            .send()
            .await
            .map_err(|e| format!("网络请求失败：{e}"));
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                last_error = Some(e);
                tokio::time::sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
                continue;
            }
        };
        let status = resp.status();
        if status.is_success() {
            return resp
                .json::<Value>()
                .await
                .map_err(|e| format!("响应解析失败：{e}"));
        }
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(160)
            .collect::<String>();
        last_error = Some(format!("HTTP {}：{}", status.as_u16(), body));
        if status.is_server_error() {
            tokio::time::sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
            continue;
        }
        break;
    }
    Err(last_error.unwrap_or_else(|| "余额查询失败".into()))
}

/// DeepSeek 余额端点：base 与模型端点同源；兼容带 /v1 的写法，取根路径。
fn deepseek_url(api_url: &str) -> String {
    let base = api_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    format!("{base}/user/balance")
}

/// 提炼 DeepSeek balance_infos（多币种取第一条）。
fn deepseek_display(raw: &Value) -> Value {
    let info = raw
        .get("balance_infos")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .cloned()
        .unwrap_or_default();
    json!({
        "is_available": raw.get("is_available").cloned().unwrap_or(Value::Null),
        "currency": info.get("currency").cloned().unwrap_or(Value::Null),
        "total_balance": info.get("total_balance").cloned().unwrap_or(Value::Null),
        "granted_balance": info.get("granted_balance").cloned().unwrap_or(Value::Null),
        "topped_up_balance": info.get("topped_up_balance").cloned().unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::settings::{AccountConfig, ModelConfig};

    /// 造配置：`token` 非空即"已登录"（`AccountConfig::logged_in`）。
    /// `main_url` 传一个必然连不上的地址，用来证明"到底有没有去查自备余额"。
    fn settings(token: &str, main_key: &str, main_url: &str) -> Settings {
        let logged_in = !token.is_empty();
        Settings {
            log_level: crate::kernel::logger::Level::Info,
            english_mode: false,
            nickname: "走查".into(),
            main_model: ModelConfig {
                api_url: main_url.into(),
                api_key: main_key.into(),
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
                server_url: "http://127.0.0.1:8080".into(),
                token: token.into(),
                email: if logged_in {
                    "demo@example.test".into()
                } else {
                    String::new()
                },
                role: if logged_in {
                    "user".into()
                } else {
                    String::new()
                },
                sync_enabled: false,
            },
        }
    }

    /// ADR-0048 修订 R10：登录态**绝不**用自备 key 查余额。
    /// main_model 指向必然连不上的地址——若真去查了，这里必然 ok=false + error=Some。
    #[tokio::test]
    async fn logged_in_never_queries_self_key() {
        let s = settings("mka_token", "sk-self-secret", "http://127.0.0.1:1");
        let report = check_balance(&s).await;
        assert!(report.platform, "登录态必须走平台模式");
        assert!(
            report.main.error.is_none(),
            "登录态不得发起到自备余额的请求：{:?}",
            report.main.error
        );
        assert!(
            !format!("{report:?}").contains("sk-self-secret"),
            "自备 key 不得出现在报告里"
        );
    }

    /// 未登录时行为不变：仍然用自备 key（这里真的会去连，连不上即证明走了自备路径）。
    #[tokio::test]
    async fn logged_out_still_uses_self_key() {
        let s = settings("", "sk-self-secret", "http://127.0.0.1:1");
        let report = check_balance(&s).await;
        assert!(!report.platform, "未登录不得标记为平台模式");
        assert!(
            report.main.error.is_some(),
            "未登录应当真的去查自备余额（此处应连接失败）"
        );
    }
}
