//! Agent loop：LLM 唯一决策者，kernel 执行工具调用并保证护栏。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{AuditRecord, Auditor};
use crate::contract::{ToolError, ToolErrorCode};
use crate::dispatch::{Caller, Dispatch};
use crate::events::{Event, EventSink};
use crate::message::{Message, MessageId, MessageKind, append_to_path};
use crate::services::{
    AbortSignal, ItemKind, ModelChunk, ModelKind, ModelRequest, ModelService, ToolChoice,
    ToolSchema,
};

pub type SystemPromptProvider = Arc<dyn Fn() -> String + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Natural,
    ToolCallLimit,
    ConsecutiveFailures,
    TurnTimeout,
    UserAborted,
    Failed,
}

pub struct TurnInput {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub signal: AbortSignal,
    pub turn_budget: Duration,
    pub forced_tool: Option<String>,
}

#[derive(Debug)]
pub struct TurnOutcome {
    pub messages: Vec<Message>,
    pub stop_reason: StopReason,
    pub tool_calls: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum LoopError {
    #[error("模型错误：{0}")]
    Model(String),
    #[error("内部错误：{0}")]
    Internal(String),
}

struct ToolCallAcc {
    name: String,
    arguments: String,
}

pub struct AgentLoop {
    model: Arc<dyn ModelService>,
    dispatch: Arc<Dispatch>,
    auditor: Auditor,
    events: Arc<dyn EventSink>,
    system_prompt: SystemPromptProvider,
    max_tool_calls: usize,
    max_consecutive_failures: usize,
}

impl AgentLoop {
    pub fn new(
        model: Arc<dyn ModelService>,
        dispatch: Arc<Dispatch>,
        auditor: Auditor,
        events: Arc<dyn EventSink>,
        system_prompt: SystemPromptProvider,
    ) -> Self {
        Self {
            model,
            dispatch,
            auditor,
            events,
            system_prompt,
            max_tool_calls: 25,
            max_consecutive_failures: 3,
        }
    }

