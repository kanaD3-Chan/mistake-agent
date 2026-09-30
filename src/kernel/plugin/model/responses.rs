use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};

use crate::kernel::plugin::services::{
    AbortSignal, ItemKind, ModelChunk, ModelError, ModelRequest, ModelService, ModelStream,
    TokenUsage, ToolChoice,
};

use super::*;

/// 请求被拒时的输入摘要（调试用）：逐项列出类型与关键字段，reasoning 标注文本长度。
fn summarize_input(input: &Value) -> String {
    let items = input.as_array().map(|a| a.len()).unwrap_or(0);
    let mut parts: Vec<String> = Vec::new();
    if let Some(arr) = input.as_array() {
        for item in arr {
            match item["type"].as_str() {
                Some("message") => {
                    let role = item["role"].as_str().unwrap_or("?");
                    let text = item["content"][0]["text"]
                        .as_str()
                        .unwrap_or_default();
                    parts.push(format!(
                        "msg({role},len={},head={:?})",
                        text.len(),
                        text.chars().take(20).collect::<String>()
                    ));
                }
                Some("reasoning") => {
                    let id = item["id"].as_str().unwrap_or("?");
                    let clen = item["content"][0]["text"]
                        .as_str()
                        .map(|s| s.len())
                        .unwrap_or(0);
                    let slen = item["summary"][0]["text"]
                        .as_str()
                        .map(|s| s.len())
                        .unwrap_or(0);
                    parts.push(format!(
                        "reasoning(id={id},content_len={clen},summary_len={slen})"
                    ));
                }
                Some("function_call") => {
                    parts.push(format!(
                        "call(name={},call_id={})",
                        item["name"].as_str().unwrap_or("?"),
                        item["call_id"].as_str().unwrap_or("?")
                    ));
                }
                Some("function_call_output") => {
                    parts.push(format!(
                        "output(call_id={})",
                        item["call_id"].as_str().unwrap_or("?")
                    ));
                }
                other => parts.push(format!("item({other:?})")),
            }
        }
    }
    format!("input_items={items} [{}]", parts.join(" "))
}

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

// ---------- 通用工具 ----------

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
            // 无 IPv6 环境下强制走 IPv4（避免解析到 v6 后连接立即失败）。
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
                ToolChoice::Function { name } => json!({
                    "type": "function",
                    "name": name,
                }),
            };
            // API 限制：thinking 模式不支持 tool_choice，强制调用时关闭思考。
            body["reasoning"] = json!({"effort": "none"});
        }
        Ok(body)
    }

    /// 兜底请求体：剥离全部 reasoning + thinking 关闭。
    /// DeepSeek 校验失败（reasoning_text 未回传）时重试一次，宁可丢思考连续性也不让回合死掉。
    fn build_body_no_reasoning(&self, request: &ModelRequest) -> Result<Value, ModelError> {
        let mut body = json!({
            "model": self.model,
            "input": messages_to_responses_input_no_reasoning(&request.messages)?,
            "stream": true,
            "reasoning": {"effort": "none"},
        });
        if let Some(tools) = &request.tools {
            body["tools"] = json!(tools.iter().map(tool_to_function).collect::<Vec<_>>());
        }
        if let Some(fmt) = &request.response_format {
            body["text"] = json!({"format": text_format(fmt)});
        }
        Ok(body)
    }

    async fn post(&self, url: &str, body: &Value, turn_id: &str) -> Result<reqwest::Response, ModelError> {
        tokio::time::timeout(
            Duration::from_secs(60),
            self.client
                .post(url)
                .bearer_auth(&self.api_key)
                // 回合标识（ADR-0047 修订 R14）：服务端据此把同一回合的多次往返并为一次扣次
                .header("x-ma-turn-id", turn_id)
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
        let turn_id = crate::kernel::plugin::model::turn::for_request(
            request.turn_id.as_deref(),
            &request.messages,
        );
        let mut response = self.post(&url, &body, &turn_id).await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            log::warn!(
                "主模型请求被拒（{status}）：{} tools={}",
                summarize_input(&body["input"]),
                body["tools"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or(0),
            );
            let err = map_status_error(status, &text);
            // DeepSeek 回放校验（thinking 开启 + 工具调用）失败时，兜底重试一次：
            // 剥离 reasoning + reasoning.effort=none（实测可过）。不做 LLM 改写——
            // 校验要求 reasoning_content 原样回传，改写只会更糟。
            if let ModelError::Protocol(msg) = &err
                && msg.contains("reasoning_text")
            {
                log::warn!("reasoning 回放被拒，兜底重试：剥离 reasoning + reasoning.effort=none");
                let fallback = self.build_body_no_reasoning(request)?;
                response = self.post(&url, &fallback, &turn_id).await?;
                if !response.status().is_success() {
                    let status2 = response.status();
                    let text2 = response.text().await.unwrap_or_default();
                    log::warn!(
                        "兜底重试仍被拒（{status2}）：{} tools={}",
                        summarize_input(&fallback["input"]),
                        fallback["tools"]
                            .as_array()
                            .map(|a| a.len())
                            .unwrap_or(0),
                    );
                    return Err(map_status_error(status2, &text2));
                }
            } else {
                return Err(err);
            }
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
                                        // DeepSeek Responses 事件里，item 的类型/字段在 `item` 子对象
                                        // （顶层 `type` 是事件名本身），与 OpenAI 文档示例一致。
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
                                            // usage 位于 `response.usage`（顶层无 usage）。
                                            let usage_src = if v["response"]["usage"].is_object() {
                                                &v["response"]["usage"]
                                            } else {
                                                &v["usage"]
                                            };
                                            let usage = parse_usage(usage_src);
                                            let _ = tx.send(Ok(ModelChunk::Usage(usage))).await;
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

fn parse_usage(v: &Value) -> TokenUsage {
    let input_tokens = v["input_tokens"].as_u64();
    let cached_tokens = v["input_tokens_details"]["cached_tokens"].as_u64();
    TokenUsage {
        input_tokens,
        output_tokens: v["output_tokens"].as_u64(),
        cached_tokens,
        // input_tokens 含缓存命中部分：未命中 = 输入 - 命中。
        cache_miss_tokens: match (input_tokens, cached_tokens) {
            (Some(i), Some(c)) => Some(i.saturating_sub(c)),
            _ => None,
        },
        reasoning_tokens: v["output_tokens_details"]["reasoning_tokens"].as_u64(),
    }
}

// ---------- SiliconFlow / Ollama Chat Completions 适配器（视觉模型） ----------
