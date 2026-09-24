//! Session scheduler（M1.5）：SessionKey、生命周期、交接摘要、空闲超时。

// ---------- 会话类型（Key/Goal/Status/Meta） ----------

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::kernel::message::{Message, MessageId};
use crate::kernel::plugin::services::{
    AbortSignal, ModelError, ModelRequest, ModelResponse, ModelService, SessionStore, StorageError,
};
use crate::kernel::prompt::summarize_prompt;

// ---------- SessionKey ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionKey(pub Uuid);

impl SessionKey {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionKey {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// ---------- 会话元数据 ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub key: SessionKey,
    pub goal: Option<Goal>,
    /// 用户可见的会话标题：首回合结束后由模型按首条消息生成，用户可改名（RPC `rename_session`）。
    /// 与 `goal`（学习目标，摘要器输入）语义分离；旧 JSONL 无此字段，解析不受影响。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub last_activity_at: DateTime<Utc>,
    pub active_path: Option<MessageId>,
}

impl SessionMeta {
    pub fn new(key: SessionKey) -> Self {
        let now = Utc::now();
        Self {
            key,
            goal: None,
            title: None,
            status: SessionStatus::Active,
            created_at: now,
            archived_at: None,
            last_activity_at: now,
            active_path: None,
        }
    }
}

mod clock;
mod interrupt;
mod scheduler;
mod summarize;
mod title;

pub use clock::{Clock, FakeClock, SystemClock};
pub use interrupt::{Interrupt, InterruptBus};
pub use scheduler::{CreatedSession, SchedulerError, SessionScheduler, TurnContext};
pub use summarize::{HandoffSummary, LlmSummarizer, StubSummarizer, Summarizer};
pub use title::{LlmTitler, StubTitler, Titler};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::events::{Event, MemoryEventSink};
    use crate::kernel::message::MessageKind;
    use crate::kernel::plugin::services::{
        ModelChunk, ModelError, ModelResponse, ModelStream, TokenUsage,
    };
    use crate::kernel::plugin::storage::MemoryStorage;
    use std::collections::VecDeque;

    /// 脚本模型：按队列顺序返回文本或错误（测试守卫/摘要器用）。
    struct ScriptedModel {
        queue: std::sync::Mutex<VecDeque<Result<String, String>>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl ScriptedModel {
        fn new(responses: Vec<Result<String, String>>) -> Self {
            Self {
                queue: std::sync::Mutex::new(responses.into()),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ModelService for ScriptedModel {
        async fn stream(
            &self,
            _request: &ModelRequest,
            _signal: &AbortSignal,
        ) -> Result<ModelStream, ModelError> {
            Ok(Box::new(futures_util::stream::empty::<
                Result<ModelChunk, ModelError>,
            >()))
        }

        async fn complete(
            &self,
            _request: &ModelRequest,
            _signal: &AbortSignal,
        ) -> Result<ModelResponse, ModelError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let next = self
                .queue
                .lock()
                .expect("scripted model poisoned")
                .pop_front()
                .unwrap_or_else(|| Ok(String::new()));
            match next {
                Ok(text) => Ok(ModelResponse {
                    text,
                    tool_calls: Vec::new(),
                    usage: Some(TokenUsage {
                        input_tokens: Some(1),
                        output_tokens: Some(1),
                        ..Default::default()
                    }),
                }),
                Err(e) => Err(ModelError::Transport(e)),
            }
        }
    }

    fn setup() -> (
        SessionScheduler,
        FakeClock,
        MemoryStorage,
        InterruptBus,
        Arc<MemoryEventSink>,
    ) {
        setup_with_titler(Arc::new(StubTitler))
    }

    /// 自定义标题器的装配（标题用例注入 `LlmTitler`）。
    fn setup_with_titler(
        titler: Arc<dyn Titler>,
    ) -> (
        SessionScheduler,
        FakeClock,
        MemoryStorage,
        InterruptBus,
        Arc<MemoryEventSink>,
    ) {
        let store = MemoryStorage::new();
        let clock = FakeClock::new(Utc::now());
        let bus = InterruptBus::new();
        let events = Arc::new(MemoryEventSink::default());
        let scheduler = SessionScheduler::new(
            Arc::new(store.clone()),
            Arc::new(clock.clone()),
            Arc::new(StubSummarizer),
            titler,
            bus.clone(),
            events.clone(),
        );
        (scheduler, clock, store, bus, events)
    }

    #[tokio::test]
    async fn display_text_persisted_on_forced_tool_message() {
        // force_tool 场景：text（模型指令）与 display_text（前端展示）分离落盘。
        let (scheduler, _, store, _, _) = setup();
        let ctx = scheduler
            .on_new_message_with_display(
                "请调用工具 memory::show 处理：数学/向量组的线性相关性",
                Some("翻看记忆：数学/向量组的线性相关性"),
            )
            .await
            .unwrap();
        let msgs = store.read_path(&ctx.session_key).await.unwrap();
        let user = msgs
            .iter()
            .find_map(|m| match &m.kind {
                MessageKind::User {
                    text, display_text, ..
                } => Some((text.clone(), display_text.clone())),
                _ => None,
            })
            .expect("应有 user 消息");
        assert_eq!(
            user.0,
            "请调用工具 memory::show 处理：数学/向量组的线性相关性"
        );
        assert_eq!(user.1.as_deref(), Some("翻看记忆：数学/向量组的线性相关性"));
    }

    #[tokio::test]
    async fn first_message_creates_session() {
        let (scheduler, _, store, _, _) = setup();
        let ctx = scheduler.on_new_message("帮我看看这道题").await.unwrap();
        assert_eq!(ctx.messages.len(), 1);
        let metas = store.list_sessions().await.unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].status, SessionStatus::Active);
    }

