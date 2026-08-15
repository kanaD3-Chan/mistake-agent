//! Dispatch（Q12）：统一执行入口——Caller 检查 → 懒注册 → 参数校验 → 超时/取消 → 审计。

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::kernel::agent::loop_mod::InterruptReason;
use crate::kernel::audit::{AuditRecord, Auditor};
use crate::kernel::contract::{CallerPolicy, ToolError};
use crate::kernel::events::EventSink;
use crate::kernel::logger::LoggerHandle;
use crate::kernel::plugin::services::AbortSignal;
use crate::kernel::registry::{Handler, RegisteredEntry, Registry};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub type ToolHandler = Arc<
    dyn for<'a> Fn(&'a ToolCallContext, Value) -> BoxFuture<'a, Result<Value, ToolError>>
        + Send
        + Sync,
>;

pub type CommandHandler = ToolHandler;
pub type EventHandler =
    Arc<dyn Fn(Value) -> BoxFuture<'static, Result<(), ToolError>> + Send + Sync>;
pub type EnglishModeProvider = Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Caller {
    Model,
    User,
}

/// 工具级截止时间（Q12 后门）：handler 可申请延期，受回合预算钳制。
pub struct DeadlineHandle {
    deadline: Arc<Mutex<Instant>>,
    turn_end: Instant,
}

impl DeadlineHandle {
    fn new(deadline: Arc<Mutex<Instant>>, turn_end: Instant) -> Self {
        Self { deadline, turn_end }
    }

    pub fn extend(&self, extra: Duration) -> bool {
        let mut dl = self.deadline.lock().expect("deadline poisoned");
        let proposed = Instant::now() + extra;
        if proposed > self.turn_end {
            return false;
        }
        *dl = proposed;
        true
    }
}

/// 内部中断控制（Q17c）：插件/服务可主动请求中断当前工具执行。
#[derive(Clone)]
pub struct TurnControl {
    cancel: CancellationToken,
    reason: Arc<Mutex<Option<InterruptReason>>>,
}

impl TurnControl {
    pub fn interrupt(&self, reason: InterruptReason) {
        *self.reason.lock().expect("reason poisoned") = Some(reason);
        self.cancel.cancel();
    }

    pub fn take_reason(&self) -> Option<InterruptReason> {
        self.reason.lock().expect("reason poisoned").take()
    }
}

pub struct ToolCallContext {
    pub signal: AbortSignal,
    pub deadline: DeadlineHandle,
    pub interrupt: TurnControl,
    pub logger: LoggerHandle,
    /// 英语沉浸模式开关：插件据此选择提示词语言。
    pub english_mode: bool,
    /// 进度播报通道（ToolProgress 等）。
    pub events: Arc<dyn EventSink>,
}

pub struct Dispatch {
    registry: Arc<Registry>,
    auditor: Auditor,
    default_tool_timeout: Duration,
    grace: Duration,
    turn_budget: Duration,
    events: Arc<dyn EventSink>,
    english_mode: EnglishModeProvider,
}

