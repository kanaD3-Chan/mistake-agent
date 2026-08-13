//! 回合处理与各 Method 分支（persist/start_turn/handle）。

use super::protocol::custom_params;
use super::protocol::*;
use super::*;

pub(crate) async fn persist_turn_messages(
    store: &Arc<dyn SessionStore>,
    key: &SessionKey,
    messages: &[Message],
    skip_id: Option<MessageId>,
) -> Result<Option<MessageId>, String> {
    let mut last_kept: Option<MessageId> = None;
    let mut skipped_switch: Option<MessageId> = None;
    for msg in messages {
        if Some(msg.id) == skip_id {
            continue;
        }
        if msg.is_switch_tool_call() {
            if last_kept.is_none() {
                last_kept = msg.parent_id;
            }
            skipped_switch = Some(msg.id);
            continue;
        }
        let mut m = msg.clone();
        if skipped_switch.is_some_and(|sid| m.parent_id == Some(sid))
            && let Some(anchor) = last_kept
        {
            m.parent_id = Some(anchor);
        }
        store
            .append_message(key, &m)
            .await
            .map_err(|e| e.to_string())?;
        last_kept = Some(m.id);
    }
    Ok(last_kept)
}

pub(crate) struct TurnHandle {
    pub(crate) key: SessionKey,
    pub(crate) signal: AbortSignal,
}

pub(crate) struct KernelState {
    pub(crate) turn: Option<TurnHandle>,
}

impl Kernel {
    /// 处理一个请求；返回需要写回 GUI 的响应帧（事件经 EventSink 另发）。
    pub async fn handle(&self, request: RpcRequest) -> Result<Option<RpcFrame>, RpcError> {
        match request.method {
            WireMethod::Generic(method) => self.handle_generic(request.id, method).await,
            WireMethod::Custom(custom) => {
                let params = custom_params(&custom);
                for ext in &self.extensions {
                    if let Some(result) = ext.handle(&custom.method, params.clone()).await? {
                        return Ok(Some(RpcFrame::Response {
                            id: request.id,
                            result: Some(result),
                            error: None,
                        }));
                    }
                }
                Err(RpcError::new(
                    "unknown_method",
                    format!("未知方法：{}", custom.method),
                ))
            }
        }
    }

