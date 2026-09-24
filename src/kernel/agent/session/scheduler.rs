//! SessionScheduler：会话生命周期调度（M1.5 核心）。
//!
//! 会话切换只由用户发起（ADR-0044）：新建会话经 [`SessionScheduler::create_new_session`]，
//! 没有任何模型侧的自动判断或自动分叉。

use super::*;

use crate::kernel::events::{Event, EventSink};

// ---------- SessionScheduler ----------

#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error("storage 错误：{0}")]
    Storage(#[from] StorageError),
    #[error("内部错误：{0}")]
    Internal(String),
}

/// 一个回合的上下文：在哪个会话跑、活跃路径是什么。
#[derive(Debug, Clone)]
pub struct TurnContext {
    pub session_key: SessionKey,
    pub messages: Vec<Message>,
}

/// 用户手动新建会话的结果。
#[derive(Debug, Clone)]
pub struct CreatedSession {
    pub key: SessionKey,
    /// 被归档的旧活动会话（原本就没有活动会话时为 None）。
    pub archived: Option<SessionKey>,
    /// 是否已把交接摘要挂为新会话首条系统消息。
    pub summary_attached: bool,
}

pub struct SessionScheduler {
    store: Arc<dyn SessionStore>,
    clock: Arc<dyn Clock>,
    summarizer: Arc<dyn Summarizer>,
    titler: Arc<dyn Titler>,
    bus: InterruptBus,
    events: Arc<dyn EventSink>,
    idle_timeout: Duration,
}

