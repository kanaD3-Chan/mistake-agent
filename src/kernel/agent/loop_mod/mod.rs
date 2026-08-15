//! agent loop（M1）：单 agent 主循环、护栏、压缩、中断消费。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::kernel::agent::dispatch::{Caller, Dispatch};
use crate::kernel::agent::session::{InterruptBus, SessionKey, SessionSwitch, Summarizer};
use crate::kernel::audit::{AuditRecord, Auditor};
use crate::kernel::contract::{ToolError, ToolErrorCode};
use crate::kernel::events::{Event, EventSink};
use crate::kernel::message::{Message, MessageId, MessageKind, append_to_path};
use crate::kernel::plugin::services::{
    ItemKind, ModelChunk, ModelKind, ModelRequest, ModelService, TokenUsage, ToolChoice,
};

/// 系统提示提供者：人格/教学规则注入点，替代 loop 直接调用静态 prompt。
pub type SystemPromptProvider = Arc<dyn Fn() -> String + Send + Sync>;

mod turn;

pub use turn::{CompactionInfo, InterruptReason, LoopError, StopReason, TurnInput, TurnOutcome};

struct ToolCallAcc {
    name: String,
    arguments: String,
    call_id: String,
}

/// 累加一次流调用的 token 用量到回合级累计器。
fn add_usage(acc: &mut TokenUsage, u: &TokenUsage) {
    acc.input_tokens = Some(acc.input_tokens.unwrap_or(0) + u.input_tokens.unwrap_or(0));
    acc.output_tokens = Some(acc.output_tokens.unwrap_or(0) + u.output_tokens.unwrap_or(0));
    acc.cached_tokens = Some(acc.cached_tokens.unwrap_or(0) + u.cached_tokens.unwrap_or(0));
    acc.cache_miss_tokens =
        Some(acc.cache_miss_tokens.unwrap_or(0) + u.cache_miss_tokens.unwrap_or(0));
    acc.reasoning_tokens =
        Some(acc.reasoning_tokens.unwrap_or(0) + u.reasoning_tokens.unwrap_or(0));
}

/// 没有任何输入 token 信息时返回 None（缓存统计会跳过空 usage）。
fn usage_opt(u: &TokenUsage) -> Option<TokenUsage> {
    if u.input_tokens.is_none() && u.cached_tokens.is_none() && u.cache_miss_tokens.is_none() {
        None
    } else {
        Some(u.clone())
    }
}

#[cfg(test)]
mod tests;

pub struct AgentLoop {
    model: Arc<dyn ModelService>,
    dispatch: Arc<Dispatch>,
    auditor: Auditor,
    events: Arc<dyn EventSink>,
    max_tool_calls: usize,
    max_consecutive_failures: usize,
    summarizer: Arc<dyn Summarizer>,
    bus: InterruptBus,
    system_prompt: SystemPromptProvider,
    /// 压缩阈值（按 token 粗估：字符数/2；达 75% 触发）。
    context_limit_tokens: usize,
    /// 压缩时保留的最近消息条数。
    compaction_keep_last: usize,
    /// 回合内主动切换会话（session::switch 工具）。
    switcher: Option<Arc<dyn SessionSwitch>>,
}

