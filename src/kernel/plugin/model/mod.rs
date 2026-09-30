//! model 服务适配器（ADR-0020/0045）：
//! - 主链路：DeepSeek Responses API（POST /responses，SSE 语义事件，无状态，支持 input_image）；
//! - 兼容回退：Ollama 等不兼容端点经 transport=chat_completions 走 Chat Completions。

use std::error::Error;

use serde_json::{Value, json};

use crate::kernel::contract::full_to_wire;
use crate::kernel::message::{Message, MessageKind};
use crate::kernel::plugin::services::{ModelError, ResponseFormat, ToolSchema};

// ---------- SSE 解析（Responses API 语义事件） ----------

mod chat;
mod responses;
mod routing;
pub(crate) mod turn;

pub use chat::ChatCompletionsModelService;
pub use responses::ResponsesModelService;
pub use routing::{
    AttachmentResolvingModelService, LiveSettingsModelService, build_main_service,
    effective_config,
};

// ---------- 内核插件入口（ADR-0035）：model 服务身份声明 ----------
//
// 服务实例（LiveSettingsModelService）由 Kernel::new 引导构造（依赖 settings 热更新），
// 注册表侧只声明 ServiceId 提供与 namespace 占用。

use crate::kernel::context::KernelContext;
use crate::kernel::contract::{Info, PluginError};
use crate::kernel::plugin::services::ServiceId;
use crate::kernel::registry::{KernelDescriptor, KernelPlugin};

pub struct ModelPlugin;

impl KernelPlugin for ModelPlugin {
    fn info() -> Info {
        Info {
            namespace: "model".into(),
            provides: vec![ServiceId::Model],
            ..Default::default()
        }
    }

    fn register(_ctx: KernelContext<'_>) -> Result<(), PluginError> {
        Ok(())
    }
}

pub fn descriptor() -> KernelDescriptor {
    KernelDescriptor::from_plugin::<ModelPlugin>()
}

pub(crate) fn responses_endpoint(api_url: &str) -> String {
    let base = api_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    format!("{base}/responses")
}

pub(crate) fn text_format(fmt: &ResponseFormat) -> Value {
    match fmt {
        ResponseFormat::JsonObject => json!({"type": "json_object"}),
        ResponseFormat::JsonSchema { name, schema } => json!({
            "type": "json_schema",
            "name": name,
            "schema": schema,
        }),
    }
}