impl Dispatch {
    pub fn new(
        registry: Arc<Registry>,
        auditor: Auditor,
        default_tool_timeout: Duration,
        grace: Duration,
        turn_budget: Duration,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            registry,
            auditor,
            default_tool_timeout,
            grace,
            turn_budget,
            events,
            english_mode: Arc::new(|| false),
        }
    }

    pub fn with_english_mode(mut self, provider: EnglishModeProvider) -> Self {
        self.english_mode = provider;
        self
    }

    pub async fn call_tool(
        &self,
        full_name: &str,
        params: Value,
        caller: Caller,
    ) -> Result<Value, ToolError> {
        let entry = self
            .registry
            .ensure_tool(full_name)
            .map_err(|e| ToolError::internal(e.to_string()))?;
        self.check_policy(&entry, caller, full_name)?;
        self.validate(&entry, &params)?;
        let handler = match &entry.handler {
            Handler::Tool(h) => h.clone(),
            _ => return Err(ToolError::internal("入口点不是工具")),
        };
        let result = self.run(entry.clone(), handler, params).await;
        self.auditor.record(AuditRecord::EntryPointCall {
            entry: full_name.into(),
            caller,
            ok: result.is_ok(),
            error: result
                .as_ref()
                .err()
                .map(|e| format!("{:?}: {}", e.code, e.message)),
        });
        result
    }

    pub async fn call_command(&self, full_name: &str, params: Value) -> Result<Value, ToolError> {
        let entry = match self.registry.ensure_command(full_name) {
            Ok(e) => e,
            // 命令通道回退：找不到 Command 时放行同名 Tool（UserAndModel/UserOnly 用户必可调），
            // 让 GUI 能经 trigger_command 直查工具能力（如错题本 grading::list）。
            Err(_) => return self.call_tool(full_name, params, Caller::User).await,
        };
        self.validate(&entry, &params)?;
        let handler = match &entry.handler {
            Handler::Command(h) => h.clone(),
            _ => return Err(ToolError::internal("入口点不是命令")),
        };
        let result = self.run(entry.clone(), handler, params).await;
        self.auditor.record(AuditRecord::EntryPointCall {
            entry: full_name.into(),
            caller: Caller::User,
            ok: result.is_ok(),
            error: result
                .as_ref()
                .err()
                .map(|e| format!("{:?}: {}", e.code, e.message)),
        });
        result
    }

    /// wire name → 内部全名（Q7：模型调用的是 `namespace__tool`，`::` → `__`）。
    pub fn resolve_wire(&self, wire: &str) -> Option<String> {
        self.registry.resolve_wire(wire)
    }

    pub fn entry_icon(&self, full_name: &str) -> Option<String> {
        self.registry.entry_icon(full_name)
    }

    fn check_policy(
        &self,
        entry: &RegisteredEntry,
        caller: Caller,
        full_name: &str,
    ) -> Result<(), ToolError> {
        if caller == Caller::Model && entry.policy == CallerPolicy::UserOnly {
            self.auditor.record(AuditRecord::AccessDenied {
                entry: full_name.into(),
                caller,
            });
            return Err(ToolError::forbidden());
        }
        Ok(())
    }

    fn validate(&self, entry: &RegisteredEntry, params: &Value) -> Result<(), ToolError> {
        let schema = serde_json::to_value(&entry.params)
            .map_err(|e| ToolError::internal(format!("schema 序列化失败：{e}")))?;
        let validator = jsonschema::validator_for(&schema)
            .map_err(|e| ToolError::internal(format!("schema 无效：{e}")))?;
        validator
            .validate(params)
            .map_err(|e| ToolError::invalid_params(format!("参数校验失败：{e}")))
    }

    async fn run(
        &self,
        entry: RegisteredEntry,
        handler: ToolHandler,
        params: Value,
    ) -> Result<Value, ToolError> {
        let timeout = entry
            .timeout
            .unwrap_or(self.default_tool_timeout)
            .min(self.turn_budget);
        let deadline = Arc::new(Mutex::new(Instant::now() + timeout));
        let turn_end = Instant::now() + self.turn_budget;
        let cancel = CancellationToken::new();
        let reason = Arc::new(Mutex::new(None));
        let ctx = ToolCallContext {
            signal: AbortSignal::from_token(cancel.clone()),
            deadline: DeadlineHandle::new(deadline.clone(), turn_end),
            interrupt: TurnControl {
                cancel: cancel.clone(),
                reason: reason.clone(),
            },
            logger: self.registry.logger().clone(),
            english_mode: (self.english_mode)(),
            events: self.events.clone(),
        };
        let task: JoinHandle<Result<Value, ToolError>> =
            tokio::spawn(async move { handler(&ctx, params).await });
        let mut task = task;
        // 看门狗：截止时间可被 handler 经 DeadlineHandle::extend 延期；
        // 只有截止时间真正变化时才继续等，否则超时 SIGKILL。
        let finish = |r: Result<Result<Value, ToolError>, tokio::task::JoinError>| match r {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(e),
            Err(je) => Err(ToolError::internal(format!("handler 任务异常：{je}"))),
        };
        loop {
            let dl = *deadline.lock().expect("deadline poisoned");
            tokio::select! {
                r = &mut task => return finish(r),
                _ = tokio::time::sleep_until(dl) => {
                    if *deadline.lock().expect("deadline poisoned") == dl {
                        task.abort();
                        return Err(ToolError::timeout());
                    }
                    // 已延期：回到循环重新等待新截止时间。
                }
                _ = cancel.cancelled() => {
                    // SIGTERM：宽限期等 handler 自主收尾；到期 SIGKILL。
                    tokio::select! {
                        r = &mut task => return finish(r),
                        _ = tokio::time::sleep(self.grace) => {
                            task.abort();
                            return Err(ToolError::aborted());
                        }
                    }
                }
            }
        }
    }
}
