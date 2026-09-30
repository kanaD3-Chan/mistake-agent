//! 平台服务 HTTP 客户端（ADR-0048）：只做请求与响应解析，**不碰 settings**。
//!
//! 令牌的读写、审计、事件一律在 [`super::service::AccountService`]，这里保持无状态，
//! 才能被单测直接换掉（见 tests.rs 的 URL 拼接与错误分流）。

use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};

use super::error::AccountError;

/// 账号信息（服务端 `user` 对象里客户端用得上的字段；`id`/`created_at` 不解析）。
#[derive(Debug, Clone, Deserialize)]
pub struct UserDto {
    pub email: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub sync_enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct UserEnvelope {
    user: UserDto,
}

/// 登录响应：令牌明文**只在这里出现一次**，服务端只存 SHA-256，丢了只能重新登录。
#[derive(Debug, Clone, Deserialize)]
pub struct LoginDto {
    pub token: String,
    pub user: UserDto,
}

/// 拼服务端 URL：地址已在 `settings::normalize_server_url` 收掉尾斜杠，这里再兜一次，
/// 免得旧 settings.json 里存着带斜杠的地址时拼出 `//api/v1/...`。
pub fn endpoint(server_url: &str, path: &str) -> String {
    format!("{}{}", server_url.trim().trim_end_matches('/'), path)
}

pub struct AccountClient {
    http: Client,
}

impl AccountClient {
    pub fn new() -> Result<Self, AccountError> {
        let http = Client::builder()
            .timeout(Duration::from_secs(20))
            // 与模型服务、余额查询一致（testing.md bug #4）：无 IPv6 环境下强制走 IPv4，
            // 否则解析到 v6 后连接立即失败。
            .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
            .build()
            .map_err(|e| AccountError::BadResponse(e.to_string()))?;
        Ok(Self { http })
    }

    /// 注册（201）。**不自动登录**——服务端不签发令牌，登录是下一步独立动作。
    pub async fn register(
        &self,
        server_url: &str,
        email: &str,
        password: &str,
        display_name: Option<&str>,
    ) -> Result<UserDto, AccountError> {
        let body = self
            .send(
                self.http
                    .post(endpoint(server_url, "/api/v1/auth/register"))
                    .json(&json!({
                        "email": email,
                        "password": password,
                        "display_name": display_name,
                    })),
            )
            .await?;
        Self::parse::<UserEnvelope>(&body).map(|e| e.user)
    }

    pub async fn login(
        &self,
        server_url: &str,
        email: &str,
        password: &str,
    ) -> Result<LoginDto, AccountError> {
        let body = self
            .send(
                self.http
                    .post(endpoint(server_url, "/api/v1/auth/login"))
                    .json(&json!({ "email": email, "password": password })),
            )
            .await?;
        Self::parse::<LoginDto>(&body)
    }

    /// 登出（204，无响应体）。只撤销当前这一个令牌。
    pub async fn logout(&self, server_url: &str, token: &str) -> Result<(), AccountError> {
        self.send(
            self.http
                .post(endpoint(server_url, "/api/v1/auth/logout"))
                .bearer_auth(token),
        )
        .await
        .map(|_| ())
    }

    pub async fn me(&self, server_url: &str, token: &str) -> Result<UserDto, AccountError> {
        let body = self
            .send(
                self.http
                    .get(endpoint(server_url, "/api/v1/me"))
                    .bearer_auth(token),
            )
            .await?;
        Self::parse::<UserEnvelope>(&body).map(|e| e.user)
    }

    /// 平台额度视图（ADR-0047 修订 R13）：`GET /api/v1/me/quota`。
    ///
    /// **原样透传服务端的 JSON**（契约见 docs/server-api.md §2.6）：窗口数量与字段由服务端
    /// 说了算，客户端不在这里重新建模——否则服务端加一个窗口，客户端就得跟着发版。
    pub async fn quota(&self, server_url: &str, token: &str) -> Result<Value, AccountError> {
        let body = self
            .send(
                self.http
                    .get(endpoint(server_url, "/api/v1/me/quota"))
                    .bearer_auth(token),
            )
            .await?;
        Self::parse::<Value>(&body)
    }

    /// 发请求并把 4xx/5xx 定型成错误；成功则交出响应体原文。
    async fn send(&self, req: reqwest::RequestBuilder) -> Result<String, AccountError> {
        let resp = req.send().await.map_err(AccountError::from_reqwest)?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(AccountError::from_reqwest)?;
        if status >= 400 {
            return Err(AccountError::from_status(status, &body));
        }
        Ok(body)
    }

    fn parse<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, AccountError> {
        serde_json::from_str(body).map_err(|e| AccountError::BadResponse(e.to_string()))
    }
}