    pub async fn run_turn(&self, input: TurnInput) -> Result<TurnOutcome, LoopError> {
        let preexisting: std::collections::HashSet<MessageId> =
            input.messages.iter().map(|m| m.id).collect();
        let mut conversation = input.messages;
        let turn_deadline = std::time::Instant::now() + input.turn_budget;
        let mut tool_calls = 0usize;
        let mut consecutive_failures = 0usize;
        let mut last_code: Option<ToolErrorCode> = None;
        let mut remaining_forced = input.forced_tool.clone();
        let reasoning_off = input.forced_tool.is_some();

        let stop_reason = loop {
            if input.signal.is_cancelled() {
                break StopReason::UserAborted;
            }
            if std::time::Instant::now() >= turn_deadline {
                break StopReason::TurnTimeout;
            }

            let mut req_messages = vec![Message::system((self.system_prompt)())];
            req_messages.extend(conversation.iter().cloned());
            let mut request = ModelRequest {
                model: ModelKind::Main,
                messages: req_messages,
                tools: Some(input.tools.clone()),
                reasoning_effort: if reasoning_off {
                    Some("none".into())
                } else {
                    None
                },
                response_format: None,
                tool_choice: None,
            };
            if let Some(wire) = remaining_forced.take() {
                request.tool_choice = Some(ToolChoice::Function { name: wire });
            }

            let mut stream = self
                .model
                .stream(&request, &input.signal)
                .await
                .map_err(|e| LoopError::Model(e.to_string()))?;
            let mut pending_bubble: Option<Message> = None;
            let mut pending_reasoning: Option<Message> = None;
            let mut calls: BTreeMap<usize, ToolCallAcc> = BTreeMap::new();
            let mut calls_done: Vec<(usize, ToolCallAcc)> = Vec::new();
            while let Some(chunk) = stream.next().await {
                match chunk.map_err(|e| LoopError::Model(e.to_string()))? {
                    ModelChunk::TextDelta(delta) => {
                        let entry = pending_bubble.get_or_insert_with(|| Message {
                            id: MessageId::new(),
                            parent_id: None,
                            kind: MessageKind::Assistant {
                                text: String::new(),
                            },
                            created_at: chrono::Utc::now(),
                        });
                        if let MessageKind::Assistant { text } = &mut entry.kind {
                            text.push_str(&delta);
                        }
                        self.events.emit(Event::MessageDelta {
                            message_id: entry.id,
                            delta,
                        });
                    }
                    ModelChunk::ReasoningDelta(delta) => {
                        if let Some(r) = pending_reasoning.as_mut()
                            && let MessageKind::Reasoning { text, .. } = &mut r.kind
                        {
                            text.push_str(&delta);
                        } else {
                            pending_reasoning = Some(Message {
                                id: MessageId::new(),
                                parent_id: None,
                                kind: MessageKind::Reasoning {
                                    id: MessageId::new().to_string(),
                                    text: delta.clone(),
                                },
                                created_at: chrono::Utc::now(),
                            });
                        }
                        self.events.emit(Event::ReasoningDelta { delta });
                    }
                    ModelChunk::ReasoningItemStart { id } => {
                        if let Some(mut r) = pending_reasoning.take()
                            && let MessageKind::Reasoning { id: rid, .. } = &mut r.kind
                        {
                            *rid = id;
                            pending_reasoning = Some(r);
                        } else {
                            pending_reasoning = Some(Message {
                                id: MessageId::new(),
                                parent_id: None,
                                kind: MessageKind::Reasoning {
                                    id,
                                    text: String::new(),
                                },
                                created_at: chrono::Utc::now(),
                            });
                        }
                    }
                    ModelChunk::ToolCallStart {
                        index,
                        call_id: _,
                        name,
                    } => {
                        calls.insert(
                            index,
                            ToolCallAcc {
                                name,
                                arguments: String::new(),
                            },
                        );
                    }
                    ModelChunk::ToolCallDelta { index, data } => {
                        if let Some(acc) = calls.get_mut(&index) {
                            acc.arguments.push_str(&data);
                        }
                    }
                    ModelChunk::ItemDone {
                        kind: ItemKind::Message,
                    } => {
                        if let Some(bubble) = pending_bubble.take() {
                            let text = match &bubble.kind {
                                MessageKind::Assistant { text } => text.clone(),
                                _ => String::new(),
                            };
                            if !text.is_empty() {
                                self.auditor.record(AuditRecord::MessageCompleted {
                                    message_id: bubble.id,
                                });
                                append_to_path(&mut conversation, bubble);
                            }
                        }
                    }
                    ModelChunk::ItemDone {
                        kind: ItemKind::FunctionCall,
                    } => {
                        if let Some((idx, acc)) = calls.pop_first() {
                            calls_done.push((idx, acc));
                        }
                    }
                    ModelChunk::ItemDone {
                        kind: ItemKind::Reasoning,
                    } => {
                        if let Some(r) = pending_reasoning.take() {
                            append_to_path(&mut conversation, r);
                        }
                    }
                    ModelChunk::Usage(_) => {}
                    ModelChunk::Done => break,
                }
            }

            if input.signal.is_cancelled() {
                break StopReason::UserAborted;
            }
            if calls_done.is_empty() {
                break StopReason::Natural;
            }

            let mut stop: Option<StopReason> = None;
            for (_idx, acc) in calls_done {
                tool_calls += 1;
                if tool_calls > self.max_tool_calls {
                    stop = Some(StopReason::ToolCallLimit);
                    break;
                }
                let wire_name = acc.name.clone();
                let full_name = self.dispatch.resolve_wire(&wire_name).unwrap_or_default();
                let params: Value =
                    serde_json::from_str(&acc.arguments).unwrap_or_else(|_| json!({}));
                self.events.emit(Event::ToolStart {
                    entry: full_name.clone(),
                    icon: self.dispatch.entry_icon(&full_name),
                });
                let result = if full_name.is_empty() {
                    Err(ToolError::unknown_tool(&wire_name))
                } else {
                    self.dispatch
                        .call_tool(&full_name, params.clone(), Caller::Model)
                        .await
                };
                self.events.emit(Event::ToolEnd {
                    entry: full_name.clone(),
                    ok: result.is_ok(),
                });

                match &result {
                    Ok(_) => consecutive_failures = 0,
                    Err(e) => {
                        if Some(e.code) == last_code {
                            consecutive_failures += 1;
                        } else {
                            consecutive_failures = 1;
                        }
                        last_code = Some(e.code);
                        if consecutive_failures >= self.max_consecutive_failures {
                            append_to_path(
                                &mut conversation,
                                Message::tool_call(full_name, params, result),
                            );
                            stop = Some(StopReason::ConsecutiveFailures);
                            break;
                        }
                    }
                }
                append_to_path(
                    &mut conversation,
                    Message::tool_call(full_name, params, result),
                );
            }
            if let Some(s) = stop {
                break s;
            }
        };

        self.auditor.record(AuditRecord::TurnEnded {
            stop_reason: format!("{stop_reason:?}"),
            tool_calls,
        });
        Ok(TurnOutcome {
            messages: conversation
                .into_iter()
                .filter(|m| !preexisting.contains(&m.id))
                .collect(),
            stop_reason,
            tool_calls,
        })
    }
}
