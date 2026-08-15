//! 通用入口点契约：调用方策略、工具/命令/事件元数据、结构化错误。

use schemars::JsonSchema;
use schemars::Schema;
use serde::{Deserialize, Serialize};

use crate::services::ServiceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CallerPolicy {
    UserAndModel,
    UserOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LoadPolicy {
    Eager,
    #[default]
    Lazy,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Info {
    pub namespace: String,
    #[serde(default)]
    pub requires: Vec<ServiceId>,
    #[serde(default)]
    pub provides: Vec<ServiceId>,
    #[serde(default)]
    pub load: LoadPolicy,
    #[serde(default)]
    pub tools: Vec<ToolDef>,
    #[serde(default)]
    pub commands: Vec<CommandDef>,
    #[serde(default)]
    pub events: Vec<EventDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    #[serde(default = "default_true")]
    pub user_visible: bool,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    pub description: String,
    pub params: Schema,
    pub policy: CallerPolicy,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandDef {
    pub name: String,
    #[serde(default = "default_true")]
    pub user_visible: bool,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    pub description: String,
    pub params: Schema,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDef {
    pub name: String,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorCode {
    UnknownTool,
    InvalidParams,
    HandlerError,
    Timeout,
    Aborted,
    Forbidden,
    ModelUnavailable,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolError {
    pub code: ToolErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl ToolError {
    pub fn new(code: ToolErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }

    pub fn unknown_tool(name: &str) -> Self {
        Self::new(
            ToolErrorCode::UnknownTool,
            format!("未知工具：{name}"),
            false,
        )
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::InvalidParams, message, true)
    }

    pub fn handler(message: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::HandlerError, message, false)
    }

    pub fn timeout() -> Self {
        Self::new(
            ToolErrorCode::Timeout,
            "工具执行超时，已被内核强制终止",
            true,
        )
    }

    pub fn aborted() -> Self {
        Self::new(ToolErrorCode::Aborted, "执行被取消", false)
    }

    pub fn forbidden() -> Self {
        Self::new(
            ToolErrorCode::Forbidden,
            "该入口点不允许当前调用方调用",
            false,
        )
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::Internal, message, false)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("namespace 已被占用：{0}")]
    NamespaceTaken(String),
    #[error("声明的服务不可用：{0:?}")]
    CapabilityUnavailable(Vec<ServiceId>),
    #[error("服务已被内核插件提供：{0:?}")]
    ServiceTaken(ServiceId),
    #[error("只有内核插件能声明 provides：{0:?}")]
    ProvisionNotAllowed(Vec<ServiceId>),
    #[error("入口点已存在：{0}")]
    DuplicateEntry(String),
    #[error("未声明的入口点：{0}")]
    UndeclaredEntry(String),
    #[error("wire name 冲突：{0}")]
    WireNameCollision(String),
    #[error("注册失败：{0}")]
    Internal(String),
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct EmptyParams {}

pub fn empty_params() -> Schema {
    schemars::json_schema!({"type": "object"})
}

pub fn full_to_wire(full: &str) -> String {
    full.replace("::", "__")
}

pub fn full_name(namespace: &str, short: &str) -> String {
    format!("{namespace}::{short}")
}
