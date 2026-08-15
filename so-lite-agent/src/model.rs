//! 模型 Provider 层：`ModelService` 抽象的内置适配器与注册表。
//!
//! M3：`register_provider()` + OpenAI 兼容（Chat Completions / Responses，覆盖 DeepSeek）
//! + Anthropic 兼容 + 自定义 URL；流式事件归一化为 `ModelChunk`。

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};

use crate::contract::full_to_wire;
use crate::message::{Message, MessageKind};
use crate::services::{
    AbortSignal, ItemKind, ModelChunk, ModelError, ModelRequest, ModelResponse, ModelService,
    ModelStream, ResponseFormat, TokenUsage, ToolCallSpec, ToolChoice, ToolSchema,
};

pub type ProviderFactory = Arc<dyn Fn(&str, &str, &str) -> Arc<dyn ModelService> + Send + Sync>;

static PROVIDERS: OnceLock<RwLock<HashMap<String, ProviderFactory>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<String, ProviderFactory>> {
    PROVIDERS.get_or_init(|| {
        let mut map = HashMap::new();
        map.insert("openai".into(), {
            let f: ProviderFactory = Arc::new(|url, key, model| {
                Arc::new(ChatCompletionsModelService::new(
                    url.to_string(),
                    key.to_string(),
                    model.to_string(),
                ))
            });
            f
        });
        map.insert("responses".into(), {
            let f: ProviderFactory = Arc::new(|url, key, model| {
                Arc::new(ResponsesModelService::new(
                    url.to_string(),
                    key.to_string(),
                    model.to_string(),
                ))
            });
            f
        });
        map.insert("anthropic".into(), {
            let f: ProviderFactory = Arc::new(|url, key, model| {
                Arc::new(AnthropicModelService::new(
                    url.to_string(),
                    key.to_string(),
                    model.to_string(),
                ))
            });
            f
        });
        RwLock::new(map)
    })
}

/// 注册自定义 Provider：`build_provider("my_provider", ...)` 即可使用。
pub fn register_provider(name: &str, factory: ProviderFactory) -> Result<(), String> {
    let mut map = registry()
        .write()
        .map_err(|_| "provider registry poisoned".to_string())?;
    map.insert(name.to_string(), factory);
    Ok(())
}

pub fn build_provider(
    name: &str,
    api_url: &str,
    api_key: &str,
    model: &str,
) -> Result<Arc<dyn ModelService>, String> {
    let map = registry()
        .read()
        .map_err(|_| "provider registry poisoned".to_string())?;
    let factory = map
        .get(name)
        .ok_or_else(|| format!("未知 Provider：{name}"))?;
    Ok(factory(api_url, api_key, model))
}

// ---------- OpenAI 兼容 Chat Completions ----------

pub struct ChatCompletionsModelService {
    client: Client,
    api_url: String,
    api_key: String,
    model: String,
    max_tokens: u32,
}

impl ChatCompletionsModelService {
    pub fn new(api_url: String, api_key: String, model: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
            .build()
            .expect("reqwest client 构建失败");
        Self {
            client,
            api_url,
            api_key,
            model,
            max_tokens: 4096,
        }
    }

    async fn complete_inner(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        let url = format!("{}/chat/completions", self.api_url.trim_end_matches('/'));
        let mut body = json!({
            "model": self.model,
            "messages": messages_to_cc(&request.messages),
            "max_tokens": self.max_tokens,
        });
        if let Some(fmt) = &request.response_format {
            body["response_format"] = match fmt {
                ResponseFormat::JsonObject => json!({"type": "json_object"}),
                ResponseFormat::JsonSchema { .. } => json!({"type": "json_object"}),
            };
        }
        let response = match tokio::time::timeout(
            Duration::from_secs(180),
            self.client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send(),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(ModelError::Transport(reqwest_chain(&e))),
            Err(_) => return Err(ModelError::Timeout),
        };
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(map_status_error(status, &text));
        }
        if signal.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let data: Value = response
            .json()
            .await
            .map_err(|e| ModelError::Protocol(format!("响应解析失败：{e}")))?;
        let text = data["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let usage = TokenUsage {
            input_tokens: data["usage"]["prompt_tokens"].as_u64(),
            output_tokens: data["usage"]["completion_tokens"].as_u64(),
            cached_tokens: data["usage"]["prompt_cache_hit_tokens"].as_u64(),
            cache_miss_tokens: data["usage"]["prompt_cache_miss_tokens"].as_u64(),
            ..Default::default()
        };
        Ok(ModelResponse {
            text,
            tool_calls: Vec::new(),
            usage: Some(usage),
        })
    }
}

