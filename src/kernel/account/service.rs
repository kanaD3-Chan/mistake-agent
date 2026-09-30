//! 账号服务（ADR-0048）：**唯一**读写 `settings.account` 的地方。
//!
//! RPC 分支只解析参数，真正的顺序——请求服务端 → 落 settings → 刷新模型链路 → 发事件 → 记审计
//! ——全在这里，保证「令牌变了但模型服务还指着老地址」这种半截状态不可能出现。

use std::sync::Arc;

use serde_json::{Value, json};

use crate::kernel::agent::session::{Interrupt, InterruptBus};
use crate::kernel::audit::{AuditRecord, Auditor};
use crate::kernel::events::{Event, EventSink};
use crate::kernel::plugin::model::LiveSettingsModelService;
use crate::kernel::settings::Settings;

use super::client::{AccountClient, UserDto};
use super::error::AccountError;

pub struct AccountService {
    settings: Arc<std::sync::RwLock<Settings>>,
    /// 登录/登出后必须 `refresh()`：令牌一变，模型链路指向的地址与密钥就变了。
    main_service: Arc<LiveSettingsModelService>,
    interrupt_bus: InterruptBus,
    auditor: Auditor,
    events: Arc<dyn EventSink>,
    client: AccountClient,
}

impl AccountService {
    pub fn new(
        settings: Arc<std::sync::RwLock<Settings>>,
        main_service: Arc<LiveSettingsModelService>,
        interrupt_bus: InterruptBus,
        auditor: Auditor,
        events: Arc<dyn EventSink>,
    ) -> Result<Self, String> {
        Ok(Self {
            settings,
            main_service,
            interrupt_bus,
            auditor,
            events,
            client: AccountClient::new().map_err(|e| e.to_string())?,
        })
    }

    /// 注册（201）。**不自动登录**：服务端不签发令牌，登录是独立的下一步。
    pub async fn register(
        &self,
        email: &str,
        password: &str,
        display_name: Option<&str>,
    ) -> Result<Value, AccountError> {
        let email = email.trim();
        // 只挡"必然失败"的空值，长度/格式一律让服务端说——它的 message 已是中文文案
        // （docs/server-api.md §1），客户端再抄一遍规则只会两边不一致。
        if email.is_empty() || password.is_empty() {
            return Err(AccountError::InvalidInput("邮箱与口令都不能为空".into()));
        }
        let display_name = display_name.map(str::trim).filter(|s| !s.is_empty());
        let user = self
            .client
            .register(&self.server_url(), email, password, display_name)
            .await?;
        self.auditor.record(AuditRecord::AccountRegistered {
            email: user.email.clone(),
        });
        log::info!("平台账号已注册：{}", user.email);
        Ok(json!({
            "email": user.email,
            "display_name": user.display_name,
        }))
    }

    /// 登录：拿到令牌 → 落 settings → 模型链路切到平台 → 广播账号变化。
    pub async fn login(&self, email: &str, password: &str) -> Result<Value, AccountError> {
        let email = email.trim();
        if email.is_empty() || password.is_empty() {
            return Err(AccountError::InvalidInput("邮箱与口令都不能为空".into()));
        }
        let dto = self
            .client
            .login(&self.server_url(), email, password)
            .await?;
        let view = self.store_identity(&dto.token, &dto.user)?;
        self.announce(&dto.user.email, true);
        self.auditor.record(AuditRecord::AccountLoggedIn {
            email: dto.user.email.clone(),
        });
        // 只记邮箱，**不记令牌**（AGENTS.md：审计默认全覆盖，敏感值脱敏）。
        log::info!("平台账号已登录：{}", dto.user.email);
        Ok(view)
    }

    /// 登出：撤销服务端令牌 → 清本地身份 → 模型链路切回自备 Key。
    pub async fn logout(&self) -> Result<Value, AccountError> {
        let (server_url, token) = self.snapshot();
        if token.is_empty() {
            // 本来就没登录：幂等返回当前视图，不报错。
            return Ok(self.account_view());
        }
        // 服务端撤销失败（断网 / 令牌已过期）**不阻塞本地退出**：学生要的是"现在别再用这个账号"，
        // 本地清干净才是目的；服务端那条令牌到期自然失效。
        if let Err(e) = self.client.logout(&server_url, &token).await {
            log::warn!("服务端登出未成功，本地仍然退出：{e}");
        }
        let view = self.clear_identity()?;
        self.announce("", false);
        self.auditor.record(AuditRecord::AccountLoggedOut);
        log::info!("平台账号已退出登录");
        Ok(view)
    }

