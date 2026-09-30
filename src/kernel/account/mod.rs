//! 平台账号（ADR-0048）：注册 / 登录 / 登出 / 账号状态，以及令牌驱动的模型链路切换。
//!
//! - [`client`]：HTTP 请求与响应解析，无状态，不碰 settings；
//! - [`error`]：错误定型与分流（被拒 / 连不上 / 读不懂 / 入参不合法）；
//! - [`service`]：**唯一**读写 `settings.account` 的地方，RPC 只在这里落地。
//!
//! 令牌是**不透明字符串**（`mka_` + 64 位十六进制）：客户端不解码、不自己算过期，
//! 有效期由服务端说了算，只有服务端明确回 `invalid_token`/`account_disabled` 才清本地令牌
//! （见 [`AccountError::invalidates_token`]）。

mod client;
mod error;
mod service;

#[cfg(test)]
mod tests;

pub use error::AccountError;
pub use service::AccountService;