#[async_trait::async_trait]
impl ModelService for ChatCompletionsModelService {
    async fn stream(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError> {
        let response = self.complete_inner(request, signal).await?;
        let mut chunks: Vec<Result<ModelChunk, ModelError>> = Vec::new();
        if !response.text.is_empty() {
            chunks.push(Ok(ModelChunk::TextDelta(response.text)));
        }
        chunks.push(Ok(ModelChunk::Done));
        Ok(Box::new(futures_util::stream::iter(chunks)))
    }

    async fn complete(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        self.complete_inner(request, signal).await
    }
}

// ---------- Anthropic 兼容（Messages API 最小适配器） ----------

pub struct AnthropicModelService {
    client: Client,
    api_url: String,
    api_key: String,
    model: String,
}

impl AnthropicModelService {
    pub fn new(api_url: String, api_key: String, model: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("reqwest client 构建失败");
        Self {
            client,
            api_url,
            api_key,
            model,
        }
    }

    async fn complete_inner(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        let url = format!("{}/v1/messages", self.api_url.trim_end_matches('/'));
        let system = request
            .messages
            .iter()
            .filter_map(|m| match &m.kind {
                MessageKind::System { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let messages: Vec<Value> = request
            .messages
            .iter()
            .filter_map(|m| match &m.kind {
                MessageKind::User { text, .. } => Some(json!({"role": "user", "content": text})),
                MessageKind::Assistant { text } => {
                    Some(json!({"role": "assistant", "content": text}))
                }
                _ => None,
            })
            .collect();
        let mut body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "messages": messages,
        });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        let response = match tokio::time::timeout(
            Duration::from_secs(180),
            self.client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send(),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(ModelError::Transport(reqwest_chain(&e))),
            Err(_) => return Err(ModelError::Timeout),
        };
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(map_status_error(status, &text));
        }
        if signal.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let data: Value = response
            .json()
            .await
            .map_err(|e| ModelError::Protocol(format!("响应解析失败：{e}")))?;
        let text = data["content"]
            .as_array()
            .and_then(|arr| {
                arr.iter().find_map(|item| {
                    (item["type"] == "text")
                        .then(|| item["text"].as_str().unwrap_or_default().to_string())
                })
            })
            .unwrap_or_default();
        Ok(ModelResponse {
            text,
            tool_calls: Vec::<ToolCallSpec>::new(),
            usage: None,
        })
    }
}

#[async_trait::async_trait]
impl ModelService for AnthropicModelService {
    async fn stream(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError> {
        let response = self.complete_inner(request, signal).await?;
        let mut chunks: Vec<Result<ModelChunk, ModelError>> = Vec::new();
        if !response.text.is_empty() {
            chunks.push(Ok(ModelChunk::TextDelta(response.text)));
        }
        chunks.push(Ok(ModelChunk::Done));
        Ok(Box::new(futures_util::stream::iter(chunks)))
    }

    async fn complete(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        self.complete_inner(request, signal).await
    }
}

// ---------- DeepSeek Responses API（SSE） ----------

pub(crate) struct SseEvent {
    pub(crate) name: String,
    pub(crate) data: String,
}

#[derive(Default)]
pub(crate) struct SseParser {
    buffer: Vec<u8>,
    event: String,
    data: String,
}

impl SseParser {
    pub(crate) fn push_chunk(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1])
                .trim_end_matches('\r')
                .to_string();
            if line.is_empty() {
                if !self.event.is_empty() || !self.data.is_empty() {
                    events.push(SseEvent {
                        name: std::mem::take(&mut self.event),
                        data: std::mem::take(&mut self.data),
                    });
                }
            } else if let Some(v) = line.strip_prefix("event:") {
                self.event = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(v.trim());
            }
        }
        events
    }
}

pub struct ResponsesModelService {
    client: Client,
    api_url: String,
    api_key: String,
    model: String,
}