impl AgentLoop {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: Arc<dyn ModelService>,
        dispatch: Arc<Dispatch>,
        auditor: Auditor,
        events: Arc<dyn EventSink>,
        summarizer: Arc<dyn Summarizer>,
        bus: InterruptBus,
        system_prompt: SystemPromptProvider,
        switcher: Option<Arc<dyn SessionSwitch>>,
    ) -> Self {
        Self {
            model,
            dispatch,
            auditor,
            events,
            max_tool_calls: 25,
            max_consecutive_failures: 3,
            summarizer,
            bus,
            system_prompt,
            context_limit_tokens: 131_072,
            compaction_keep_last: 15,
            switcher,
        }
    }

    #[cfg(test)]
    pub fn with_compaction_limits(
        mut self,
        context_limit_tokens: usize,
        compaction_keep_last: usize,
    ) -> Self {
        self.context_limit_tokens = context_limit_tokens;
        self.compaction_keep_last = compaction_keep_last;
        self
    }

    pub async fn run_turn(&self, input: TurnInput) -> Result<TurnOutcome, LoopError> {
        // 回合边界消费环境变更中断（ADR-0023）：记录审计，上下文重组由调度层完成。
        for interrupt in self.bus.take_all() {
            log::info!("回合边界消费中断：{interrupt:?}");
            self.auditor.record(AuditRecord::Interrupt {
                name: interrupt_name(&interrupt),
                reason: format!("{interrupt:?}"),
            });
        }

        let preexisting: std::collections::HashSet<MessageId> =
            input.messages.iter().map(|m| m.id).collect();
        let mut conversation = input.messages;
        let turn_deadline = Instant::now() + input.turn_budget;
        let mut tool_calls = 0usize;
        let mut current_session: Option<SessionKey> = None;
        let mut consecutive_failures = 0usize;
        let mut last_code: Option<ToolErrorCode> = None;
        let mut remaining_forced = input.forced_tool.clone();
        let mut turn_usage = TokenUsage::default();
        // 强制调用回合全程关闭思考模式：Responses API 要求 thinking 的
        // reasoning_text 必须随历史回传，混用 none/thinking 会协议报错。
        let reasoning_off = input.forced_tool.is_some();

        let stop_reason = loop {
            if input.signal.is_cancelled() {
                break StopReason::UserAborted;
            }
            if Instant::now() >= turn_deadline {
                break StopReason::TurnTimeout;
            }

            // 系统提示每次请求注入（不落消息树），保证无状态 API 拿到完整人格设定。
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
            let started = Instant::now();
            let mut stream = match self.model.stream(&request, &input.signal).await {
                Ok(s) => s,
                // 瞬时错误（503/限流/传输）重试一次；系统性错误与取消不重试。
                Err(e)
                    if !e.is_systemic()
                        && !matches!(e, crate::kernel::plugin::services::ModelError::Cancelled) =>
                {
                    log::warn!("主模型流失败，1 次重试：{e}");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    if input.signal.is_cancelled() {
                        break StopReason::UserAborted;
                    }
                    match self.model.stream(&request, &input.signal).await {
                        Ok(s) => s,
                        Err(e2) => {
                            self.auditor.record(AuditRecord::LlmCall {
                                provider: "main".into(),
                                model: "main".into(),
                                kind: "stream".into(),
                                tokens_in: None,
                                tokens_out: None,
                                duration_ms: started.elapsed().as_millis() as u64,
                                ok: false,
                            });
                            if e2.is_systemic() {
                                break StopReason::InternalAbort {
                                    reason: InterruptReason::ModelUnavailable,
                                };
                            }
                            if matches!(e2, crate::kernel::plugin::services::ModelError::Cancelled)
                            {
                                break StopReason::UserAborted;
                            }
                            return Err(LoopError::Model(e2.to_string()));
                        }
                    }
                }
                Err(e) => {
                    self.auditor.record(AuditRecord::LlmCall {
                        provider: "main".into(),
                        model: "main".into(),
                        kind: "stream".into(),
                        tokens_in: None,
                        tokens_out: None,
                        duration_ms: started.elapsed().as_millis() as u64,
                        ok: false,
                    });
                    if e.is_systemic() {
                        break StopReason::InternalAbort {
                            reason: InterruptReason::ModelUnavailable,
                        };
                    }
                    if matches!(e, crate::kernel::plugin::services::ModelError::Cancelled) {
                        break StopReason::UserAborted;
                    }
                    return Err(LoopError::Model(e.to_string()));
                }
            };

            let mut pending_bubble: Option<Message> = None;
            let mut pending_reasoning: Option<Message> = None;
            let mut calls: BTreeMap<usize, ToolCallAcc> = BTreeMap::new();
            let mut calls_done: Vec<(usize, ToolCallAcc)> = Vec::new();
            let mut usage: Option<TokenUsage> = None;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(ModelChunk::TextDelta(delta)) => {
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
                    Ok(ModelChunk::ReasoningDelta(delta)) => {
                        if let Some(r) = pending_reasoning.as_mut()
                            && let MessageKind::Reasoning { text, .. } = &mut r.kind
                        {
                            text.push_str(&delta);
                        } else {
                            // 防御：delta 先于 output_item.added(reasoning) 到达时，
                            // 先占位累积，避免推理文本丢失导致下一轮回传校验失败。
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
                    Ok(ModelChunk::ReasoningItemStart { id }) => {
                        if let Some(mut r) = pending_reasoning.take()
                            && let MessageKind::Reasoning { id: rid, .. } = &mut r.kind
                        {
                            // delta 先到达时保留已累积文本，只补上真实 item id。
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
                    Ok(ModelChunk::ToolCallStart {
                        index,
                        call_id,
                        name,
                    }) => {
                        calls.insert(
                            index,
                            ToolCallAcc {
                                name,
                                arguments: String::new(),
                                call_id,
                            },
                        );
                    }
                    Ok(ModelChunk::ToolCallDelta { index, data }) => {
                        if let Some(acc) = calls.get_mut(&index) {
                            acc.arguments.push_str(&data);
                        }
                    }
                    Ok(ModelChunk::ItemDone {
                        kind: ItemKind::Message,
                    }) => {
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
                    Ok(ModelChunk::ItemDone {
                        kind: ItemKind::FunctionCall,
                    }) => {
                        // 收集完成顺序（BTreeMap 已按 index 排序，执行按输出顺序串行）。
                        if let Some((idx, acc)) = calls.pop_first() {
                            calls_done.push((idx, acc));
                        }
                    }
                    Ok(ModelChunk::ItemDone {
                        kind: ItemKind::Reasoning,
                    }) => {
                        // 推理 item 必须按 id 回传，无论文本是否为空都要保留。
                        if let Some(r) = pending_reasoning.take() {
                            append_to_path(&mut conversation, r);
                        }
                    }
                    Ok(ModelChunk::Usage(u)) => {
                        add_usage(&mut turn_usage, &u);
                        usage = Some(u);
                    }
                    Ok(ModelChunk::Done) => break,
                    Err(e) => {
                        self.auditor.record(AuditRecord::LlmCall {
                            provider: "main".into(),
                            model: "main".into(),
                            kind: "stream".into(),
                            tokens_in: usage.as_ref().and_then(|u| u.input_tokens),
                            tokens_out: usage.as_ref().and_then(|u| u.output_tokens),
                            duration_ms: started.elapsed().as_millis() as u64,
                            ok: false,
                        });
                        if e.is_systemic() {
                            return Ok(TurnOutcome {
                                messages: new_messages(&conversation, &preexisting),
                                stop_reason: StopReason::InternalAbort {
                                    reason: InterruptReason::ModelUnavailable,
                                },
                                tool_calls,
                                compaction: None,
                                usage: usage_opt(&turn_usage),
                                session_key: current_session,
                            });
                        }
                        return Err(LoopError::Model(e.to_string()));
                    }
                }
            }

            // 流被截断时补收尾：未关闭的气泡不落盘，半截调用丢弃。
            self.auditor.record(AuditRecord::LlmCall {
                provider: "main".into(),
                model: "main".into(),
                kind: "stream".into(),
                tokens_in: usage.as_ref().and_then(|u| u.input_tokens),
                tokens_out: usage.as_ref().and_then(|u| u.output_tokens),
                duration_ms: started.elapsed().as_millis() as u64,
                ok: true,
            });

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
                let params: Value = serde_json::from_str(&acc.arguments).unwrap_or(Value::Null);
                self.events.emit(Event::ToolStart {
                    entry: full_name.clone(),
                    icon: self.dispatch.entry_icon(&full_name),
                });
                let result = if full_name.is_empty() {
                    Err(ToolError::unknown_tool(&wire_name))
                } else if full_name == "session::switch" {
                    // 回合内主动切换（ADR-0030）：执行会话切换并回填新会话键。
                    match &self.switcher {
                        Some(s) => {
                            let goal = params
                                .get("goal")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            match s.switch(&goal).await {
                                Ok(key) => {
                                    current_session = Some(key);
                                    Ok(json!({
                                        "switched": true,
                                        "session_key": key.to_string(),
                                    }))
                                }
                                Err(e) => Err(ToolError::handler(e)),
                            }
                        }
                        None => Err(ToolError::handler("会话切换不可用")),
                    }
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
                                Message::tool_call_with_id(
                                    full_name,
                                    params,
                                    result,
                                    acc.call_id.clone(),
                                ),
                            );
                            stop = Some(StopReason::ConsecutiveFailures);
                            break;
                        }
                    }
                }
                append_to_path(
                    &mut conversation,
                    Message::tool_call_with_id(full_name, params, result, acc.call_id.clone()),
                );
            }
            if let Some(s) = stop {
                break s;
            }
        };

        let compaction = self.maybe_compact(&mut conversation).await;
        let outcome = TurnOutcome {
            messages: new_messages(&conversation, &preexisting),
            stop_reason,
            tool_calls,
            compaction,
            usage: usage_opt(&turn_usage),
            session_key: current_session,
        };
        // TurnEnd 事件不在 loop 内发：由 RPC 层在消息落盘 + active_path 更新后发出
        // （否则前端收到 turn_end 立刻回读会话时，活跃路径还是旧的，新消息会从链上消失）。
        self.auditor.record(AuditRecord::TurnEnded {
            stop_reason: format!("{:?}", outcome.stop_reason),
            tool_calls,
        });
        Ok(outcome)
    }

    /// 上下文用量 ≥ 窗口 75% 时压缩：最近 N 条不压，其余交给摘要器；
    /// 摘要为空则重试一次，仍失败就下回合再试（原始消息仍在会话 JSONL）。
    async fn maybe_compact(&self, conversation: &mut Vec<Message>) -> Option<CompactionInfo> {
        let total_chars: usize = conversation.iter().map(message_chars).sum();
        let est_tokens = total_chars / 2 + 1;
        if est_tokens < self.context_limit_tokens * 3 / 4 {
            return None;
        }
        let keep_from = conversation.len().saturating_sub(self.compaction_keep_last);
        if keep_from == 0 {
            return None;
        }
        let to_compact = conversation[..keep_from].to_vec();
        let mut summary = self.summarizer.summarize(&to_compact, None).await;
        if summary.trim().is_empty() {
            summary = self.summarizer.summarize(&to_compact, None).await;
        }
        let summary = summary.trim();
        if summary.is_empty() {
            log::warn!("压缩摘要为空，下回合再试");
            return None;
        }
        let mut tail = conversation[keep_from..].to_vec();
        let tail_start = tail.first().map(|m| m.id)?;
        let tail_end = tail.last().map(|m| m.id)?;
        let mut sys = Message::system(format!("上下文压缩摘要：{summary}"));
        sys.parent_id = None;
        // 内存链同步：保留段首条挂到摘要下，旧前缀从活跃路径剔除（JSONL 原样保留）。
        if let Some(first) = tail.first_mut() {
            first.parent_id = Some(sys.id);
        }
        let info = CompactionInfo {
            summary: sys.clone(),
            tail_start,
            summarized: to_compact.len(),
            tail_end,
        };
        *conversation = std::iter::once(sys).chain(tail).collect();
        Some(info)
    }
}

