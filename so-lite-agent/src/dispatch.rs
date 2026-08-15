//! 统一工具执行：Caller 检查 → 懒注册 → 参数校验 → 超时/取消 → 审计。

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::audit::{AuditRecord, Auditor};
use crate::contract::{CallerPolicy, ToolError};
use crate::events::EventSink;
use crate::registry::{Handler, RegisteredEntry, Registry};
use crate::services::AbortSignal;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub type ToolHandler = Arc<
    dyn for<'a> Fn(&'a ToolCallContext, Value) -> BoxFuture<'a, Result<Value, ToolError>>
        + Send
        + Sync,
>;

pub type CommandHandler = ToolHandler;
pub type EventHandler =
    Arc<dyn Fn(Value) -> BoxFuture<'static, Result<(), ToolError>> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Caller {
    Model,
    User,
}

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

pub struct ToolCallContext {
    pub signal: AbortSignal,
    pub deadline: DeadlineHandle,
    pub events: Arc<dyn EventSink>,
}

pub struct Dispatch {
    registry: Arc<Registry>,
    auditor: Auditor,
    default_tool_timeout: Duration,
    grace: Duration,
    turn_budget: Duration,
    events: Arc<dyn EventSink>,
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
        }
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
        if caller == Caller::Model && entry.policy == CallerPolicy::UserOnly {
            self.auditor.record(AuditRecord::EntryPointCall {
                entry: full_name.into(),
                caller,
                ok: false,
                error: Some("forbidden".into()),
            });
            return Err(ToolError::forbidden());
        }
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

    pub fn resolve_wire(&self, wire: &str) -> Option<String> {
        self.registry.resolve_wire(wire)
    }

    pub fn entry_icon(&self, full_name: &str) -> Option<String> {
        self.registry.entry_icon(full_name)
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
        let ctx = ToolCallContext {
            signal: AbortSignal::from_token(cancel.clone()),
            deadline: DeadlineHandle::new(deadline.clone(), turn_end),
            events: self.events.clone(),
        };
        let task: JoinHandle<Result<Value, ToolError>> =
            tokio::spawn(async move { handler(&ctx, params).await });
        let mut task = task;
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
                }
                _ = cancel.cancelled() => {
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