impl ResponsesModelService {
    pub fn new(api_url: String, api_key: String, model: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
            .build()
            .expect("reqwest client 构建失败");
        Self {
            client,
            api_url,
            api_key,
            model,
        }
    }

    fn build_body(&self, request: &ModelRequest) -> Result<Value, ModelError> {
        let mut body = json!({
            "model": self.model,
            "input": messages_to_responses_input(&request.messages)?,
            "stream": true,
        });
        if let Some(tools) = &request.tools {
            body["tools"] = json!(tools.iter().map(tool_to_function).collect::<Vec<_>>());
        }
        if let Some(effort) = &request.reasoning_effort {
            body["reasoning"] = json!({"effort": effort});
        }
        if let Some(fmt) = &request.response_format {
            body["text"] = json!({"format": text_format(fmt)});
        }
        if let Some(choice) = &request.tool_choice {
            body["tool_choice"] = match choice {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::Required => json!("required"),
                ToolChoice::Function { name } => json!({"type": "function", "name": name}),
            };
            body["reasoning"] = json!({"effort": "none"});
        }
        Ok(body)
    }

    async fn post(&self, url: &str, body: &Value) -> Result<reqwest::Response, ModelError> {
        tokio::time::timeout(
            Duration::from_secs(60),
            self.client
                .post(url)
                .bearer_auth(&self.api_key)
                .json(body)
                .send(),
        )
        .await
        .map_err(|_| ModelError::Timeout)?
        .map_err(|e| ModelError::Transport(reqwest_chain(&e)))
    }
}