pub(crate) fn parse_delta(data: &str) -> String {
    serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|v| v["delta"].as_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

pub(crate) fn tool_to_function(t: &ToolSchema) -> Value {
    json!({
        "type": "function",
        "name": t.name,
        "description": t.description,
        "parameters": t.input_schema,
    })
}

/// 内部 Message 树 → Responses API input items。
/// ToolCall 一条消息展开为 function_call + function_call_output（call_id = 消息 id）；
/// User 附件展开为 `input_image`（base64 data URL），由 deepseek-flash 直接理解。
pub(crate) fn messages_to_responses_input(messages: &[Message]) -> Result<Vec<Value>, ModelError> {
    messages_to_responses_input_impl(messages, true)
}

/// 兜底（reasoning_text 回传校验失败时）：剥离全部 reasoning item，
/// 请求方同时把 thinking 关掉（reasoning.effort=none），调用不再要求回传。
pub(crate) fn messages_to_responses_input_no_reasoning(
    messages: &[Message],
) -> Result<Vec<Value>, ModelError> {
    messages_to_responses_input_impl(messages, false)
}

fn messages_to_responses_input_impl(
    messages: &[Message],
    include_reasoning: bool,
) -> Result<Vec<Value>, ModelError> {
    let mut items = Vec::new();
    // DeepSeek 回放校验：thinking 开启时，输入里每个 function_call 前都必须紧跟 reasoning item。
    // 模型一次输出可带一个 reasoning + 多个并行调用，回放时按调用复制该 reasoning（实测必要）。
    let mut pending_reasoning: Option<(String, String)> = None;
    let mut calls_since_reasoning = 0usize;
    for msg in messages {
        match &msg.kind {
            MessageKind::User {
                text,
                attachments,
                ..
            } => {
                pending_reasoning = None;
                calls_since_reasoning = 0;
                let mut content: Vec<Value> = Vec::new();
                for att in attachments {
                    content.push(json!({
                        "type": "input_image",
                        "image_url": format!("data:{};base64,{}", att.mime, att.data_base64),
                        "detail": "high",
                    }));
                }
                if !text.is_empty() || content.is_empty() {
                    content.push(json!({"type": "input_text", "text": text}));
                }
                items.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": content,
                }));
            }
            MessageKind::Assistant { text } => items.push(json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": text}],
            })),
            MessageKind::System { text, .. } => {
                pending_reasoning = None;
                calls_since_reasoning = 0;
                items.push(json!({
                    "type": "message",
                    "role": "system",
                    "content": [{"type": "input_text", "text": text}],
                }));
            }
            // thinking 模式要求把推理 item 按 id 回传，且必须带上推理文本：
            // DeepSeek 只消费明文 content（并入相邻 assistant 消息），不消费 summary；
            // 但输出侧 reasoning_content 又落在 summary 里，所以两者都带，保证校验通过。
            MessageKind::Reasoning { id, text } => {
                pending_reasoning = Some((id.clone(), text.clone()));
                calls_since_reasoning = 0;
                if include_reasoning {
                    items.push(reasoning_item(id, text));
                }
            }
            MessageKind::ToolCall {
                entry,
                params,
                result,
                call_id,
            } => {
                // 并行调用：第二个及之后的 function_call 前补一份同轮 reasoning（DeepSeek 实测要求）。
                if include_reasoning
                    && let Some((rid, rtext)) = &pending_reasoning
                    && calls_since_reasoning > 0
                {
                    items.push(reasoning_item(rid, rtext));
                }
                calls_since_reasoning += 1;
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

fn reasoning_item(id: &str, text: &str) -> Value {
    json!({
        "type": "reasoning",
        "id": id,
        "summary": [{"type": "summary_text", "text": text}],
        "content": [{"type": "reasoning_text", "text": text}],
    })
}

/// 内部 Message 树 → Chat Completions messages（视觉模型支持 image_url base64）。
pub(crate) fn messages_to_cc(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for msg in messages {
        match &msg.kind {
            MessageKind::User {
                text,
                attachments,
                ..
            } => {
                let mut content: Vec<Value> = Vec::new();
                for att in attachments {
                    content.push(json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", att.mime, att.data_base64),
                            "detail": "high",
                        },
                    }));
                }
                if !text.is_empty() || content.is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
                out.push(json!({"role": "user", "content": content}));
            }
            MessageKind::Assistant { text } => {
                out.push(json!({"role": "assistant", "content": text}));
            }
            MessageKind::System { text, .. } => {
                out.push(json!({"role": "system", "content": text}));
            }
            // Chat Completions 无 reasoning 概念：忽略（Ollama 等兼容端）。
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
                        "function": {
                            "name": full_to_wire(entry),
                            "arguments": arguments,
                        },
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

pub(crate) fn map_status_error(status: reqwest::StatusCode, body: &str) -> ModelError {
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

pub(crate) fn reqwest_chain(e: &reqwest::Error) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(&format!(" <- {s}"));
        src = s.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::message::Attachment;

    #[test]
    fn responses_input_expands_user_attachments_as_input_image() {
        let mut msg = Message::user("看图");
        if let MessageKind::User { attachments, .. } = &mut msg.kind {
            attachments.push(Attachment {
                mime: "image/png".into(),
                data_base64: "AAAA".into(),
            });
        }
        let items = messages_to_responses_input(&[msg]).unwrap();
        assert_eq!(items.len(), 1);
        let content = items[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "input_image");
        assert_eq!(content[0]["image_url"], "data:image/png;base64,AAAA");
        assert_eq!(content[1]["type"], "input_text");
        assert_eq!(content[1]["text"], "看图");
    }

    #[test]
    fn reasoning_item_replays_text_with_id() {
        let mut msg = Message::system("占位");
        msg.kind = MessageKind::Reasoning {
            id: "rs_1".into(),
            text: "先计算再调用工具".into(),
        };
        let items = messages_to_responses_input(&[msg]).unwrap();
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item["type"], "reasoning");
        assert_eq!(item["id"], "rs_1");
        // DeepSeek 只消费明文 content，校验按 reasoning_text 查重放；
        // summary 兜底（DeepSeek 输出侧 reasoning_content 落在 summary）。
        assert_eq!(item["content"][0]["type"], "reasoning_text");
        assert_eq!(item["content"][0]["text"], "先计算再调用工具");
        assert_eq!(item["summary"][0]["type"], "summary_text");
        assert_eq!(item["summary"][0]["text"], "先计算再调用工具");
    }

    #[test]
    fn parallel_calls_repeat_reasoning_per_call() {
        let mut reasoning = Message::system("占位");
        reasoning.kind = MessageKind::Reasoning {
            id: "rs_1".into(),
            text: "并行读三张图".into(),
        };
        let call = |i: u32, cid: &str| {
            Message::tool_call_with_id(
                "vision::read",
                json!({"file": format!("/tmp/p{i}.png")}),
                Ok(json!({"ok": true})),
                cid.into(),
            )
        };
        let messages = vec![
            Message::user("都看看"),
            reasoning,
            call(1, "call_00"),
            call(2, "call_01"),
            call(3, "call_02"),
        ];
        let items = messages_to_responses_input(&messages).unwrap();
        let kinds: Vec<&str> = items.iter().map(|i| i["type"].as_str().unwrap()).collect();
        // 期望：reasoning、call、output、reasoning、call、output、reasoning、call、output
        assert_eq!(
            kinds,
            vec![
                "message",
                "reasoning",
                "function_call",
                "function_call_output",
                "reasoning",
                "function_call",
                "function_call_output",
                "reasoning",
                "function_call",
                "function_call_output",
            ]
        );
        // 复制出来的 reasoning 与原件同 id 同文本。
        assert_eq!(items[4]["id"], "rs_1");
        assert_eq!(items[4]["content"][0]["text"], "并行读三张图");
        assert_eq!(items[7]["id"], "rs_1");
    }

    #[test]
    fn no_reasoning_variant_strips_reasoning_items() {
        let mut reasoning = Message::system("占位");
        reasoning.kind = MessageKind::Reasoning {
            id: "rs_1".into(),
            text: "并行读图".into(),
        };
        let call = Message::tool_call_with_id(
            "vision::read",
            json!({"file": "/tmp/p1.png"}),
            Ok(json!({"ok": true})),
            "call_00".into(),
        );
        let messages = vec![Message::user("都看看"), reasoning, call];
        let items = messages_to_responses_input_no_reasoning(&messages).unwrap();
        let kinds: Vec<&str> = items.iter().map(|i| i["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            vec!["message", "function_call", "function_call_output"]
        );
    }
}

// ---------- DeepSeek Responses API 适配器 ----------
