use std::time::Duration;

use reqwest::Client;
use serde_json::{Value, json};

use crate::kernel::plugin::services::{
    AbortSignal, ModelChunk, ModelError, ModelRequest, ModelResponse, ModelService, ModelStream,
    TokenUsage,
};

use super::*;

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
                // Chat Completions 兜底：json_schema 降级为 json_object（判分走 Main/Responses，不依赖此路径）。
                ResponseFormat::JsonSchema { .. } => json!({"type": "json_object"}),
            };
        }
        let turn_id = crate::kernel::plugin::model::turn::for_request(
            request.turn_id.as_deref(),
            &request.messages,
        );
        let response = match tokio::time::timeout(
            Duration::from_secs(180),
            self.client
                .post(&url)
                .bearer_auth(&self.api_key)
                // 回合标识（ADR-0047 修订 R14）
                .header("x-ma-turn-id", &turn_id)
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

// ---------- 构建函数 ----------