#[async_trait::async_trait]
impl ModelService for ResponsesModelService {
    async fn stream(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError> {
        let body = self.build_body(request)?;
        let url = responses_endpoint(&self.api_url);
        let response = self.post(&url, &body).await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(map_status_error(status, &text));
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<ModelChunk, ModelError>>(128);
        let mut byte_stream = response.bytes_stream();
        let cancel = signal.cancelled();
        tokio::spawn(async move {
            let mut parser = SseParser::default();
            let mut last_tool_index = 0usize;
            let mut done = false;
            loop {
                let next = tokio::select! {
                    chunk = byte_stream.next() => chunk,
                    _ = cancel.cancelled() => None,
                };
                let Some(chunk) = next else { break };
                match chunk {
                    Ok(bytes) => {
                        for ev in parser.push_chunk(&bytes) {
                            match ev.name.as_str() {
                                "response.output_item.added" => {
                                    if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                                        let item = &v["item"];
                                        if item["type"] == "reasoning" {
                                            let id =
                                                item["id"].as_str().unwrap_or_default().to_string();
                                            let _ = tx
                                                .send(Ok(ModelChunk::ReasoningItemStart { id }))
                                                .await;
                                        } else if item["type"] == "function_call" {
                                            last_tool_index += 1;
                                            let index = v["output_index"]
                                                .as_u64()
                                                .map(|i| i as usize)
                                                .unwrap_or(last_tool_index);
                                            let call_id = item["call_id"]
                                                .as_str()
                                                .unwrap_or_default()
                                                .to_string();
                                            let name = item["name"]
                                                .as_str()
                                                .unwrap_or_default()
                                                .to_string();
                                            let _ = tx
                                                .send(Ok(ModelChunk::ToolCallStart {
                                                    index,
                                                    call_id,
                                                    name,
                                                }))
                                                .await;
                                        }
                                    }
                                }
                                "response.output_item.done" => {
                                    if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                                        match v["item"]["type"].as_str() {
                                            Some("message") => {
                                                let _ = tx
                                                    .send(Ok(ModelChunk::ItemDone {
                                                        kind: ItemKind::Message,
                                                    }))
                                                    .await;
                                            }
                                            Some("function_call") => {
                                                let _ = tx
                                                    .send(Ok(ModelChunk::ItemDone {
                                                        kind: ItemKind::FunctionCall,
                                                    }))
                                                    .await;
                                            }
                                            Some("reasoning") => {
                                                let _ = tx
                                                    .send(Ok(ModelChunk::ItemDone {
                                                        kind: ItemKind::Reasoning,
                                                    }))
                                                    .await;
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                "response.reasoning_text.delta" => {
                                    let _ = tx
                                        .send(Ok(ModelChunk::ReasoningDelta(parse_delta(&ev.data))))
                                        .await;
                                }
                                "response.output_text.delta" => {
                                    let _ = tx
                                        .send(Ok(ModelChunk::TextDelta(parse_delta(&ev.data))))
                                        .await;
                                }
                                "response.function_call_arguments.delta" => {
                                    if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                                        let index = v["output_index"]
                                            .as_u64()
                                            .map(|i| i as usize)
                                            .unwrap_or(last_tool_index);
                                        let data =
                                            v["delta"].as_str().unwrap_or_default().to_string();
                                        let _ = tx
                                            .send(Ok(ModelChunk::ToolCallDelta { index, data }))
                                            .await;
                                    }
                                }
                                "response.completed" | "response.incomplete" => {
                                    if !done {
                                        if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                                            let usage_src = if v["response"]["usage"].is_object() {
                                                &v["response"]["usage"]
                                            } else {
                                                &v["usage"]
                                            };
                                            let _ = tx
                                                .send(Ok(ModelChunk::Usage(parse_usage(usage_src))))
                                                .await;
                                        }
                                        let _ = tx.send(Ok(ModelChunk::Done)).await;
                                        done = true;
                                    }
                                }
                                "response.failed" if !done => {
                                    let message = serde_json::from_str::<Value>(&ev.data)
                                        .ok()
                                        .and_then(|v| {
                                            v["error"]["message"].as_str().map(|s| s.to_string())
                                        })
                                        .unwrap_or_else(|| "响应失败".into());
                                    let _ = tx.send(Err(ModelError::Protocol(message))).await;
                                    done = true;
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(ModelError::Transport(e.to_string()))).await;
                        break;
                    }
                }
            }
            if !done {
                let _ = tx.send(Ok(ModelChunk::Done)).await;
            }
        });
        Ok(Box::new(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

// ---------- 协议转换助手 ----------

fn responses_endpoint(api_url: &str) -> String {
    let base = api_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    format!("{base}/responses")
}

fn text_format(fmt: &ResponseFormat) -> Value {
    match fmt {
        ResponseFormat::JsonObject => json!({"type": "json_object"}),
        ResponseFormat::JsonSchema { name, schema } => {
            json!({"type": "json_schema", "name": name, "schema": schema})
        }
    }
}

fn parse_delta(data: &str) -> String {
    serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|v| v["delta"].as_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

fn tool_to_function(t: &ToolSchema) -> Value {
    json!({
        "type": "function",
        "name": t.name,
        "description": t.description,
        "parameters": t.input_schema,
    })
}

fn parse_usage(v: &Value) -> TokenUsage {
    let input_tokens = v["input_tokens"].as_u64();
    let cached_tokens = v["input_tokens_details"]["cached_tokens"].as_u64();
    TokenUsage {
        input_tokens,
        output_tokens: v["output_tokens"].as_u64(),
        cached_tokens,
        cache_miss_tokens: match (input_tokens, cached_tokens) {
            (Some(i), Some(c)) => Some(i.saturating_sub(c)),
            _ => None,
        },
        reasoning_tokens: v["output_tokens_details"]["reasoning_tokens"].as_u64(),
    }
}

fn map_status_error(status: reqwest::StatusCode, body: &str) -> ModelError {
    let body = body.chars().take(500).collect::<String>();
    match status.as_u16() {
        401 => ModelError::AuthFailed(body),
        402 => ModelError::QuotaExceeded(body),
        404 => ModelError::ModelNotFound(body),
        429 => ModelError::RateLimited(body),
        400 | 422 => ModelError::Protocol(body),
        _ => ModelError::Transport(format!("HTTP {status}: {body}")),
    }
}

fn reqwest_chain(e: &reqwest::Error) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(&format!(" <- {s}"));
        src = s.source();
    }
    out
}

fn messages_to_cc(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for msg in messages {
        match &msg.kind {
            MessageKind::User { text, .. } => {
                out.push(json!({"role": "user", "content": text}));
            }
            MessageKind::Assistant { text } => {
                out.push(json!({"role": "assistant", "content": text}));
            }
            MessageKind::System { text, .. } => {
                out.push(json!({"role": "system", "content": text}));
            }
            MessageKind::Reasoning { .. } => {}
            MessageKind::ToolCall {
                entry,
                params,
                result,
                call_id,
            } => {
                let call_id = if call_id.is_empty() {
                    msg.id.to_string()
                } else {
                    call_id.clone()
                };
                let arguments = serde_json::to_string(params).unwrap_or_else(|_| "{}".into());
                out.push(json!({
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": call_id,
                        "type": "function",
                        "function": {"name": full_to_wire(entry), "arguments": arguments},
                    }],
                }));
                let output = match result {
                    Ok(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".into()),
                    Err(e) => {
                        serde_json::to_string(&json!({"error": e})).unwrap_or_else(|_| "{}".into())
                    }
                };
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": output,
                }));
            }
        }
    }
    out
}

fn messages_to_responses_input(messages: &[Message]) -> Result<Vec<Value>, ModelError> {
    let mut items = Vec::new();
    for msg in messages {
        match &msg.kind {
            MessageKind::User { text, .. } => {
                items.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": text}],
                }));
            }
            MessageKind::Assistant { text } => items.push(json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": text}],
            })),
            MessageKind::System { text, .. } => items.push(json!({
                "type": "message",
                "role": "system",
                "content": [{"type": "input_text", "text": text}],
            })),
            MessageKind::Reasoning { id, text } => {
                items.push(json!({
                    "type": "reasoning",
                    "id": id,
                    "summary": [{"type": "summary_text", "text": text}],
                    "content": [{"type": "reasoning_text", "text": text}],
                }));
            }
            MessageKind::ToolCall {
                entry,
                params,
                result,
                call_id,
            } => {
                let call_id = if call_id.is_empty() {
                    msg.id.to_string()
                } else {
                    call_id.clone()
                };
                let arguments = serde_json::to_string(params)
                    .map_err(|e| ModelError::Protocol(format!("参数序列化失败：{e}")))?;
                items.push(json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": full_to_wire(entry),
                    "arguments": arguments,
                }));
                let output = match result {
                    Ok(v) => serde_json::to_string(v)
                        .map_err(|e| ModelError::Protocol(format!("结果序列化失败：{e}")))?,
                    Err(e) => serde_json::to_string(&json!({"error": e}))
                        .map_err(|e| ModelError::Protocol(format!("错误序列化失败：{e}")))?,
                };
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
            }
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provider_registry_builds_builtins() {
        let svc = build_provider("openai", "http://localhost", "k", "m").unwrap();
        assert!(
            svc.complete(
                &ModelRequest {
                    model: crate::services::ModelKind::Main,
                    messages: vec![Message::user("hi")],
                    tools: None,
                    reasoning_effort: None,
                    response_format: None,
                    tool_choice: None,
                },
                &AbortSignal::new(),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn responses_stream_maps_real_event_shapes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let sse = concat!(
            "event: response.created\ndata: {\"type\":\"response.created\"}\n\n",
            "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"message\",\"id\":\"m1\"},\"output_index\":0}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"m1\",\"output_index\":0,\"delta\":\"北京今天晴天\"}\n\n",
            "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"id\":\"m1\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"北京今天晴天\"}]},\"output_index\":0}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":84,\"input_tokens_details\":{\"cached_tokens\":0},\"output_tokens\":27,\"output_tokens_details\":{\"reasoning_tokens\":17}}}}\n\n",
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await;
            let header =
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            sock.write_all(header.as_bytes()).await.unwrap();
            sock.write_all(sse.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        });

        let svc = ResponsesModelService::new(
            format!("http://{addr}"),
            "test-key".into(),
            "deepseek-v4-flash".into(),
        );
        let request = ModelRequest {
            model: crate::services::ModelKind::Main,
            messages: vec![Message::user("北京天气？")],
            tools: None,
            reasoning_effort: None,
            response_format: None,
            tool_choice: None,
        };
        let mut stream = svc
            .stream(&request, &AbortSignal::new())
            .await
            .expect("stream 应成功");
        let mut text = String::new();
        let mut done = false;
        while let Some(chunk) = stream.next().await {
            match chunk.expect("chunk 无错误") {
                ModelChunk::TextDelta(d) => text.push_str(&d),
                ModelChunk::Done => done = true,
                _ => {}
            }
        }
        assert_eq!(text, "北京今天晴天");
        assert!(done);
    }
}
