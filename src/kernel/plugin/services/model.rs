//! Model 契约（Q6 + ADR-0014/0020）。

use super::*;

// ---------- Model 契约（Q6 + ADR-0014/0020） ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    JsonObject,
    /// DeepSeek Responses API `text.format` 的 json_schema 模式（服务端强制结构）。
    JsonSchema {
        name: String,
        schema: serde_json::Value,
    },
}

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub tools: Option<Vec<ToolSchema>>,
    /// DeepSeek 思考模式 effort（none/minimal/low/medium/high/xhigh/max）。
    pub reasoning_effort: Option<String>,
    pub response_format: Option<ResponseFormat>,
    /// 工具选择策略：强制调用指定工具时用 Function{name}（API 要求关闭思考模式）。
    pub tool_choice: Option<ToolChoice>,
    /// 回合标识（ADR-0047 修订 R14）：同一学生回合的所有请求共用它，服务端据此
    /// 只扣一次。`None` = 交给适配器按"最后一条是不是用户消息"推断。
    pub turn_id: Option<String>,
}

/// 工具选择策略（OpenAI Responses 兼容）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Required,
    /// 强制调用指定工具（wire name）。
    Function {
        name: String,
    },
}

impl ModelRequest {
    pub fn chat(messages: Vec<Message>) -> Self {
        Self {
            messages,
            tools: None,
            reasoning_effort: None,
            response_format: None,
            tool_choice: None,
            turn_id: None,
        }
    }
}

/// 模型可见工具（wire name + JSON Schema）。
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
    /// 推理 item 开始（携带 id，后续轮次必须按 id 回传给 API）。
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
    /// 完整响应中的 token 用量（response.completed 携带）。
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
    /// 缓存命中的输入 tokens（Responses API: input_tokens_details.cached_tokens；
    /// Chat Completions: prompt_cache_hit_tokens）。
    pub cached_tokens: Option<u64>,
    /// 缓存未命中的输入 tokens（Responses API 由 input - cached 推算；
    /// Chat Completions: prompt_cache_miss_tokens）。
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
    /// 系统性错误：重试/换参数无意义，应中断回合（Q17c）。
    pub fn is_systemic(&self) -> bool {
        matches!(
            self,
            ModelError::AuthFailed(_) | ModelError::QuotaExceeded(_) | ModelError::ModelNotFound(_)
        )
    }
}

pub type ModelStream = Box<dyn Stream<Item = Result<ModelChunk, ModelError>> + Send + Unpin>;

/// 纯净 provider 抽象（不管超时/审计；护栏在包装层与 loop）。
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
                } => {
                    calls.push((
                        index,
                        ToolCallSpec {
                            call_id,
                            name,
                            arguments: String::new(),
                        },
                    ));
                }
                ModelChunk::ToolCallDelta { index, data } => {
                    if let Some((_, spec)) = calls.iter_mut().find(|(i, _)| *i == index) {
                        spec.arguments.push_str(&data);
                    }
                }
                ModelChunk::Usage(usage) => {
                    usage_holder = Some(usage);
                }
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

/// 注入用户插件的模型受控句柄：只暴露带超时 + abort + 审计的 complete。
#[derive(Clone)]
pub struct ModelHandle {
    inner: Arc<dyn ModelService>,
    timeout: Duration,
    auditor: Auditor,
}

impl ModelHandle {
    pub fn new(inner: Arc<dyn ModelService>, timeout: Duration, auditor: Auditor) -> Self {
        Self {
            inner,
            timeout,
            auditor,
        }
    }

    pub async fn complete(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(self.timeout, self.inner.complete(request, signal)).await;
        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(Ok(resp)) => {
                self.auditor.record(AuditRecord::LlmCall {
                    provider: "handle".into(),
                    model: "main".into(),
                    kind: "complete".into(),
                    tokens_in: resp.usage.as_ref().and_then(|u| u.input_tokens),
                    tokens_out: resp.usage.as_ref().and_then(|u| u.output_tokens),
                    duration_ms,
                    ok: true,
                });
                Ok(resp)
            }
            Ok(Err(e)) => {
                self.auditor.record(AuditRecord::LlmCall {
                    provider: "handle".into(),
                    model: "main".into(),
                    kind: "complete".into(),
                    tokens_in: None,
                    tokens_out: None,
                    duration_ms,
                    ok: false,
                });
                Err(e)
            }
            Err(_) => Err(ModelError::Timeout),
        }
    }
}