impl SessionScheduler {
    pub fn new(
        store: Arc<dyn SessionStore>,
        clock: Arc<dyn Clock>,
        summarizer: Arc<dyn Summarizer>,
        titler: Arc<dyn Titler>,
        bus: InterruptBus,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            store,
            clock,
            summarizer,
            titler,
            bus,
            events,
            idle_timeout: Duration::from_secs(12 * 60 * 60),
        }
    }

    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    pub fn interrupt_bus(&self) -> InterruptBus {
        self.bus.clone()
    }

    /// 新消息到达：定位活动会话并追加消息。
    /// 空闲超时只发提示事件（`Event::SessionIdle`），不自动切换会话——是否开新话题由用户决定。
    pub async fn on_new_message(&self, text: &str) -> Result<TurnContext, SchedulerError> {
        self.on_new_message_with_display(text, None).await
    }

    /// 带前端展示文本的新消息（force_tool 场景）：落盘的 user 消息同时携带
    /// display_text（渲染用）与 text（模型指令），二者分离（ADR-0007 修订）。
    pub async fn on_new_message_with_display(
        &self,
        text: &str,
        display_text: Option<&str>,
    ) -> Result<TurnContext, SchedulerError> {
        self.on_new_message_with_attachments(text, display_text, Vec::new())
            .await
    }

    /// 带图片附件引用的新消息（ADR-0046：图片随消息进入上下文）。
    pub async fn on_new_message_with_attachments(
        &self,
        text: &str,
        display_text: Option<&str>,
        attachment_refs: Vec<crate::kernel::message::AttachmentRef>,
    ) -> Result<TurnContext, SchedulerError> {
        let now = self.clock.now();
        let metas = self.store.list_sessions().await?;
        let active = metas
            .iter()
            .find(|m| m.status == SessionStatus::Active)
            .cloned();
        let Some(meta) = active else {
            // 首条消息：建新会话（不产生切换中断）。
            let to = self
                .create_first_session(
                    Goal {
                        text: text.chars().take(40).collect(),
                    },
                    now,
                )
                .await?;
            return self
                .append_user(&to, text, display_text, &attachment_refs)
                .await;
        };

        let idle = now - meta.last_activity_at
            > chrono::Duration::from_std(self.idle_timeout).unwrap_or(chrono::Duration::hours(12));
        if idle {
            // 系统级空闲超时：提示用户，不代其决定（ADR-0044）。
            self.events.emit(Event::SessionIdle {
                session: meta.key,
                idle_seconds: (now - meta.last_activity_at).num_seconds(),
            });
        }
        self.continue_in(&meta, text, display_text, &attachment_refs, now)
            .await
    }

    /// 在活动会话中追加用户消息并推进 active_path。
    async fn continue_in(
        &self,
        meta: &SessionMeta,
        text: &str,
        display_text: Option<&str>,
        attachment_refs: &[crate::kernel::message::AttachmentRef],
        now: DateTime<Utc>,
    ) -> Result<TurnContext, SchedulerError> {
        self.store.set_last_activity(&meta.key, now).await?;
        let mut user_msg = Message::user_with_attachments(
            text,
            display_text.map(str::to_string),
            attachment_refs.to_vec(),
        );
        let path = self.store.read_path(&meta.key).await?;
        user_msg.parent_id = path.last().map(|m| m.id);
        self.store.append_message(&meta.key, &user_msg).await?;
        self.store
            .set_active_path(&meta.key, Some(user_msg.id))
            .await?;
        Ok(TurnContext {
            session_key: meta.key,
            messages: self.store.read_path(&meta.key).await?,
        })
    }

    /// 在指定会话中追加用户消息并推进 active_path（新建会话后使用）。
    async fn append_user(
        &self,
        key: &SessionKey,
        text: &str,
        display_text: Option<&str>,
        attachment_refs: &[crate::kernel::message::AttachmentRef],
    ) -> Result<TurnContext, SchedulerError> {
        let mut user_msg = Message::user_with_attachments(
            text,
            display_text.map(str::to_string),
            attachment_refs.to_vec(),
        );
        let path = self.store.read_path(key).await?;
        user_msg.parent_id = path.last().map(|m| m.id);
        self.store.append_message(key, &user_msg).await?;
        self.store.set_active_path(key, Some(user_msg.id)).await?;
        Ok(TurnContext {
            session_key: *key,
            messages: self.store.read_path(key).await?,
        })
    }

    /// 用户手动新建会话（ADR-0044）：归档当前活动会话，新建独立 SessionKey。
    ///
    /// `carry_summary` 为真时，先用旧会话生成交接摘要并挂成新会话首条系统消息——
    /// 新会话首条用户消息随后挂在它下面，旧会话内容不会进入新会话上下文。
    pub async fn create_new_session(
        &self,
        goal: Option<Goal>,
        carry_summary: bool,
    ) -> Result<CreatedSession, SchedulerError> {
        let now = self.clock.now();
        let actives: Vec<SessionMeta> = self
            .store
            .list_sessions()
            .await?
            .into_iter()
            .filter(|m| m.status == SessionStatus::Active)
            .collect();

        // 摘要先于归档生成，避免依赖归档后的读语义。
        let mut handoff: Option<String> = None;
        if carry_summary && let Some(old) = actives.first() {
            let all = self.store.read_all(&old.key).await?;
            if !all.is_empty() {
                let text = self.summarizer.summarize(&all, old.goal.as_ref()).await;
                if !text.trim().is_empty() {
                    handoff = Some(text);
                }
            }
        }

        self.archive_all_active().await?;

        let key = SessionKey::new();
        let goal = goal.or_else(|| {
            handoff.as_ref().map(|s| Goal {
                text: s.chars().take(40).collect(),
            })
        });
        self.create_session(key, goal, now).await?;

        let mut summary_attached = false;
        if let Some(text) = handoff {
            let mut summary_msg =
                Message::system_with_display(format!("上一会话梗概：{text}"), None);
            summary_msg.created_at = now;
            self.store.append_message(&key, &summary_msg).await?;
            self.store
                .set_active_path(&key, Some(summary_msg.id))
                .await?;
            summary_attached = true;
        }

        Ok(CreatedSession {
            key,
            archived: actives.first().map(|m| m.key),
            summary_attached,
        })
    }

    /// 用户切换到既有会话（GUI 会话列表点击）：归档当前活动会话，把目标会话置为活动。
    ///
    /// 单 Active 不变量：先归档全部 Active，再激活目标——不更新 `last_activity_at`
    /// （那是消息活动时间，空闲超时判定依赖它，切换会话不算发言）。
    pub async fn open_existing(&self, key: SessionKey) -> Result<(), SchedulerError> {
        if self.store.get_session(&key).await?.is_none() {
            return Err(SchedulerError::Internal(format!("会话不存在：{key}")));
        }
        self.archive_all_active().await?;
        self.store.activate(&key).await?;
        Ok(())
    }

    /// 首回合结束后生成会话标题（侧栏会话列表用），返回新标题。
    ///
    /// 仅在尚无标题、且已有用户消息与助手回复时触发一次；模型输出为空则写截断兜底文本。
    /// 写入后 `title` 非空，后续回合不再触发（也不会覆盖用户改的名）。
    pub async fn maybe_generate_title(
        &self,
        key: &SessionKey,
    ) -> Result<Option<String>, SchedulerError> {
        let Some(meta) = self.store.get_session(key).await? else {
            return Ok(None);
        };
        if meta.title.as_deref().is_some_and(|t| !t.trim().is_empty()) {
            return Ok(None);
        }
        let messages = self.store.read_all(key).await?;
        let has_user = messages
            .iter()
            .any(|m| matches!(m.kind, crate::kernel::message::MessageKind::User { .. }));
        let has_assistant = messages.iter().any(|m| {
            matches!(
                m.kind,
                crate::kernel::message::MessageKind::Assistant { .. }
            )
        });
        if !has_user || !has_assistant {
            return Ok(None);
        }
        let generated = self.titler.title(&messages).await;
        // 模型调用期间用户可能已手动改名：写回前复查，绝不覆盖用户输入。
        if let Some(meta) = self.store.get_session(key).await?
            && meta.title.as_deref().is_some_and(|t| !t.trim().is_empty())
        {
            return Ok(None);
        }
        let title = if generated.trim().is_empty() {
            super::title::fallback_title(&messages)
        } else {
            generated
        };
        self.store.set_title(key, Some(&title)).await?;
        Ok(Some(title))
    }

    /// 归档全部活动会话（单 Active 不变量的唯一保证点）。
    async fn archive_all_active(&self) -> Result<(), SchedulerError> {
        for meta in self
            .store
            .list_sessions()
            .await?
            .iter()
            .filter(|m| m.status == SessionStatus::Active)
        {
            self.store.archive(&meta.key).await?;
        }
        Ok(())
    }

    /// 创建根会话（仅首条消息调用）：根会话没有摘要节点，直接以用户消息开头。
    async fn create_first_session(
        &self,
        goal: Goal,
        now: DateTime<Utc>,
    ) -> Result<SessionKey, SchedulerError> {
        let new_key = SessionKey::new();
        self.create_session(new_key, Some(goal), now).await?;
        Ok(new_key)
    }

    async fn create_session(
        &self,
        key: SessionKey,
        goal: Option<Goal>,
        now: DateTime<Utc>,
    ) -> Result<(), SchedulerError> {
        let mut meta = SessionMeta::new(key);
        meta.goal = goal;
        meta.created_at = now;
        meta.last_activity_at = now;
        self.store.create_session(&key, &meta).await?;
        Ok(())
    }
}
