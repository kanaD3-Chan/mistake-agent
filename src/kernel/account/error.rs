//! 账号错误的定型与分流（ADR-0048）。
//!
//! 服务端统一错误体是 `{"error":{"code":"...","message":"..."}}`，`message` 已经是
//! 面向用户的中文文案，客户端**不再自己编文案**（docs/server-api.md §1）。
//! 这里只做一件事：把 HTTP 层的三种失败——被拒、连不上、读不懂——分开定型，
//! 让上层能据此决定「清不清本地令牌」。

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Debug, Clone, Deserialize)]
struct ErrorDetail {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

/// 账号链路错误。
///
/// **`Rejected` 与 `Unreachable` 必须分开**：前者是服务端明确表态（令牌真的失效了），
/// 后者只是一次网络抖动。把后者当令牌失效处理，会把断网的学生直接踢下线。
#[derive(Debug, Clone)]
pub enum AccountError {
    /// 服务端明确拒绝，`code` 直接透出来给前端分流（`invalid_credentials` / `email_taken` / …）。
    Rejected { code: String, message: String },
    /// 连不上 / 超时 / DNS 失败：网络层问题，**不代表令牌失效**。
    Unreachable(String),
    /// 连上了但响应读不懂（代理返回 HTML、服务端 500 空体等）。
    BadResponse(String),
    /// 入参在本地就必然过不去（空邮箱、空口令）——省一次注定失败的往返。
    InvalidInput(String),
    /// 本地写 settings.json 失败（磁盘满、权限）。
    Local(String),
}

impl AccountError {
    /// 给前端分流的错误码：服务端的 code 原样透出，本地失败用固定前缀。
    pub fn code(&self) -> &str {
        match self {
            Self::Rejected { code, .. } => code,
            Self::Unreachable(_) => "network",
            Self::BadResponse(_) => "bad_response",
            Self::InvalidInput(_) => "invalid_params",
            Self::Local(_) => "save_failed",
        }
    }

    /// 这个错误是否意味着「本地该把令牌清掉」。
    ///
    /// 只有服务端明确说令牌无效/账号停用才算；`Unreachable` / `BadResponse` 一律不算
    /// ——断网时清令牌等于把学生踢下线，而他其实什么都没做错。
    pub fn invalidates_token(&self) -> bool {
        matches!(
            self,
            Self::Rejected { code, .. }
                if matches!(code.as_str(), "invalid_token" | "missing_token" | "account_disabled")
        )
    }

    /// 解析服务端错误体；读不懂时按状态码兜底定型。
    pub fn from_status(status: u16, body: &str) -> Self {
        if let Ok(parsed) = serde_json::from_str::<ErrorBody>(body)
            && !parsed.error.code.is_empty()
        {
            return Self::Rejected {
                code: parsed.error.code,
                message: parsed.error.message,
            };
        }
        // 兜底也走 Rejected：状态码是服务端给的，比"读不懂响应"更可能是真相。
        Self::Rejected {
            code: format!("http_{status}"),
            message: format!("平台服务返回 HTTP {status}"),
        }
    }

    pub fn from_reqwest(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            return Self::Unreachable("请求超时".into());
        }
        if e.is_connect() {
            return Self::Unreachable("无法建立连接".into());
        }
        Self::Unreachable(e.to_string())
    }
}

impl std::fmt::Display for AccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected { code, message } => {
                if message.trim().is_empty() {
                    write!(f, "平台服务返回 {code}")
                } else {
                    write!(f, "{message}")
                }
            }
            // 这两条要把"该怎么办"说清楚：学生看到的下一步动作是改地址或先跳过。
            Self::Unreachable(detail) => write!(
                f,
                "连不上平台服务（{detail}）；可在下方修改服务地址，或先用本地模式"
            ),
            Self::BadResponse(detail) => {
                write!(f, "平台服务返回了无法识别的内容（{detail}）")
            }
            Self::InvalidInput(detail) => write!(f, "{detail}"),
            Self::Local(detail) => write!(f, "本地设置保存失败：{detail}"),
        }
    }
}

impl std::error::Error for AccountError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_error_body() {
        let e = AccountError::from_status(
            401,
            r#"{"error":{"code":"invalid_credentials","message":"邮箱或口令不正确"}}"#,
        );
        assert_eq!(e.code(), "invalid_credentials");
        assert_eq!(e.to_string(), "邮箱或口令不正确");
        assert!(!e.invalidates_token(), "登录失败不代表已登录的令牌失效");
    }

    #[test]
    fn token_error_invalidates() {
        for code in ["invalid_token", "missing_token", "account_disabled"] {
            let body = format!(r#"{{"error":{{"code":"{code}","message":"x"}}}}"#);
            let e = AccountError::from_status(401, &body);
            assert!(e.invalidates_token(), "{code} 应判为令牌失效");
        }
    }

    #[test]
    fn network_never_invalidates_token() {
        assert!(!AccountError::Unreachable("超时".into()).invalidates_token());
        assert!(!AccountError::BadResponse("html".into()).invalidates_token());
        assert_eq!(AccountError::Unreachable("x".into()).code(), "network");
    }

    #[test]
    fn unreadable_body_falls_back_to_status_code() {
        let e = AccountError::from_status(502, "<html>Bad Gateway</html>");
        assert_eq!(e.code(), "http_502");
        assert!(!e.invalidates_token());

        // 空 code 的合法 JSON 也走兜底，不能产出一个空错误码。
        let e = AccountError::from_status(500, r#"{"error":{"code":"","message":"炸了"}}"#);
        assert_eq!(e.code(), "http_500");
    }
}