    #[tokio::test]
    async fn idle_timeout_emits_event_and_continues() {
        let (scheduler, clock, store, _, events) = setup();
        let first = scheduler.on_new_message("帮我看看这道题").await.unwrap();
        events.take();
        clock.advance(Duration::from_secs(13 * 60 * 60));
        let second = scheduler.on_new_message("生成周复习报告").await.unwrap();
        // 空闲超时不再自动分叉：留在原会话，只发提示事件（ADR-0044）。
        assert_eq!(first.session_key, second.session_key);
        let metas = store.list_sessions().await.unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].status, SessionStatus::Active);
        let msgs = store.read_path(&second.session_key).await.unwrap();
        assert!(
            !msgs.iter().any(|m| matches!(
                m.kind,
                MessageKind::System { ref text, .. } if text.contains("上一会话梗概")
            )),
            "不应再插入摘要节点"
        );
        assert!(matches!(
            msgs.last().unwrap().kind,
            MessageKind::User { .. }
        ));
        let emitted = events.take();
        assert!(
            emitted
                .iter()
                .any(|e| matches!(e, Event::SessionIdle { .. })),
            "应发出空闲提示事件：{emitted:?}"
        );
    }

    #[tokio::test]
    async fn new_message_continues_current_session() {
        let (scheduler, _, store, _, _) = setup();
        let first = scheduler.on_new_message("帮我看看这道题").await.unwrap();
        let second = scheduler.on_new_message("继续讲第二题").await.unwrap();
        // 没有任何自动切换：新消息一律继续当前会话。
        assert_eq!(first.session_key, second.session_key);
        let metas = store.list_sessions().await.unwrap();
        assert_eq!(metas.len(), 1, "不应自动切换新会话");
        assert_eq!(metas[0].status, SessionStatus::Active, "唯一会话保持活动");
    }

    #[tokio::test]
    async fn create_new_session_archives_previous_and_returns_new_key() {
        let (scheduler, _, store, _, _) = setup();
        let old = scheduler.on_new_message("帮我看看这道题").await.unwrap();

        let created = scheduler.create_new_session(None, false).await.unwrap();

        assert_ne!(created.key, old.session_key, "新会话应为独立 SessionKey");
        assert_eq!(created.archived, Some(old.session_key));
        assert!(!created.summary_attached);
        let metas = store.list_sessions().await.unwrap();
        assert_eq!(metas.len(), 2);
        let archived = metas.iter().find(|m| m.key == old.session_key).unwrap();
        assert_eq!(archived.status, SessionStatus::Archived);
        assert!(archived.archived_at.is_some());
        // 单 Active 不变量：查找活动会话的地方都取第一个匹配。
        assert_eq!(
            metas
                .iter()
                .filter(|m| m.status == SessionStatus::Active)
                .count(),
            1
        );
        assert_eq!(
            metas.iter().find(|m| m.key == created.key).unwrap().status,
            SessionStatus::Active
        );
    }

    #[tokio::test]
    async fn create_new_session_without_active_session() {
        let (scheduler, _, store, _, _) = setup();
        let created = scheduler
            .create_new_session(
                Some(Goal {
                    text: "线性代数".into(),
                }),
                false,
            )
            .await
            .unwrap();
        assert_eq!(created.archived, None);
        assert!(!created.summary_attached);
        let metas = store.list_sessions().await.unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].goal.as_ref().unwrap().text, "线性代数");
    }

    #[tokio::test]
    async fn create_new_session_carry_summary_attaches_summary_first() {
        let (scheduler, _, store, _, _) = setup();
        let old = scheduler.on_new_message("帮我看看这道题").await.unwrap();
        scheduler.on_new_message("继续讲第二题").await.unwrap();

        let created = scheduler.create_new_session(None, true).await.unwrap();

        assert!(created.summary_attached);
        let msgs = store.read_path(&created.key).await.unwrap();
        assert_eq!(msgs.len(), 1, "新会话首条消息即为交接摘要");
        assert!(matches!(
            msgs[0].kind,
            MessageKind::System { ref text, .. } if text.contains("上一会话梗概")
        ));
        // 旧会话本身不因携带摘要而被写入。
        let old_msgs = store.read_path(&old.session_key).await.unwrap();
        assert!(!old_msgs.iter().any(|m| matches!(
            m.kind,
            MessageKind::System { ref text, .. } if text.contains("上一会话梗概")
        )));
    }

    #[tokio::test]
    async fn create_new_session_carry_summary_skips_empty_session() {
        let (scheduler, _, store, _, _) = setup();
        // 先建一个空会话（无任何消息），再携带摘要新建。
        let old = scheduler.create_new_session(None, false).await.unwrap();

        let created = scheduler.create_new_session(None, true).await.unwrap();

        assert!(!created.summary_attached, "空会话没有可交接的内容");
        assert_eq!(created.archived, Some(old.key));
        assert!(store.read_path(&created.key).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn on_new_message_appends_under_handoff_summary() {
        let (scheduler, _, store, _, events) = setup();
        scheduler.on_new_message("帮我看看这道题").await.unwrap();
        let created = scheduler.create_new_session(None, true).await.unwrap();
        events.take();

        let ctx = scheduler.on_new_message("换一道新题").await.unwrap();

        assert_eq!(ctx.session_key, created.key, "新消息应落在新会话");
        let msgs = store.read_path(&created.key).await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(
            matches!(
                msgs[0].kind,
                MessageKind::System { ref text, .. } if text.contains("上一会话梗概")
            ),
            "首条应为交接摘要"
        );
        assert_eq!(
            msgs[1].parent_id,
            Some(msgs[0].id),
            "用户消息应挂在摘要节点下"
        );
        assert!(events.take().is_empty(), "刚活动过，不应触发空闲提示");
    }

    /// 造一条助手回复：标题只在「已有用户消息 + 助手回复」时才生成。
    async fn append_assistant(store: &MemoryStorage, key: &SessionKey, text: &str) -> Message {
        let path = store.read_path(key).await.unwrap();
        let mut reply = Message::assistant(text);
        reply.parent_id = path.last().map(|m| m.id);
        store.append_message(key, &reply).await.unwrap();
        reply
    }

    #[tokio::test]
    async fn title_generated_once_after_first_turn() {
        let model = Arc::new(ScriptedModel::new(vec![Ok("线性代数错题整理".into())]));
        let titler: Arc<dyn Titler> =
            Arc::new(LlmTitler::new(model.clone()).with_retry(0, Duration::ZERO));
        let (scheduler, _, store, _, _) = setup_with_titler(titler);
        let ctx = scheduler
            .on_new_message("帮我整理线性代数的错题")
            .await
            .unwrap();

        // 只有用户消息：不生成（首答还没来，没有可概括的对话）。
        assert_eq!(
            scheduler
                .maybe_generate_title(&ctx.session_key)
                .await
                .unwrap(),
            None
        );
        assert_eq!(model.call_count(), 0);

        append_assistant(&store, &ctx.session_key, "好的，先看第一题").await;
        assert_eq!(
            scheduler
                .maybe_generate_title(&ctx.session_key)
                .await
                .unwrap()
                .as_deref(),
            Some("线性代数错题整理")
        );
        assert_eq!(
            store
                .get_session(&ctx.session_key)
                .await
                .unwrap()
                .unwrap()
                .title
                .as_deref(),
            Some("线性代数错题整理")
        );
        assert_eq!(model.call_count(), 1);

        // 已有标题：不再调模型（用户改过的名字不会被下一回合覆盖）。
        assert_eq!(
            scheduler
                .maybe_generate_title(&ctx.session_key)
                .await
                .unwrap(),
            None
        );
        assert_eq!(model.call_count(), 1);
    }

    #[tokio::test]
    async fn title_falls_back_to_truncated_first_message_on_model_error() {
        let model = Arc::new(ScriptedModel::new(vec![
            Err("HTTP 503 Service Unavailable".into()),
            Err("HTTP 503 Service Unavailable".into()),
        ]));
        let titler: Arc<dyn Titler> =
            Arc::new(LlmTitler::new(model.clone()).with_retry(1, Duration::ZERO));
        let (scheduler, _, store, _, _) = setup_with_titler(titler);
        let ctx = scheduler
            .on_new_message("帮我整理线性代数的错题")
            .await
            .unwrap();
        append_assistant(&store, &ctx.session_key, "好的").await;

        // 模型失败：降级为截断的首条用户消息，绝不因辅助调用失败影响主链路。
        assert_eq!(
            scheduler
                .maybe_generate_title(&ctx.session_key)
                .await
                .unwrap()
                .as_deref(),
            Some("帮我整理线性代数的错题")
        );
        assert_eq!(model.call_count(), 2, "1 次 + 1 次重试");
    }

    #[tokio::test]
    async fn title_prefers_display_text_over_model_instruction() {
        let (scheduler, _, store, _, _) = setup();
        // forced_tool：text 是给模型的指令，display_text 才是学生看到的话。转录与兜底
        // 标题都必须取后者，否则侧栏会出现一串「请调用工具 X 处理当前请求。」。
        let ctx = scheduler
            .on_new_message_with_display(
                "请调用工具 memory::show 处理：数学/向量组的线性相关性",
                Some("翻看记忆：数学/向量组的线性相关性"),
            )
            .await
            .unwrap();
        append_assistant(&store, &ctx.session_key, "找到了").await;

        assert_eq!(
            scheduler
                .maybe_generate_title(&ctx.session_key)
                .await
                .unwrap()
                .as_deref(),
            Some("翻看记忆：数学/向量组的线性相关性")
        );
    }

    #[tokio::test]
    async fn stub_titler_falls_back_without_calling_model() {
        let (scheduler, _, store, _, _) = setup();
        let ctx = scheduler.on_new_message("   ").await.unwrap();
        append_assistant(&store, &ctx.session_key, "嗯").await;
        // 无可用文本时用固定文案兜底。
        assert_eq!(
            scheduler
                .maybe_generate_title(&ctx.session_key)
                .await
                .unwrap()
                .as_deref(),
            Some("新会话")
        );
    }

    #[tokio::test]
    async fn open_existing_archives_previous_active_only() {
        let (scheduler, _, store, _, _) = setup();
        let first = scheduler.on_new_message("第一次").await.unwrap();
        let created = scheduler.create_new_session(None, false).await.unwrap();
        let before = store
            .get_session(&first.session_key)
            .await
            .unwrap()
            .unwrap()
            .last_activity_at;

        scheduler.open_existing(first.session_key).await.unwrap();

        let metas = store.list_sessions().await.unwrap();
        assert_eq!(
            metas
                .iter()
                .filter(|m| m.status == SessionStatus::Active)
                .count(),
            1,
            "切换后仍只有一条 Active"
        );
        assert_eq!(
            metas
                .iter()
                .find(|m| m.key == first.session_key)
                .unwrap()
                .status,
            SessionStatus::Active
        );
        assert_eq!(
            metas.iter().find(|m| m.key == created.key).unwrap().status,
            SessionStatus::Archived
        );
        // 切换不算发言：活动时间不变（空闲超时判定依赖它）。
        assert_eq!(
            store
                .get_session(&first.session_key)
                .await
                .unwrap()
                .unwrap()
                .last_activity_at,
            before
        );

        // 不存在的会话：报错而不是静默成功。
        assert!(scheduler.open_existing(SessionKey::new()).await.is_err());
    }

    #[tokio::test]
    async fn llm_summarizer_falls_back_on_model_error() {
        // 8+ 条消息才走 LLM；连续失败 3 次（含重试）后降级 stub。
        let model = Arc::new(ScriptedModel::new(vec![
            Err("HTTP 503 Service Unavailable".into()),
            Err("HTTP 503 Service Unavailable".into()),
            Err("HTTP 503 Service Unavailable".into()),
        ]));
        let messages: Vec<Message> = (0..8).map(|i| Message::user(format!("消息 {i}"))).collect();
        let summarizer = LlmSummarizer::new(model).with_retry(2, Duration::ZERO);
        let text = summarizer.summarize(&messages, None).await;
        assert!(text.contains("共 8 条消息"));
    }

    #[tokio::test]
    async fn llm_summarizer_retries_transient_errors() {
        let model = Arc::new(ScriptedModel::new(vec![
            Err("HTTP 503 Service Unavailable".into()),
            Ok("本会话完成三套英语作业批改，错题已归档。".into()),
        ]));
        let messages: Vec<Message> = (0..8).map(|i| Message::user(format!("消息 {i}"))).collect();
        let summarizer = LlmSummarizer::new(model).with_retry(2, Duration::ZERO);
        let text = summarizer.summarize(&messages, None).await;
        assert!(text.contains("三套英语作业批改"));
    }

    #[tokio::test]
    async fn short_session_summary_skips_llm() {
        let model = Arc::new(ScriptedModel::new(vec![]));
        let summarizer = LlmSummarizer::new(model.clone());
        let text = summarizer
            .summarize(&[Message::user("你好"), Message::user("继续")], None)
            .await;
        assert!(text.contains("共 2 条消息"));
        assert_eq!(model.call_count(), 0, "短会话摘要不应调用模型");
    }
}