fn interrupt_name(interrupt: &crate::kernel::agent::session::Interrupt) -> String {
    match interrupt {
        crate::kernel::agent::session::Interrupt::SessionSwitched { .. } => "session_switched",
        crate::kernel::agent::session::Interrupt::GoalUpdated { .. } => "goal_updated",
        crate::kernel::agent::session::Interrupt::ConfigChanged => "config_changed",
        crate::kernel::agent::session::Interrupt::MemoryChanged { .. } => "memory_changed",
        crate::kernel::agent::session::Interrupt::CompactionDone { .. } => "compaction_done",
    }
    .into()
}

fn message_chars(msg: &Message) -> usize {
    match &msg.kind {
        MessageKind::User { text, .. }
        | MessageKind::Assistant { text }
        | MessageKind::System { text, .. } => text.len(),
        MessageKind::Reasoning { text, .. } => text.len(),
        MessageKind::ToolCall { entry, params, .. } => {
            entry.len() + serde_json::to_string(params).unwrap_or_default().len()
        }
    }
}

/// 本回合新增的消息（排除回合开始前已存在的消息；压缩摘要也算新增）。
fn new_messages(
    conversation: &[Message],
    preexisting: &std::collections::HashSet<MessageId>,
) -> Vec<Message> {
    conversation
        .iter()
        .filter(|m| !preexisting.contains(&m.id))
        .cloned()
        .collect()
}
