//! 账号、令牌与鉴权（ADR-0047 决策 3/4、ADR-0048）。
//!
//! 职责切分：
//! - `model`    —— 领域类型（`User` / `Role` / `AuthUser`）与输入校验
//! - `password` —— Argon2id 口令哈希与校验
//! - `token`    —— 不透明令牌的签发、形状预检与 SHA-256 摘要
//! - `store`    —— 数据库读写（用户、令牌、管理员种子）
//! - `error`    —— 错误类型与其 HTTP 呈现
//! - `extract`  —— 提取器：`AuthUser`（Bearer 鉴权）、`RequireAdmin`（角色守卫）
//! - `handlers` —— HTTP 处理函数与 DTO
//!
//! 对外只暴露：`router()`、`AuthUser`、`RequireAdmin`、`Role`、`bootstrap_admin()`，
//! 以及管理面要用的 `list_users` 桥接。

mod error;
mod extract;
mod handlers;
mod model;
mod password;
mod store;
mod token;

pub use error::AuthError;
pub use extract::RequireAdmin;
pub use model::{AuthUser, Role, User};

/// 管理面（`admin`）经父模块桥接取用：账号表的读取留在 auth 内，避免两处各写一份 SQL。
pub(crate) use store::list_users;

use axum::Router;
use axum::routing::{get, post};
use sqlx::PgPool;

use crate::config::Config;
use crate::http::AppState;

/// `/api/v1` 下的账号面。
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/register", post(handlers::register))
        .route("/api/v1/auth/login", post(handlers::login))
        .route("/api/v1/auth/logout", post(handlers::logout))
        .route("/api/v1/me", get(handlers::me).patch(handlers::update_me))
        // 额度视图与账号资料分开：一次查询只做一件事，也让客户端能只读数不读资料
        .route("/api/v1/me/quota", get(handlers::quota))
}

/// 启动时创建首个管理员（ADR-0047 决策 3）：`ADMIN_EMAIL` + `ADMIN_PASSWORD` 配齐
/// 且库中尚无管理员时创建，幂等；未配置则什么也不做。失败会让启动失败——宁可起不来，
/// 也不要运维以为种子生效了其实没有。
pub async fn bootstrap_admin(pool: &PgPool, config: &Config) -> Result<(), String> {
    let (Some(email), Some(password)) = (&config.admin_email, &config.admin_password) else {
        return Ok(());
    };
    store::ensure_seed_admin(pool, email, password)
        .await
        .map_err(|e| format!("管理员种子创建失败：{e}"))?;
    Ok(())
}
