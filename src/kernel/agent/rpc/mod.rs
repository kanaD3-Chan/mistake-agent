//! RPC（ADR-0013 / Q15）：GUI ↔ kernel 命令通道与事件流。

pub(crate) mod handlers;
mod protocol;

pub(crate) use handlers::{KernelState, TurnHandle, persist_turn_messages};
pub use protocol::{
    CustomMethod, ForcedToolRequest, Method, RpcError, RpcExtension, RpcFrame, RpcRequest,
    WireMethod,
};

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::kernel::agent::cache::CacheTracker;
use crate::kernel::agent::dispatch::Dispatch;
use crate::kernel::agent::loop_mod::{AgentLoop, SystemPromptProvider, TurnInput, TurnOutcome};
use crate::kernel::agent::session::{
    Interrupt, InterruptBus, LlmSummarizer, LlmTurnDecider, SessionKey, SessionScheduler,
    SessionStatus, SessionSwitch, SystemClock, scope_session_context,
};
use crate::kernel::audit::{AuditRecord, Auditor};
use crate::kernel::contract::{CallerPolicy, full_to_wire};
use crate::kernel::events::{Event, EventSink};
use crate::kernel::logger::{Logger, LoggerHandle};
use crate::kernel::message::{Message, MessageId};
use crate::kernel::plugin::compute::BridgeCompute;
use crate::kernel::plugin::memory::{FileMemoryService, InMemoryMemory};
use crate::kernel::plugin::model::{LiveSettingsModelService, RoutingModelService};
use crate::kernel::plugin::services::{
    AbortSignal, ComputeHandle, MemoryHandle, MemoryService, ModelHandle, ModelKind, ModelRequest,
    ModelService, ServiceHandles, SessionStore, StorageHandle,
};
use crate::kernel::plugin::storage::{AnyStorage, FileStorage};
use crate::kernel::registry::{KernelDescriptor, PluginDescriptor, Registry};
use crate::kernel::settings::Settings;

pub struct Kernel {
    registry: Arc<Registry>,
    dispatch: Arc<Dispatch>,
    loop_engine: Arc<AgentLoop>,
    scheduler: Arc<SessionScheduler>,
    store: Arc<dyn SessionStore>,
    auditor: Auditor,
    events: Arc<dyn EventSink>,
    state: Arc<Mutex<KernelState>>,
    cache: Arc<CacheTracker>,
    extensions: Vec<Arc<dyn RpcExtension>>,
}

/// 通用 Kernel 装配入口：事件、句柄、插件、系统提示与 RPC 扩展都经 builder 注入，
/// 应用专属服务（FileStorage/LiveSettingsModelService/AppRpc…）由使用方在 `Kernel::new`
/// 或自己的装配函数中构造后传入。
pub struct KernelBuilder {
    events: Arc<dyn EventSink>,
    system_prompt: SystemPromptProvider,
    settings: Option<Arc<std::sync::RwLock<Settings>>>,
    handles: ServiceHandles,
    store: Option<Arc<dyn SessionStore>>,
    main_model: Option<Arc<dyn ModelService>>,
    auditor: Option<Auditor>,
    cache: Option<Arc<CacheTracker>>,
    interrupt_bus: InterruptBus,
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
            events: Arc::new(crate::kernel::events::MemoryEventSink::default()),
            system_prompt: Arc::new(String::new),
            settings: None,
            handles: ServiceHandles::default(),
            store: None,
            main_model: None,
            auditor: None,
            cache: None,
            interrupt_bus: InterruptBus::new(),
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

