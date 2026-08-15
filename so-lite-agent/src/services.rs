//! 通用服务契约：模型 Provider 抽象、会话存储、取消信号、受控句柄。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::message::{Message, MessageId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceId {
    Storage,
    Memory,
    Compute,
    Model,
}

#[derive(Clone)]
pub struct AbortSignal {
    token: CancellationToken,
}

impl AbortSignal {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    pub fn from_token(token: CancellationToken) -> Self {
        Self { token }
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub fn cancelled(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        Self::new()
    }
}

// ---------- 模型 Provider ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Main,
    Vision,
}

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub model: ModelKind,
    pub messages: Vec<Message>,
    pub tools: Option<Vec<ToolSchema>>,
    pub reasoning_effort: Option<String>,
    pub response_format: Option<ResponseFormat>,
    pub tool_choice: Option<ToolChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    JsonObject,
    JsonSchema {
        name: String,
        schema: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Required,
    Function { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone)]
pub enum ItemKind {
    Message,
    FunctionCall,
    Reasoning,
}

#[derive(Debug, Clone)]
pub enum ModelChunk {
    TextDelta(String),
    ReasoningDelta(String),
    ReasoningItemStart {
        id: String,
    },
    ToolCallStart {
        index: usize,
        call_id: String,
        name: String,
    },
    ToolCallDelta {
        index: usize,
        data: String,
    },
    ItemDone {
        kind: ItemKind,
    },
    Usage(TokenUsage),
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallSpec {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_miss_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCallSpec>,
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ModelError {
    #[error("鉴权失败：{0}")]
    AuthFailed(String),
    #[error("余额或配额不足：{0}")]
    QuotaExceeded(String),
    #[error("模型不存在或已下架：{0}")]
    ModelNotFound(String),
    #[error("请求超时")]
    Timeout,
    #[error("被取消")]
    Cancelled,
    #[error("限流：{0}")]
    RateLimited(String),
    #[error("传输错误：{0}")]
    Transport(String),
    #[error("协议错误：{0}")]
    Protocol(String),
    #[error("配置缺失：{0}")]
    Config(String),
}

impl ModelError {
    pub fn is_systemic(&self) -> bool {
        matches!(
            self,
            ModelError::AuthFailed(_) | ModelError::QuotaExceeded(_) | ModelError::ModelNotFound(_)
        )
    }
}

pub type ModelStream = Box<dyn Stream<Item = Result<ModelChunk, ModelError>> + Send + Unpin>;

#[async_trait]
pub trait ModelService: Send + Sync {
    async fn stream(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError>;

    async fn complete(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        use futures_util::StreamExt;

        let mut stream = self.stream(request, signal).await?;
        let mut text = String::new();
        let mut calls: Vec<(usize, ToolCallSpec)> = Vec::new();
        let mut usage_holder: Option<TokenUsage> = None;
        while let Some(chunk) = stream.next().await {
            match chunk? {
                ModelChunk::TextDelta(d) => text.push_str(&d),
                ModelChunk::ToolCallStart {
                    index,
                    call_id,
                    name,
                } => calls.push((
                    index,
                    ToolCallSpec {
                        call_id,
                        name,
                        arguments: String::new(),
                    },
                )),
                ModelChunk::ToolCallDelta { index, data } => {
                    if let Some((_, spec)) = calls.iter_mut().find(|(i, _)| *i == index) {
                        spec.arguments.push_str(&data);
                    }
                }
                ModelChunk::Usage(u) => usage_holder = Some(u),
                _ => {}
            }
        }
        Ok(ModelResponse {
            text,
            tool_calls: calls.into_iter().map(|(_, spec)| spec).collect(),
            usage: usage_holder,
        })
    }
}

#[derive(Clone)]
pub struct ModelHandle {
    inner: Arc<dyn ModelService>,
    timeout: Duration,
}

impl ModelHandle {
    pub fn new(inner: Arc<dyn ModelService>, timeout: Duration) -> Self {
        Self { inner, timeout }
    }

    pub async fn complete(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        match tokio::time::timeout(self.timeout, self.inner.complete(request, signal)).await {
            Ok(result) => result,
            Err(_) => Err(ModelError::Timeout),
        }
    }
}

// ---------- 会话 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionKey(pub uuid::Uuid);

impl SessionKey {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for SessionKey {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub key: SessionKey,
    pub goal: Option<Goal>,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub last_activity_at: DateTime<Utc>,
    pub active_path: Option<MessageId>,
}

impl SessionMeta {
    pub fn new(key: SessionKey) -> Self {
        let now = Utc::now();
        Self {
            key,
            goal: None,
            status: SessionStatus::Active,
            created_at: now,
            archived_at: None,
            last_activity_at: now,
            active_path: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("会话不存在：{0}")]
    NotFound(SessionKey),
    #[error("已存在：{0}")]
    AlreadyExists(String),
    #[error("数据损坏：{0}")]
    Corrupt(String),
    #[error("IO 错误：{0}")]
    Io(String),
    #[error("内部错误：{0}")]
    Internal(String),
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_session(
        &self,
        key: &SessionKey,
        meta: &SessionMeta,
    ) -> Result<(), SessionError>;
    async fn get_session(&self, key: &SessionKey) -> Result<Option<SessionMeta>, SessionError>;
    async fn append_message(&self, key: &SessionKey, msg: &Message) -> Result<(), SessionError>;
    async fn read_path(&self, key: &SessionKey) -> Result<Vec<Message>, SessionError>;
    async fn read_all(&self, key: &SessionKey) -> Result<Vec<Message>, SessionError>;
    async fn set_active_path(
        &self,
        key: &SessionKey,
        message_id: Option<MessageId>,
    ) -> Result<(), SessionError>;
    async fn derive_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
        text: &str,
    ) -> Result<Vec<Message>, SessionError>;
    async fn switch_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
    ) -> Result<Vec<Message>, SessionError>;
    async fn set_goal(&self, key: &SessionKey, goal: &Goal) -> Result<(), SessionError>;
    async fn archive(&self, key: &SessionKey) -> Result<(), SessionError>;
    async fn list_sessions(&self) -> Result<Vec<SessionMeta>, SessionError>;
    async fn set_last_activity(
        &self,
        key: &SessionKey,
        at: DateTime<Utc>,
    ) -> Result<(), SessionError>;
}

// ---------- 服务句柄 ----------

#[derive(Default, Clone)]
pub struct ServiceHandles {
    model: Option<ModelHandle>,
}

impl ServiceHandles {
    pub fn model(&self) -> Option<&ModelHandle> {
        self.model.as_ref()
    }

    pub fn with_model(mut self, h: ModelHandle) -> Self {
        self.model = Some(h);
        self
    }

    pub fn available(&self) -> HashSet<ServiceId> {
        let mut set = HashSet::new();
        if self.model.is_some() {
            set.insert(ServiceId::Model);
        }
        set
    }

    pub fn filter(&self, requires: &[ServiceId]) -> ServiceHandles {
        let mut out = ServiceHandles::default();
        for id in requires {
            if *id == ServiceId::Model {
                out.model = self.model.clone();
            }
        }
        out
    }
}
