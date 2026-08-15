//! 通用 RPC：通用 Method 子集 + custom 兜底 + RpcExtension + KernelBuilder。

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::agent::{AgentLoop, SystemPromptProvider, TurnInput};
use crate::audit::{Auditor, MemoryAuditSink};
use crate::contract::{CallerPolicy, PluginError};
use crate::defaults::{InMemorySessionStore, MockModelService};
use crate::dispatch::Dispatch;
use crate::events::{Event, EventSink, MemoryEventSink};
use crate::message::{Message, MessageId};
use crate::registry::{KernelDescriptor, PluginDescriptor, Registry};
use crate::services::{
    AbortSignal, ModelHandle, ModelService, ServiceHandles, SessionKey, SessionStatus, SessionStore,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Method {
    SendUserMessage {
        text: String,
        #[serde(default)]
        force_tool: Option<ForcedToolRequest>,
        #[serde(default)]
        attachments: Vec<AttachmentInfo>,
    },
    TriggerCommand {
        entry: String,
        params: Value,
    },
    EditMessage {
        message_id: MessageId,
        text: String,
    },
    SwitchBranch {
        message_id: MessageId,
    },
    Abort,
    GetState,
    ListSessions,
    ReadSession {
        key: SessionKey,
    },
    ListTools,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomMethod {
    pub method: String,
    #[serde(default)]
    pub params: Value,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireMethod {
    Generic(Method),
    Custom(CustomMethod),
}

impl From<Method> for WireMethod {
    fn from(method: Method) -> Self {
        Self::Generic(method)
    }
}

impl WireMethod {
    pub fn custom(method: impl Into<String>, params: Value) -> Self {
        Self::Custom(CustomMethod {
            method: method.into(),
            params,
            extra: BTreeMap::new(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: u64,
    #[serde(flatten)]
    pub method: WireMethod,
}

impl RpcRequest {
    pub fn custom(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self {
            id,
            method: WireMethod::custom(method, params),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl RpcError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcFrame {
    Response {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<RpcError>,
    },
    Event {
        event: Event,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForcedToolRequest {
    pub entry: String,
    #[serde(default)]
    pub hint: Option<String>,
    #[serde(default)]
    pub display: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentInfo {
    pub path: String,
    pub name: String,
}

#[async_trait]
pub trait RpcExtension: Send + Sync {
    async fn handle(&self, method: &str, params: Value) -> Result<Option<Value>, RpcError>;
}

fn custom_params(c: &CustomMethod) -> Value {
    if c.extra.is_empty() {
        return c.params.clone();
    }
    let mut map = match &c.params {
        Value::Object(obj) => obj.clone(),
        Value::Null => serde_json::Map::new(),
        other => {
            let mut obj = serde_json::Map::new();
            obj.insert("params".into(), other.clone());
            obj
        }
    };
    for (k, v) in &c.extra {
        map.insert(k.clone(), v.clone());
    }
    Value::Object(map)
}

struct TurnHandle {
    key: SessionKey,
    signal: AbortSignal,
}

#[derive(Default)]
struct KernelState {
    turn: Option<TurnHandle>,
}

pub struct Kernel {
    registry: Arc<Registry>,
    dispatch: Arc<Dispatch>,
    loop_engine: Arc<AgentLoop>,
    store: Arc<dyn SessionStore>,
    events: Arc<dyn EventSink>,
    state: Arc<Mutex<KernelState>>,
    extensions: Vec<Arc<dyn RpcExtension>>,
}

pub struct KernelBuilder {
    events: Arc<dyn EventSink>,
    system_prompt: SystemPromptProvider,
    handles: ServiceHandles,
    store: Option<Arc<dyn SessionStore>>,
    main_model: Option<Arc<dyn ModelService>>,
    auditor: Option<Auditor>,
    kernel_plugins: Vec<KernelDescriptor>,
    user_plugins: Vec<PluginDescriptor>,
    extensions: Vec<Arc<dyn RpcExtension>>,
}

impl Default for KernelBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelBuilder {
    pub fn new() -> Self {
        Self {
            events: Arc::new(MemoryEventSink::default()),
            system_prompt: Arc::new(|| "你是通用 Agent。".to_string()),
            handles: ServiceHandles::default(),
            store: None,
            main_model: None,
            auditor: None,
            kernel_plugins: Vec::new(),
            user_plugins: Vec::new(),
            extensions: Vec::new(),
        }
    }

    pub fn event_sink(mut self, events: Arc<dyn EventSink>) -> Self {
        self.events = events;
        self
    }

    pub fn system_prompt(mut self, provider: SystemPromptProvider) -> Self {
        self.system_prompt = provider;
        self
    }

    pub fn service_handles(mut self, handles: ServiceHandles) -> Self {
        self.handles = handles;
        self
    }

    pub fn session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.store = Some(store);
        self
    }

    pub fn main_model(mut self, model: Arc<dyn ModelService>) -> Self {
        self.main_model = Some(model);
        self
    }

    pub fn auditor(mut self, auditor: Auditor) -> Self {
        self.auditor = Some(auditor);
        self
    }

    pub fn register_kernel_plugin(mut self, desc: KernelDescriptor) -> Self {
        self.kernel_plugins.push(desc);
        self
    }

    pub fn register_plugin(mut self, desc: PluginDescriptor) -> Self {
        self.user_plugins.push(desc);
        self
    }

    pub fn extension(mut self, ext: Arc<dyn RpcExtension>) -> Self {
        self.extensions.push(ext);
        self
    }

    pub async fn build(self) -> Result<Arc<Kernel>, String> {
        let store = self
            .store
            .unwrap_or_else(|| Arc::new(InMemorySessionStore::new()));
        let main_model = self
            .main_model
            .unwrap_or_else(|| Arc::new(MockModelService::default()) as Arc<dyn ModelService>);
        let auditor = self
            .auditor
            .unwrap_or_else(|| Auditor::new(Arc::new(MemoryAuditSink::default())));
        let handles = if self.handles.model().is_some() {
            self.handles
        } else {
            self.handles.with_model(ModelHandle::new(
                main_model.clone(),
                std::time::Duration::from_secs(180),
            ))
        };

        let registry = Arc::new(Registry::new(handles));
        for desc in self.kernel_plugins {
            registry
                .register_kernel_plugin(desc)
                .map_err(|e: PluginError| format!("内核插件注册失败：{e}"))?;
        }
        for desc in self.user_plugins {
            registry
                .register_plugin(desc)
                .map_err(|e: PluginError| format!("插件注册失败：{e}"))?;
        }
        let dispatch = Arc::new(Dispatch::new(
            registry.clone(),
            auditor.clone(),
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(10 * 60),
            self.events.clone(),
        ));
        let loop_engine = Arc::new(AgentLoop::new(
            main_model,
            dispatch.clone(),
            auditor,
            self.events.clone(),
            self.system_prompt,
        ));
        Ok(Arc::new(Kernel {
            registry,
            dispatch,
            loop_engine,
            store,
            events: self.events,
            state: Arc::new(Mutex::new(KernelState::default())),
            extensions: self.extensions,
        }))
    }
}

impl Kernel {
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    pub fn dispatch(&self) -> Arc<Dispatch> {
        self.dispatch.clone()
    }

    pub async fn is_idle(&self) -> bool {
        self.state.lock().await.turn.is_none()
    }

    async fn active_session_key(&self) -> Result<SessionKey, RpcError> {
        let metas = self
            .store
            .list_sessions()
            .await
            .map_err(|e| RpcError::new("storage_error", e.to_string()))?;
        metas
            .iter()
            .find(|m| m.status == SessionStatus::Active)
            .map(|m| m.key)
            .ok_or_else(|| RpcError::new("no_active_session", "没有活动会话"))
    }

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
                attachments,
            } => {
                let mut user_text = text.clone();
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
                }
                for a in &attachments {
                    user_text.push_str(&format!("\n附件：{}|{}", a.path, a.name));
                }

                let now = chrono::Utc::now();
                let metas = self
                    .store
                    .list_sessions()
                    .await
                    .map_err(|e| RpcError::new("storage_error", e.to_string()))?;
                let active = metas
                    .iter()
                    .find(|m| m.status == SessionStatus::Active)
                    .map(|m| m.key);
                let key = match active {
                    Some(key) => key,
                    None => {
                        let key = SessionKey::new();
                        let mut meta = crate::services::SessionMeta::new(key);
                        meta.goal = Some(crate::services::Goal {
                            text: user_text.chars().take(40).collect(),
                        });
                        meta.last_activity_at = now;
                        self.store
                            .create_session(&key, &meta)
                            .await
                            .map_err(|e| RpcError::new("storage_error", e.to_string()))?;
                        key
                    }
                };
                let mut user = Message::user(user_text);
                user.created_at = now;
                self.store
                    .append_message(&key, &user)
                    .await
                    .map_err(|e| RpcError::new("storage_error", e.to_string()))?;
                self.store
                    .set_active_path(&key, Some(user.id))
                    .await
                    .map_err(|e| RpcError::new("storage_error", e.to_string()))?;
                let path = self
                    .store
                    .read_path(&key)
                    .await
                    .map_err(|e| RpcError::new("storage_error", e.to_string()))?;
                let forced_wire = None;
                self.start_turn(key, path, forced_wire).await?;
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
            Method::ListTools => Ok(Some(RpcFrame::Response {
                id,
                result: Some(json!({ "tools": self.registry.user_entries() })),
                error: None,
            })),
            Method::EditMessage { message_id, text } => {
                let key = self.active_session_key().await?;
                let path = self
                    .store
                    .derive_branch(&key, message_id, &text)
                    .await
                    .map_err(|e| RpcError::new("branch_error", e.to_string()))?;
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

    async fn start_turn(
        &self,
        key: SessionKey,
        messages: Vec<Message>,
        forced_tool: Option<String>,
    ) -> Result<(), RpcError> {
        let signal = AbortSignal::new();
        let tools = self.registry.model_tools();
        let loop_engine = self.loop_engine.clone();
        let store = self.store.clone();
        let events = self.events.clone();
        let state_for_task = self.state.clone();
        let mut state = self.state.lock().await;
        if state.turn.is_some() {
            return Err(RpcError::new(
                "turn_in_progress",
                "当前有回合在跑，请先停止再发送新消息",
            ));
        }
        state.turn = Some(TurnHandle {
            key,
            signal: signal.clone(),
        });
        drop(state);

        tokio::spawn(async move {
            let input = TurnInput {
                messages,
                tools,
                signal,
                turn_budget: std::time::Duration::from_secs(10 * 60),
                forced_tool,
            };
            match loop_engine.run_turn(input).await {
                Ok(outcome) => {
                    let mut last_id: Option<MessageId> = None;
                    for msg in &outcome.messages {
                        if store.append_message(&key, msg).await.is_ok() {
                            last_id = Some(msg.id);
                        }
                    }
                    if let Some(end) = last_id
                        && store.set_active_path(&key, Some(end)).await.is_err()
                    {
                        events.emit(Event::Error {
                            message: "活跃路径推进失败".into(),
                        });
                    }
                    events.emit(Event::TurnEnd {
                        stop_reason: outcome.stop_reason.clone(),
                    });
                }
                Err(e) => {
                    events.emit(Event::TurnEnd {
                        stop_reason: crate::agent::StopReason::Failed,
                    });
                    events.emit(Event::Error {
                        message: format!("回合失败：{e}"),
                    });
                }
            }
            let mut st = state_for_task.lock().await;
            if st.turn.as_ref().is_some_and(|t| t.key == key) {
                st.turn = None;
            }
        });
        Ok(())
    }
}