    pub fn settings(mut self, settings: Arc<std::sync::RwLock<Settings>>) -> Self {
        self.settings = Some(settings);
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

    pub fn cache(mut self, cache: Arc<CacheTracker>) -> Self {
        self.cache = Some(cache);
        self
    }

    pub fn interrupt_bus(mut self, bus: InterruptBus) -> Self {
        self.interrupt_bus = bus;
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

    pub fn register_kernel_plugins(mut self, descs: Vec<KernelDescriptor>) -> Self {
        self.kernel_plugins.extend(descs);
        self
    }

    pub fn register_plugins(mut self, descs: Vec<PluginDescriptor>) -> Self {
        self.user_plugins.extend(descs);
        self
    }

    pub fn extension(mut self, ext: Arc<dyn RpcExtension>) -> Self {
        self.extensions.push(ext);
        self
    }

    pub async fn build(self) -> Result<Arc<Kernel>, String> {
        let store = self
            .store
            .ok_or_else(|| "KernelBuilder 缺少 session_store".to_string())?;
        let main_model = self
            .main_model
            .ok_or_else(|| "KernelBuilder 缺少 main_model".to_string())?;
        let auditor = self
            .auditor
            .ok_or_else(|| "KernelBuilder 缺少 auditor".to_string())?;
        let cache = self.cache.unwrap_or_default();

        let logger: LoggerHandle = Arc::new(Logger);
        let registry = Arc::new(Registry::new(self.handles, logger));
        for desc in self.kernel_plugins {
            registry
                .register_kernel_plugin(desc)
                .map_err(|e| format!("内核插件注册失败：{e}"))?;
        }
        for desc in self.user_plugins {
            registry
                .register_plugin(desc)
                .map_err(|e| format!("插件注册失败：{e}"))?;
        }

        let english_mode: crate::kernel::agent::dispatch::EnglishModeProvider =
            match self.settings.as_ref() {
                Some(settings) => {
                    let settings = settings.clone();
                    Arc::new(move || settings.read().map(|s| s.english_mode).unwrap_or(false))
                }
                None => Arc::new(|| false),
            };
        let dispatch = Dispatch::new(
            registry.clone(),
            auditor.clone(),
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(10 * 60),
            self.events.clone(),
        )
        .with_english_mode(english_mode);
        let dispatch = Arc::new(dispatch);
        let llm_settings = self.settings.clone();
        let mut decider = LlmTurnDecider::new(main_model.clone());
        let mut scheduler_summarizer = LlmSummarizer::new(main_model.clone());
        let mut loop_summarizer = LlmSummarizer::new(main_model.clone());
        if let Some(settings) = llm_settings.clone() {
            decider = decider.with_settings(settings.clone());
            scheduler_summarizer = scheduler_summarizer.with_settings(settings.clone());
            loop_summarizer = loop_summarizer.with_settings(settings);
        }
        // 中断总线必须由 scheduler 与 loop 共享：scheduler 发环境变更，loop 回合边界消费。
        let scheduler = Arc::new(SessionScheduler::new(
            store.clone(),
            Arc::new(decider),
            Arc::new(SystemClock),
            Arc::new(scheduler_summarizer),
            self.interrupt_bus.clone(),
        ));
        let loop_model = main_model.clone();
        let loop_engine = Arc::new(AgentLoop::new(
            loop_model,
            dispatch.clone(),
            auditor.clone(),
            self.events.clone(),
            Arc::new(loop_summarizer),
            self.interrupt_bus,
            self.system_prompt,
            Some(scheduler.clone() as Arc<dyn SessionSwitch>),
        ));

        Ok(Arc::new(Kernel {
            registry,
            dispatch,
            loop_engine,
            scheduler,
            store,
            auditor,
            events: self.events,
            state: Arc::new(Mutex::new(KernelState { turn: None })),
            cache,
            extensions: self.extensions,
        }))
    }
}

/// mistake-agent 应用专属 RPC 扩展：settings/balance/cache/compute 方法走 custom 兜底，
/// 不占通用 `Method` 子集（M1 解耦，`so-lite-agent` 已迁出至独立仓库；本项留在 mistake-agent app 侧）。
struct AppRpc {
    settings: Arc<std::sync::RwLock<Settings>>,
    store: Arc<dyn SessionStore>,
    main_service: Arc<LiveSettingsModelService>,
    vision_service: Arc<LiveSettingsModelService>,
    compute: Arc<BridgeCompute>,
    cache: Arc<CacheTracker>,
    interrupt_bus: InterruptBus,
    auditor: Auditor,
}

#[async_trait]
impl RpcExtension for AppRpc {
    async fn handle(&self, method: &str, params: Value) -> Result<Option<Value>, RpcError> {
        match method {
            "get_settings" => Ok(Some(
                self.settings
                    .read()
                    .expect("settings poisoned")
                    .public_view(),
            )),
            "set_settings" => {
                let patch: crate::kernel::settings::SettingsPatch =
                    serde_json::from_value(params.get("patch").cloned().unwrap_or(Value::Null))
                        .map_err(|e| RpcError::new("invalid_settings", e.to_string()))?;
                let view = {
                    let mut settings = self.settings.write().expect("settings poisoned");
                    settings
                        .apply_patch(&patch)
                        .map_err(|e| RpcError::new("invalid_settings", e))?;
                    settings
                        .save()
                        .map_err(|e| RpcError::new("save_failed", e))?;
                    if let Some(level) = patch.log_level {
                        Logger::set_level(level);
                    }
                    settings.public_view()
                };
                log::info!(
                    "设置已保存并热更新：main_key_set={} vision_key_set={}",
                    view["main_model"]["key_set"],
                    view["vision_model"]["key_set"]
                );
                self.main_service.refresh();
                self.vision_service.refresh();
                self.interrupt_bus.send(Interrupt::ConfigChanged);
                self.auditor.record(AuditRecord::SettingsChanged);
                crate::kernel::bootstrap::init_data_root(
                    &crate::kernel::settings::Settings::data_root(),
                )
                .map_err(|e| RpcError::new("bootstrap_failed", e))?;
                Ok(Some(view))
            }
            "compute_result" => {
                let id = params
                    .get("compute_id")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| RpcError::new("invalid_params", "缺少 compute_id"))?;
                let result = crate::kernel::plugin::services::ComputeResult {
                    stdout: params
                        .get("stdout")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    stderr: params
                        .get("stderr")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    duration_ms: params
                        .get("duration_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                };
                let delivered = self.compute.deliver(id, result);
                Ok(Some(json!({ "delivered": delivered })))
            }
            "test_connection" => {
                let started = std::time::Instant::now();
                let is_vision = params.get("model").and_then(Value::as_str) == Some("vision");
                let model_req = ModelRequest {
                    model: ModelKind::Main,
                    messages: vec![Message::user("回复：ok")],
                    tools: None,
                    reasoning_effort: Some("none".into()),
                    tool_choice: None,
                    response_format: None,
                };
                let api_key = params.get("api_key").and_then(Value::as_str);
                let result = if let Some(key) = api_key
                    && !key.trim().is_empty()
                {
                    let snapshot = self.settings.read().expect("settings poisoned").clone();
                    let mut model_cfg = if is_vision {
                        snapshot.vision_model.clone()
                    } else {
                        snapshot.main_model.clone()
                    };
                    model_cfg.api_key = key.trim().to_string();
                    let temp_settings = if is_vision {
                        crate::kernel::settings::Settings {
                            log_level: snapshot.log_level,
                            english_mode: snapshot.english_mode,
                            main_model: snapshot.main_model.clone(),
                            vision_model: model_cfg,
                        }
                    } else {
                        crate::kernel::settings::Settings {
                            log_level: snapshot.log_level,
                            english_mode: snapshot.english_mode,
                            main_model: model_cfg,
                            vision_model: snapshot.vision_model.clone(),
                        }
                    };
                    if is_vision {
                        crate::kernel::plugin::model::build_vision_service(&temp_settings)
                            .complete(&model_req, &AbortSignal::new())
                            .await
                    } else {
                        crate::kernel::plugin::model::build_main_service(&temp_settings)
                            .complete(&model_req, &AbortSignal::new())
                            .await
                    }
                } else if is_vision {
                    self.vision_service
                        .complete(&model_req, &AbortSignal::new())
                        .await
                } else {
                    self.main_service
                        .complete(&model_req, &AbortSignal::new())
                        .await
                };
                match result {
                    Ok(_) => Ok(Some(json!({
                        "ok": true,
                        "latency_ms": started.elapsed().as_millis() as u64,
                    }))),
                    Err(e) => Err(RpcError::new("connection_failed", e.to_string())),
                }
            }
            "check_balance" => {
                let settings = self.settings.read().expect("settings poisoned").clone();
                let report = crate::kernel::agent::balance::check_balance(&settings).await;
                self.auditor.record(AuditRecord::BalanceChecked {
                    main_ok: report.main.ok,
                    vision_ok: report.vision.ok,
                });
                Ok(Some(
                    serde_json::to_value(&report).unwrap_or_else(|_| serde_json::json!({})),
                ))
            }
            "get_cache_stats" => {
                let metas = self
                    .store
                    .list_sessions()
                    .await
                    .map_err(|e| RpcError::new("storage_error", e.to_string()))?;
                let active = metas
                    .iter()
                    .find(|m| m.status == SessionStatus::Active)
                    .map(|m| m.key);
                Ok(Some(self.cache.snapshot(active)))
            }
            _ => Ok(None),
        }
    }
}

impl Kernel {
    /// mistake-agent 便捷装配：应用专属服务 + 通用 KernelBuilder。
    pub async fn new(events: Arc<dyn EventSink>) -> Result<Arc<Self>, String> {
        let settings = Arc::new(std::sync::RwLock::new(Settings::load()?));
        let data_root = Settings::data_root();
        // 数据根目录一次性初始化（子目录 + AGENTS.md 模板，幂等）。
        crate::kernel::bootstrap::init_data_root(&data_root)?;
        Logger::init(
            settings.read().expect("settings poisoned").log_level,
            &data_root.join("logs"),
        )?;
        let storage = Arc::new(match FileStorage::open(&data_root) {
            Ok(file) => AnyStorage::File(file),
            Err(e) => {
                eprintln!("[kernel] 文件存储打开失败，回退内存存储：{e}");
                AnyStorage::Mem(crate::kernel::plugin::storage::MemoryStorage::new())
            }
        });
        let memory: Arc<dyn MemoryService> = match FileMemoryService::open_default(storage.clone())
        {
            Ok(file_memory) => {
                // 旧存储布局迁移（ADR-0042）：中文路径 → base64url 段编码；失败不阻塞启动。
                if let Err(e) = file_memory.migrate_legacy_layout().await {
                    eprintln!("[kernel] 记忆布局迁移失败（继续启动）：{e}");
                }
                Arc::new(file_memory)
            }
            Err(e) => {
                eprintln!("[kernel] 记忆目录打开失败，回退内存记忆：{e}");
                Arc::new(InMemoryMemory::new())
            }
        };
        let compute = Arc::new(BridgeCompute::new(events.clone()));
        let main_service = Arc::new(LiveSettingsModelService::new(
            settings.clone(),
            ModelKind::Main,
        ));
        let vision_service = Arc::new(LiveSettingsModelService::new(
            settings.clone(),
            ModelKind::Vision,
        ));
        let cache = Arc::new(CacheTracker::default());

        let auditor = Auditor::new(storage.clone());
        let router = Arc::new(RoutingModelService::new(
            main_service.clone() as Arc<dyn crate::kernel::plugin::services::ModelService>,
            vision_service.clone() as Arc<dyn crate::kernel::plugin::services::ModelService>,
        ));
        let handles = ServiceHandles::default()
            .with_storage(
                StorageHandle::new(storage.clone()).with_io(storage.clone(), storage.clone()),
            )
            .with_memory(MemoryHandle::with_observability(
                memory.clone(),
                events.clone(),
                auditor.clone(),
            ))
            .with_compute(ComputeHandle::new(compute.clone()))
            .with_model(ModelHandle::new(
                router,
                std::time::Duration::from_secs(180),
                auditor.clone(),
            ));

        let interrupt_bus = InterruptBus::new();
        let app_rpc = AppRpc {
            settings: settings.clone(),
            store: storage.clone(),
            main_service: main_service.clone(),
            vision_service: vision_service.clone(),
            compute: compute.clone(),
            cache: cache.clone(),
            interrupt_bus: interrupt_bus.clone(),
            auditor: auditor.clone(),
        };

        KernelBuilder::new()
            .event_sink(events)
            .settings(settings.clone())
            .system_prompt({
                let settings_for_prompt = settings.clone();
                Arc::new(move || {
                    crate::kernel::prompt::agent_system_prompt(
                        settings_for_prompt
                            .read()
                            .map(|s| s.english_mode)
                            .unwrap_or(false),
                    )
                })
            })
            .service_handles(handles)
            .session_store(storage.clone())
            .main_model(main_service.clone())
            .auditor(auditor)
            .cache(cache.clone())
            .interrupt_bus(interrupt_bus)
            .extension(Arc::new(app_rpc))
            .register_kernel_plugins(crate::kernel::plugin::builtin_kernel_plugins())
            .register_plugins(crate::plugin::builtin_plugins())
            .build()
            .await
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    pub fn dispatch(&self) -> Arc<Dispatch> {
        self.dispatch.clone()
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

    /// 当前是否有回合在跑（GUI 关闭收尾时轮询用）。
    pub async fn is_idle(&self) -> bool {
        self.state.lock().await.turn.is_none()
    }

    /// 发起一轮 agent 回合（send_user_message 与「编辑用户消息后重发」共用）：
    /// 登记 turn 句柄、spawn loop、落盘、事件与审计收尾。
    async fn start_turn(
        &self,
        key: SessionKey,
        messages: Vec<Message>,
        forced_tool: Option<String>,
    ) -> Result<(), RpcError> {
        let signal = AbortSignal::new();
        let tools = self.registry.model_tools();
        // 会话上下文边界：从最近的「上一会话梗概」起算，旧会话内容不进模型上下文。
        let messages = scope_session_context(&messages);
        // 注入当前会话 ID：分叉会话 = 摘要节点（会话边界）的消息 UUID；根会话 = 链首消息 UUID。
        // 模型据此确认是否真的切换到了新会话（分叉后 ID 变化，会话内保持不变）。
        let session_id = messages
            .first()
            .map(|m| m.id.to_string())
            .unwrap_or_else(|| key.to_string());
        let mut scoped_messages = Vec::with_capacity(messages.len() + 1);
        scoped_messages.push(Message::system(format!("当前会话 ID：{session_id}")));
        scoped_messages.extend(messages);
        let messages = scoped_messages;
        let loop_engine = self.loop_engine.clone();
        let scheduler = self.scheduler.clone();
        let store = self.store.clone();
        let events = self.events.clone();
        let auditor = self.auditor.clone();
        let cache = self.cache.clone();
        let state_for_task = self.state.clone();
        let mut state = self.state.lock().await;
        if state.turn.is_some() {
            // 并发竞态兜底：另一请求已登记回合。
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
            let outcome: Result<TurnOutcome, _> = loop_engine.run_turn(input).await;
            match outcome {
                Ok(outcome) => {
                    let compaction = outcome.compaction.clone();
                    // 回合内经 session::switch 切换后，后半段消息归新会话。
                    let persist_key = outcome.session_key.unwrap_or(key);
                    let skip_summary = compaction.as_ref().map(|c| c.summary.id);
                    let persisted_last = match persist_turn_messages(
                        &store,
                        &persist_key,
                        &outcome.messages,
                        skip_summary,
                    )
                    .await
                    {
                        Ok(last) => last,
                        Err(e) => {
                            events.emit(Event::Error {
                                message: format!("消息落盘失败：{e}"),
                            });
                            None
                        }
                    };
                    if let Some(info) = &compaction {
                        if let Err(e) = store
                            .splice_compaction(&persist_key, &info.summary, info.tail_start)
                            .await
                        {
                            events.emit(Event::Error {
                                message: format!("压缩摘要接入失败：{e}"),
                            });
                        }
                        events.emit(Event::Compaction {
                            session: persist_key,
                        });
                        auditor.record(AuditRecord::Compaction {
                            session: persist_key.to_string(),
                            summarized: info.summarized,
                        });
                        scheduler.interrupt_bus().send(Interrupt::CompactionDone {
                            session: persist_key,
                        });
                    }
                    // 活跃路径推进到回合末条（消息树分支语义）。
                    let next_active = compaction.as_ref().map(|c| c.tail_end).or(persisted_last);
                    if let Some(next) = next_active
                        && let Err(e) = store.set_active_path(&persist_key, Some(next)).await
                    {
                        events.emit(Event::Error {
                            message: format!("活跃路径推进失败：{e}"),
                        });
                    }
                    if let Err(e) = scheduler.on_turn_end(&persist_key, &outcome.messages).await {
                        events.emit(Event::Error {
                            message: format!("回合收尾失败：{e}"),
                        });
                    }
                    // 消息已落盘、活跃路径已推进：此刻通知前端刷新，链式渲染不会丢新消息。
                    events.emit(Event::TurnEnd {
                        stop_reason: outcome.stop_reason.clone(),
                    });
                    if let Some(usage) = &outcome.usage {
                        cache.record_main(&persist_key, usage);
                        // 实时推送：前端收到事件即更新，无需再查一次（可能读到旧值）。
                        events.emit(Event::CacheStatsUpdated {
                            stats: cache.snapshot(Some(persist_key)),
                        });
                    }
                    auditor.record(AuditRecord::Lifecycle {
                        phase: "turn_finished".into(),
                    });
                }
                Err(e) => {
                    events.emit(Event::TurnEnd {
                        stop_reason: crate::kernel::agent::loop_mod::StopReason::Failed,
                    });
                    events.emit(Event::Error {
                        message: format!("回合失败：{e}"),
                    });
                }
            }
            // 回合结束：清除 turn 句柄（abort 在结束后无操作）。
            let mut st = state_for_task.lock().await;
            if st.turn.as_ref().is_some_and(|t| t.key == key) {
                st.turn = None;
            }
        });
        Ok(())
    }
}
#[cfg(test)]
mod tests;