    async fn handle_generic(&self, id: u64, method: Method) -> Result<Option<RpcFrame>, RpcError> {
        match method {
            Method::SendUserMessage {
                text,
                force_tool,
                file,
                asset,
            } => {
                {
                    let state = self.state.lock().await;
                    if state.turn.is_some() {
                        return Err(RpcError::new(
                            "turn_in_progress",
                            "当前有回合在跑，请先停止再发送新消息",
                        ));
                    }
                }
                let mut user_text = text.clone();
                let mut display_text: Option<String> = None;
                let mut forced_wire: Option<String> = None;
                if let Some(ft) = force_tool {
                    let entry = self
                        .registry
                        .ensure_tool(&ft.entry)
                        .map_err(|e| RpcError::new("unknown_tool", e.to_string()))?;
                    if entry.policy == CallerPolicy::UserOnly {
                        return Err(RpcError::new(
                            "forbidden_tool",
                            "该工具仅用户可调，不能被模型强制调用",
                        ));
                    }
                    let hint = ft.hint.as_deref().unwrap_or("").trim();
                    user_text = if hint.is_empty() {
                        format!("请调用工具 {} 处理当前请求。", ft.entry)
                    } else {
                        format!("请调用工具 {} 处理：{}", ft.entry, hint)
                    };
                    display_text =
                        ft.display
                            .clone()
                            .filter(|s| !s.trim().is_empty())
                            .or_else(|| {
                                self.registry.entry_title(&ft.entry).map(|title| {
                                    if hint.is_empty() {
                                        title
                                    } else {
                                        format!("{title}：{hint}")
                                    }
                                })
                            });
                    forced_wire = Some(full_to_wire(&ft.entry));
                }
                for f in &file {
                    user_text.push_str(&format!("\n暂存文件：{f}"));
                    display_text.get_or_insert_with(|| text.clone());
                }
                for a in &asset {
                    user_text.push_str(&format!("\n附件：{}|{}", a.path, a.name));
                    display_text.get_or_insert_with(|| text.clone());
                }
                let ctx = self
                    .scheduler
                    .on_new_message_with_display(&user_text, display_text.as_deref())
                    .await
                    .map_err(|e| RpcError::new("scheduler_error", e.to_string()))?;
                let key = ctx.session_key;
                self.start_turn(key, ctx.messages, forced_wire).await?;

                Ok(Some(RpcFrame::Response {
                    id,
                    result: Some(json!({"accepted": true})),
                    error: None,
                }))
            }
            Method::TriggerCommand { entry, params } => {
                let result = self.dispatch.call_command(&entry, params).await;
                let frame = match result {
                    Ok(v) => RpcFrame::Response {
                        id,
                        result: Some(v),
                        error: None,
                    },
                    Err(e) => RpcFrame::Response {
                        id,
                        result: None,
                        error: Some(RpcError::new("tool_error", e.message)),
                    },
                };
                Ok(Some(frame))
            }
            Method::Abort => {
                let state = self.state.lock().await;
                let aborted = if let Some(turn) = &state.turn {
                    turn.signal.cancel();
                    true
                } else {
                    false
                };
                Ok(Some(RpcFrame::Response {
                    id,
                    result: Some(json!({"aborted": aborted})),
                    error: None,
                }))
            }
            Method::GetState => {
                let state = self.state.lock().await;
                let (status, session_key) = match &state.turn {
                    Some(t) => ("busy", Some(t.key.to_string())),
                    None => ("idle", None),
                };
                Ok(Some(RpcFrame::Response {
                    id,
                    result: Some(json!({"status": status, "session_key": session_key})),
                    error: None,
                }))
            }
            Method::ListSessions => {
                let metas = self
                    .store
                    .list_sessions()
                    .await
                    .map_err(|e| RpcError::new("storage_error", e.to_string()))?;
                Ok(Some(RpcFrame::Response {
                    id,
                    result: Some(json!({
                        "sessions": serde_json::to_value(&metas).unwrap_or_default(),
                    })),
                    error: None,
                }))
            }
            Method::ReadSession { key } => {
                let meta = self
                    .store
                    .get_session(&key)
                    .await
                    .map_err(|e| RpcError::new("storage_error", e.to_string()))?;
                let messages = self
                    .store
                    .read_all(&key)
                    .await
                    .map_err(|e| RpcError::new("storage_error", e.to_string()))?;
                Ok(Some(RpcFrame::Response {
                    id,
                    result: Some(json!({
                        "meta": serde_json::to_value(&meta).unwrap_or_default(),
                        "messages": serde_json::to_value(&messages).unwrap_or_default(),
                    })),
                    error: None,
                }))
            }
            Method::ListTools => {
                let tools = self.registry.user_entries();
                Ok(Some(RpcFrame::Response {
                    id,
                    result: Some(json!({ "tools": tools })),
                    error: None,
                }))
            }
            Method::GetRulesStatus => {
                let root = crate::kernel::settings::Settings::data_root();
                let path = root.join("AGENTS.md");
                let (loaded, reason, bytes) = match crate::kernel::prompt::load_agents_md(&root) {
                    Ok(text) => (true, None::<&str>, Some(text.len())),
                    Err(e) => (false, Some(e.reason()), None),
                };
                Ok(Some(RpcFrame::Response {
                    id,
                    result: Some(json!({
                        "loaded": loaded,
                        "path": path.to_string_lossy(),
                        "reason": reason,
                        "bytes": bytes,
                    })),
                    error: None,
                }))
            }
            Method::EditMessage { message_id, text } => {
                let key = self.active_session_key().await?;
                let path = self
                    .store
                    .derive_branch(&key, message_id, &text)
                    .await
                    .map_err(|e| RpcError::new("branch_error", e.to_string()))?;
                let branch_id = path.last().map(|m| m.id).unwrap_or(message_id);
                self.auditor.record(AuditRecord::MessageEdited {
                    message_id,
                    branch_id,
                });
                self.start_turn(key, path.clone(), None).await?;
                Ok(Some(RpcFrame::Response {
                    id,
                    result: Some(json!({
                        "session_key": key,
                        "messages": serde_json::to_value(&path).unwrap_or_default(),
                    })),
                    error: None,
                }))
            }
            Method::SwitchBranch { message_id } => {
                let key = self.active_session_key().await?;
                let path = self
                    .store
                    .switch_branch(&key, message_id)
                    .await
                    .map_err(|e| RpcError::new("branch_error", e.to_string()))?;
                self.auditor
                    .record(AuditRecord::BranchSwitched { message_id });
                Ok(Some(RpcFrame::Response {
                    id,
                    result: Some(json!({
                        "session_key": key,
                        "messages": serde_json::to_value(&path).unwrap_or_default(),
                    })),
                    error: None,
                }))
            }
        }
    }
}
