//! 默认服务：非插件、不注册工具、不占 namespace（M2 骨架，开箱即用）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::message::{Message, MessageId};
use crate::services::{
    AbortSignal, ItemKind, ModelChunk, ModelError, ModelRequest, ModelResponse, ModelService,
    ModelStream, SessionError, SessionKey, SessionMeta, SessionStatus, SessionStore, TokenUsage,
    ToolCallSpec,
};
use async_trait::async_trait;

#[derive(Default, Clone)]
pub struct InMemorySessionStore {
    sessions: Arc<Mutex<HashMap<SessionKey, SessionMeta>>>,
    messages: Arc<Mutex<HashMap<SessionKey, Vec<Message>>>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn create_session(
        &self,
        key: &SessionKey,
        meta: &SessionMeta,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("store poisoned");
        if sessions.contains_key(key) {
            return Err(SessionError::AlreadyExists(key.to_string()));
        }
        sessions.insert(*key, meta.clone());
        self.messages
            .lock()
            .expect("store poisoned")
            .insert(*key, Vec::new());
        Ok(())
    }

    async fn get_session(&self, key: &SessionKey) -> Result<Option<SessionMeta>, SessionError> {
        Ok(self
            .sessions
            .lock()
            .expect("store poisoned")
            .get(key)
            .cloned())
    }

    async fn append_message(&self, key: &SessionKey, msg: &Message) -> Result<(), SessionError> {
        let mut all = self.messages.lock().expect("store poisoned");
        let list = all.get_mut(key).ok_or(SessionError::NotFound(*key))?;
        list.push(msg.clone());
        Ok(())
    }

    async fn read_path(&self, key: &SessionKey) -> Result<Vec<Message>, SessionError> {
        let sessions = self.sessions.lock().expect("store poisoned");
        let meta = sessions.get(key).ok_or(SessionError::NotFound(*key))?;
        let all = self.messages.lock().expect("store poisoned");
        let list = all.get(key).ok_or(SessionError::NotFound(*key))?;
        let Some(end) = meta.active_path else {
            return Ok(list.clone());
        };
        Ok(path_to(&end, list))
    }

    async fn read_all(&self, key: &SessionKey) -> Result<Vec<Message>, SessionError> {
        Ok(self
            .messages
            .lock()
            .expect("store poisoned")
            .get(key)
            .cloned()
            .unwrap_or_default())
    }

    async fn set_active_path(
        &self,
        key: &SessionKey,
        message_id: Option<MessageId>,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("store poisoned");
        let meta = sessions.get_mut(key).ok_or(SessionError::NotFound(*key))?;
        meta.active_path = message_id;
        Ok(())
    }

    async fn derive_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
        text: &str,
    ) -> Result<Vec<Message>, SessionError> {
        let edited = {
            let mut all = self.messages.lock().expect("store poisoned");
            let list = all.get_mut(key).ok_or(SessionError::NotFound(*key))?;
            let index = list
                .iter()
                .position(|m| m.id == message_id)
                .ok_or_else(|| SessionError::Internal("消息不存在".into()))?;
            let original = list[index].clone();
            let mut edited = original.clone();
            edited.id = MessageId::new();
            edited.kind = crate::message::MessageKind::User {
                text: text.to_string(),
                display_text: None,
                attachments: Vec::new(),
            };
            list.truncate(index + 1);
            list.push(edited.clone());
            edited
        };
        self.set_active_path(key, Some(edited.id)).await?;
        Ok(vec![edited])
    }

    async fn switch_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
    ) -> Result<Vec<Message>, SessionError> {
        let path = {
            let all = self.messages.lock().expect("store poisoned");
            let list = all.get(key).ok_or(SessionError::NotFound(*key))?;
            if !list.iter().any(|m| m.id == message_id) {
                return Err(SessionError::Internal("消息不存在".into()));
            }
            path_to(&message_id, list)
        };
        self.set_active_path(key, Some(message_id)).await?;
        Ok(path)
    }

    async fn set_goal(
        &self,
        key: &SessionKey,
        goal: &crate::services::Goal,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("store poisoned");
        let meta = sessions.get_mut(key).ok_or(SessionError::NotFound(*key))?;
        meta.goal = Some(goal.clone());
        Ok(())
    }

    async fn archive(&self, key: &SessionKey) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("store poisoned");
        let meta = sessions.get_mut(key).ok_or(SessionError::NotFound(*key))?;
        meta.status = SessionStatus::Archived;
        meta.archived_at = Some(chrono::Utc::now());
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<SessionMeta>, SessionError> {
        let mut metas: Vec<_> = self
            .sessions
            .lock()
            .expect("store poisoned")
            .values()
            .cloned()
            .collect();
        metas.sort_by_key(|m| m.last_activity_at);
        Ok(metas)
    }

    async fn set_last_activity(
        &self,
        key: &SessionKey,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("store poisoned");
        let meta = sessions.get_mut(key).ok_or(SessionError::NotFound(*key))?;
        meta.last_activity_at = at;
        Ok(())
    }
}

fn path_to(end: &MessageId, list: &[Message]) -> Vec<Message> {
    let mut out = Vec::new();
    let mut current = *end;
    while let Some(msg) = list.iter().find(|m| m.id == current) {
        out.push(msg.clone());
        match msg.parent_id {
            Some(parent) => current = parent,
            None => break,
        }
    }
    out.reverse();
    out
}

/// 固定文本模型桩：链路自检/测试用，不依赖真实 API。
#[derive(Clone, Copy)]
pub struct MockModelService {
    reply: &'static str,
}

impl MockModelService {
    pub fn new(reply: &'static str) -> Self {
        Self { reply }
    }
}

impl Default for MockModelService {
    fn default() -> Self {
        Self::new("你好，我是 so-lite-agent。")
    }
}

#[async_trait]
impl ModelService for MockModelService {
    async fn stream(
        &self,
        _request: &ModelRequest,
        _signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError> {
        let chunks = vec![
            Ok(ModelChunk::TextDelta(self.reply.to_string())),
            Ok(ModelChunk::ItemDone {
                kind: ItemKind::Message,
            }),
            Ok(ModelChunk::Usage(TokenUsage {
                input_tokens: Some(1),
                output_tokens: Some(1),
                ..Default::default()
            })),
            Ok(ModelChunk::Done),
        ];
        Ok(Box::new(futures_util::stream::iter(chunks)))
    }

    async fn complete(
        &self,
        _request: &ModelRequest,
        _signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse {
            text: self.reply.to_string(),
            tool_calls: Vec::<ToolCallSpec>::new(),
            usage: Some(TokenUsage {
                input_tokens: Some(1),
                output_tokens: Some(1),
                ..Default::default()
            }),
        })
    }
}