    /// 账号状态。`revalidate` 为真时打一次 `GET /me` 校令牌，并把身份字段刷成服务端当前值。
    ///
    /// **只有服务端明确说令牌无效才清令牌**；断网/读不懂响应一律只回 `reason`，本地不动
    /// ——一次网络抖动不该把学生踢下线（`AccountError::invalidates_token`）。
    pub async fn status(&self, revalidate: bool) -> Result<Value, AccountError> {
        let (server_url, token) = self.snapshot();
        if !revalidate || token.is_empty() {
            return Ok(self.account_view());
        }
        match self.client.me(&server_url, &token).await {
            Ok(user) => self.store_identity_fields(&user),
            Err(e) if e.invalidates_token() => {
                let reason = if matches!(
                    &e,
                    AccountError::Rejected { code, .. } if code.as_str() == "account_disabled"
                ) {
                    "account_disabled"
                } else {
                    "token_invalid"
                };
                let mut view = self.clear_identity()?;
                self.announce("", false);
                log::warn!("平台令牌已失效（{reason}），本地令牌已清除");
                view["reason"] = json!(reason);
                Ok(view)
            }
            Err(e) => {
                let reason = if matches!(&e, AccountError::Unreachable(_)) {
                    "unreachable"
                } else {
                    "server_error"
                };
                log::warn!("平台账号状态未校验（{reason}）：{e}");
                let mut view = self.account_view();
                view["reason"] = json!(reason);
                Ok(view)
            }
        }
    }

    fn server_url(&self) -> String {
        self.settings
            .read()
            .expect("settings poisoned")
            .account
            .server_url
            .clone()
    }

    fn snapshot(&self) -> (String, String) {
        let s = self.settings.read().expect("settings poisoned");
        (s.account.server_url.clone(), s.account.token.clone())
    }

    fn account_view(&self) -> Value {
        self.settings
            .read()
            .expect("settings poisoned")
            .public_view()["account"]
            .clone()
    }

    /// 写入登录身份并落盘，返回更新后的账号公开视图（**不含令牌**）。
    fn store_identity(&self, token: &str, user: &UserDto) -> Result<Value, AccountError> {
        let mut s = self.settings.write().expect("settings poisoned");
        s.account.token = token.to_string();
        s.account.email = user.email.clone();
        s.account.role = user.role.clone();
        s.account.sync_enabled = user.sync_enabled;
        s.save().map_err(AccountError::Local)?;
        Ok(s.public_view()["account"].clone())
    }

    /// 只刷新身份字段（令牌不动，故无需重建模型服务）。
    fn store_identity_fields(&self, user: &UserDto) -> Result<Value, AccountError> {
        let mut s = self.settings.write().expect("settings poisoned");
        s.account.email = user.email.clone();
        s.account.role = user.role.clone();
        s.account.sync_enabled = user.sync_enabled;
        s.save().map_err(AccountError::Local)?;
        Ok(s.public_view()["account"].clone())
    }

    /// 清空登录身份。`server_url` **保留**——退出后地址还是原来那个，不必重填。
    fn clear_identity(&self) -> Result<Value, AccountError> {
        let mut s = self.settings.write().expect("settings poisoned");
        s.account.token.clear();
        s.account.email.clear();
        s.account.role.clear();
        s.account.sync_enabled = false;
        s.save().map_err(AccountError::Local)?;
        Ok(s.public_view()["account"].clone())
    }

    /// 账号状态变了之后的固定三连：模型链路热更新、打断在飞回合、广播给 GUI。
    fn announce(&self, email: &str, logged_in: bool) {
        self.main_service.refresh();
        self.interrupt_bus.send(Interrupt::ConfigChanged);
        self.events.emit(Event::AccountChanged {
            logged_in,
            email: email.to_string(),
        });
    }
}
