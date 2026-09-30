//! 提取器：Bearer 鉴权（[`AuthUser`]）与管理员守卫（[`RequireAdmin`]）。
//!
//! 做成提取器而不是中间件：axum 的提取器把"这个处理函数需要登录"写在**函数签名**上，
//! 漏写就编译不过；中间件方案则是"忘了挂就等于没鉴权"，失败方向相反。

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;

use super::error::AuthError;
use super::model::AuthUser;
use super::{store, token};
use crate::http::AppState;

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AuthError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let plain = platform_token(parts).ok_or(AuthError::MissingToken)?;
        // 形状预检：明显不是我们签发的令牌不查库
        if !token::looks_valid(plain) {
            return Err(AuthError::InvalidToken);
        }
        let hash = token::hash_of(plain);
        store::authenticate_token(&state.pool, &hash)
            .await?
            .ok_or(AuthError::InvalidToken)
    }
}

/// 管理员守卫：在鉴权之上再要求 `admin` 角色（`teacher` 首期无任何特权）。
pub struct RequireAdmin(pub AuthUser);

impl FromRequestParts<AppState> for RequireAdmin {
    type Rejection = AuthError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if !user.is_admin() {
            tracing::warn!(user_id = %user.user.id, role = %user.user.role.as_str(), "非管理员访问管理接口，已拒绝");
            return Err(AuthError::Forbidden);
        }
        Ok(RequireAdmin(user))
    }
}

/// 取平台令牌，两种头都认：
/// - `Authorization: Bearer <token>`——OpenAI 系客户端与本项目客户端；
/// - `x-api-key: <token>`——Anthropic 系客户端（Claude Code 等）。中转面必须认它，
///   否则这类客户端根本接不进来（ADR-0047 修订 R1）。
///
/// 两者是同一个 bearer 秘密，账号面顺带也认后者，不构成额外攻击面。
fn platform_token(parts: &Parts) -> Option<&str> {
    if let Some(raw) = parts
        .headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(token) = parse_bearer(raw)
    {
        return Some(token);
    }
    let raw = parts.headers.get("x-api-key")?.to_str().ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// 解析 `Authorization: Bearer <token>`。方案名大小写不敏感（RFC 7235）。
fn parse_bearer(raw: &str) -> Option<&str> {
    let (scheme, value) = raw.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    #[test]
    fn parses_bearer_header_case_insensitively() {
        assert_eq!(parse_bearer("Bearer mka_abc"), Some("mka_abc"));
        assert_eq!(parse_bearer("bearer mka_abc"), Some("mka_abc"));
        assert_eq!(parse_bearer("BEARER   mka_abc  "), Some("mka_abc"));
    }

    #[test]
    fn accepts_anthropic_style_x_api_key() {
        // Claude Code 这类客户端只发 x-api-key（ADR-0047 修订 R1）
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "mka_from_anthropic_client".parse().unwrap());
        let parts = request_parts(headers);
        assert_eq!(platform_token(&parts), Some("mka_from_anthropic_client"));

        // Authorization 优先于 x-api-key（两者都在时以标准头为准）
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer mka_standard".parse().unwrap());
        headers.insert("x-api-key", "mka_other".parse().unwrap());
        let parts = request_parts(headers);
        assert_eq!(platform_token(&parts), Some("mka_standard"));
    }

    #[test]
    fn empty_or_missing_credentials_yield_none() {
        let parts = request_parts(HeaderMap::new());
        assert_eq!(platform_token(&parts), None);

        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "   ".parse().unwrap());
        let parts = request_parts(headers);
        assert_eq!(platform_token(&parts), None);
    }

    /// 构造只含请求头的 `Parts`（提取器只读头，不需要真的请求体）。
    fn request_parts(headers: HeaderMap) -> Parts {
        let mut request = axum::http::Request::new(());
        *request.headers_mut() = headers;
        let (parts, ()) = request.into_parts();
        parts
    }

    #[test]
    fn rejects_other_schemes_and_malformed_headers() {
        for bad in [
            "",
            "mka_abc",
            "Basic dXNlcjpwYXNz",
            "Bearer",
            "Bearer ",
            "Token mka_abc",
        ] {
            assert_eq!(parse_bearer(bad), None, "{bad} 不应被当作 Bearer 令牌");
        }
    }
}
