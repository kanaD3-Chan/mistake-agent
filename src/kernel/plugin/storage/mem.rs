//! storage 服务：M1 内存实现 + M2 文件持久化（会话 JSONL / 错题 JSON / 审计 JSONL 轮转）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::kernel::agent::session::{Goal, SessionKey, SessionMeta, SessionStatus};
use crate::kernel::audit::{AuditRecord, AuditSink};
use crate::kernel::message::{Message, MessageId, MessageKind};
use crate::kernel::plugin::services::{
    Mistake, MistakeFilter, MistakeId, MistakePatch, MistakeStore, SessionStore, StorageError,
    StorageService,
};

use super::Inner;

use super::active_chain;

/// M1 内存 storage（M2 换成 JSONL + 错题本 JSON）。
#[derive(Clone)]
pub struct MemoryStorage {
    inner: Arc<Mutex<Inner>>,
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }
}

#[async_trait]
impl SessionStore for MemoryStorage {
    async fn create_session(
        &self,
        key: &SessionKey,
        meta: &SessionMeta,
    ) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        if inner.sessions.contains_key(key) {
            return Err(StorageError::AlreadyExists(key.to_string()));
        }
        inner.sessions.insert(*key, meta.clone());
        inner.messages.insert(*key, Vec::new());
        Ok(())
    }

    async fn get_session(&self, key: &SessionKey) -> Result<Option<SessionMeta>, StorageError> {
        Ok(self
            .inner
            .lock()
            .expect("storage poisoned")
            .sessions
            .get(key)
            .cloned())
    }

    async fn append_message(&self, key: &SessionKey, msg: &Message) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let path = inner
            .messages
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?;
        path.push(msg.clone());
        Ok(())
    }

    async fn read_path(&self, key: &SessionKey) -> Result<Vec<Message>, StorageError> {
        let (messages, active_path) = {
            let inner = self.inner.lock().expect("storage poisoned");
            let messages = inner
                .messages
                .get(key)
                .cloned()
                .ok_or(StorageError::SessionNotFound(*key))?;
            let active_path = inner.sessions.get(key).and_then(|m| m.active_path);
            (messages, active_path)
        };
        Ok(active_chain(&messages, active_path))
    }

    async fn read_all(&self, key: &SessionKey) -> Result<Vec<Message>, StorageError> {
        Ok(self
            .inner
            .lock()
            .expect("storage poisoned")
            .messages
            .get(key)
            .cloned()
            .unwrap_or_default())
    }

    async fn set_active_path(
        &self,
        key: &SessionKey,
        message_id: Option<MessageId>,
    ) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let meta = inner
            .sessions
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?;
        meta.active_path = message_id;
        Ok(())
    }

    async fn derive_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
        text: &str,
    ) -> Result<Vec<Message>, StorageError> {
        let (messages, active_path) = {
            let inner = self.inner.lock().expect("storage poisoned");
            let messages = inner
                .messages
                .get(key)
                .cloned()
                .ok_or(StorageError::SessionNotFound(*key))?;
            let active_path = inner.sessions.get(key).and_then(|m| m.active_path);
            (messages, active_path)
        };
        let chain = active_chain(&messages, active_path);
        let idx = chain
            .iter()
            .position(|m| m.id == message_id)
            .ok_or(StorageError::Internal("消息不在活跃路径".into()))?;
        let original = &chain[idx];
        let mut new_msg = original.clone();
        new_msg.id = MessageId::new();
        new_msg.parent_id = original.parent_id;
        new_msg.created_at = chrono::Utc::now();
        match &mut new_msg.kind {
            // 仅允许编辑用户消息（改完重发）；assistant 等由模型生成，不可手改。
            MessageKind::User {
                text: t,
                display_text,
                ..
            } => {
                // 编辑用户消息：新文本作为模型指令与展示文本，附件保留（改完重发语义）。
                *t = text.to_string();
                *display_text = None;
            }
            _ => {
                return Err(StorageError::Internal("只能编辑 user 消息".into()));
            }
        };

        let mut new_path = chain[..idx].to_vec();
        new_path.push(new_msg.clone());
        let mut inner = self.inner.lock().expect("storage poisoned");
        let path = inner
            .messages
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?;
        path.push(new_msg);
        let meta = inner
            .sessions
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?;
        meta.active_path = new_path.last().map(|m| m.id);
        Ok(new_path)
    }

    async fn switch_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
    ) -> Result<Vec<Message>, StorageError> {
        let messages = self.read_all(key).await?;
        if !messages.iter().any(|m| m.id == message_id) {
            return Err(StorageError::Internal("消息不存在".into()));
        }
        let chain = active_chain(&messages, Some(message_id));
        self.set_active_path(key, Some(message_id)).await?;
        Ok(chain)
    }

    async fn splice_compaction(
        &self,
        key: &SessionKey,
        summary: &Message,
        tail_start: MessageId,
    ) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let path = inner
            .messages
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?;
        let tail = path
            .iter_mut()
            .find(|m| m.id == tail_start)
            .ok_or(StorageError::Internal("保留段首条不存在".into()))?;
        tail.parent_id = Some(summary.id);
        path.push(summary.clone());
        Ok(())
    }

    async fn set_goal(&self, key: &SessionKey, goal: &Goal) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let meta = inner
            .sessions
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?;
        meta.goal = Some(goal.clone());
        Ok(())
    }

    async fn set_title(&self, key: &SessionKey, title: Option<&str>) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let meta = inner
            .sessions
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?;
        meta.title = title
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string);
        Ok(())
    }

    async fn archive(&self, key: &SessionKey) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let meta = inner
            .sessions
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?;
        meta.status = SessionStatus::Archived;
        meta.archived_at = Some(chrono::Utc::now());
        Ok(())
    }

    async fn activate(&self, key: &SessionKey) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let meta = inner
            .sessions
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?;
        meta.status = SessionStatus::Active;
        meta.archived_at = None;
        Ok(())
    }

    async fn remove_session(&self, key: &SessionKey) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        if inner.sessions.remove(key).is_none() {
            return Err(StorageError::SessionNotFound(*key));
        }
        inner.messages.remove(key);
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<SessionMeta>, StorageError> {
        Ok(self
            .inner
            .lock()
            .expect("storage poisoned")
            .sessions
            .values()
            .cloned()
            .collect())
    }

    async fn set_last_activity(
        &self,
        key: &SessionKey,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let meta = inner
            .sessions
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?;
        meta.last_activity_at = at;
        Ok(())
    }
}

#[async_trait]
impl MistakeStore for MemoryStorage {
    async fn save(&self, mistake: &Mistake) -> Result<MistakeId, StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        if inner.mistakes.iter().any(|m| m.id == mistake.id) {
            return Err(StorageError::AlreadyExists(mistake.id.to_string()));
        }
        inner.mistakes.push(mistake.clone());
        Ok(mistake.id)
    }

    async fn get(&self, id: &MistakeId) -> Result<Option<Mistake>, StorageError> {
        Ok(self
            .inner
            .lock()
            .expect("storage poisoned")
            .mistakes
            .iter()
            .find(|m| m.id == *id && m.deleted_at.is_none())
            .cloned())
    }

    async fn list(&self, filter: &MistakeFilter) -> Result<Vec<Mistake>, StorageError> {
        let inner = self.inner.lock().expect("storage poisoned");
        Ok(inner
            .mistakes
            .iter()
            .filter(|m| {
                filter
                    .subject
                    .as_deref()
                    .map(|s| m.subject == s)
                    .unwrap_or(true)
                    && filter
                        .knowledge_point
                        .as_deref()
                        .map(|k| m.knowledge_point == k)
                        .unwrap_or(true)
                    && filter.is_correct.map(|c| m.is_correct == c).unwrap_or(true)
                    && m.deleted_at.is_none()
            })
            .cloned()
            .collect())
    }

    async fn update(&self, id: &MistakeId, patch: &MistakePatch) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let m = inner
            .mistakes
            .iter_mut()
            .find(|m| m.id == *id)
            .ok_or(StorageError::MistakeNotFound(id.to_string()))?;
        if m.deleted_at.is_some() {
            return Err(StorageError::MistakeNotFound(id.to_string()));
        }
        if let Some(s) = &patch.subject {
            m.subject = s.clone();
        }
        if let Some(k) = &patch.knowledge_point {
            m.knowledge_point = k.clone();
        }
        if let Some(q) = &patch.question {
            m.question = q.clone();
        }
        if let Some(s) = &patch.student_answer {
            m.student_answer = s.clone();
        }
        if let Some(r) = &patch.reference_answer {
            m.reference_answer = r.clone();
        }
        if let Some(a) = &patch.analysis {
            m.analysis = a.clone();
        }
        if let Some(c) = patch.is_correct {
            m.is_correct = c;
        }
        if let Some(p) = patch.pinned {
            m.pinned = p;
        }
        Ok(())
    }

    async fn remove(&self, id: &MistakeId) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let m = inner
            .mistakes
            .iter_mut()
            .find(|m| m.id == *id)
            .ok_or(StorageError::MistakeNotFound(id.to_string()))?;
        if m.deleted_at.is_none() {
            m.deleted_at = Some(chrono::Utc::now());
        }
        Ok(())
    }

    async fn remove_many(&self, ids: &[MistakeId]) -> Result<usize, StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let now = chrono::Utc::now();
        let mut deleted = 0usize;
        for id in ids {
            if let Some(m) = inner.mistakes.iter_mut().find(|m| m.id == *id)
                && m.deleted_at.is_none()
            {
                m.deleted_at = Some(now);
                deleted += 1;
            }
        }
        Ok(deleted)
    }
}

impl AuditSink for MemoryStorage {
    fn append(&self, record: AuditRecord) {
        self.inner
            .lock()
            .expect("storage poisoned")
            .audit
            .push(record);
    }
}

impl StorageService for MemoryStorage {}

// ---------- 域内文件 IO / 暂存 IO（ADR-0042，内存模拟供测试与回退） ----------

use crate::kernel::plugin::services::{Domain, DomainIo, RelPath, TmpIo};

#[async_trait]
impl DomainIo for MemoryStorage {
    async fn read(&self, domain: Domain, rel: &RelPath) -> Result<Vec<u8>, StorageError> {
        let inner = self.inner.lock().expect("storage poisoned");
        let key = format!("{}/{}", domain.as_dir(), rel.as_str());
        inner
            .files
            .get(&key)
            .cloned()
            .ok_or_else(|| StorageError::Io(format!("内存文件不存在：{key}")))
    }

    async fn write(&self, domain: Domain, rel: &RelPath, bytes: &[u8]) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let key = format!("{}/{}", domain.as_dir(), rel.as_str());
        inner.files.insert(key, bytes.to_vec());
        Ok(())
    }

    async fn remove(&self, domain: Domain, rel: &RelPath) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let key = format!("{}/{}", domain.as_dir(), rel.as_str());
        if inner.files.remove(&key).is_none() {
            return Err(StorageError::Io(format!("内存文件不存在：{key}")));
        }
        Ok(())
    }

    async fn remove_tree(&self, domain: Domain, rel: &RelPath) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let prefix = format!("{}/{}/", domain.as_dir(), rel.as_str());
        let exact = format!("{}/{}", domain.as_dir(), rel.as_str());
        let keys: Vec<String> = inner
            .files
            .keys()
            .filter(|k| **k == exact || k.starts_with(&prefix))
            .cloned()
            .collect();
        if keys.is_empty() {
            return Err(StorageError::Io("内存子树不存在".into()));
        }
        for k in keys {
            inner.files.remove(&k);
        }
        Ok(())
    }

    async fn list(&self, domain: Domain) -> Result<Vec<String>, StorageError> {
        let inner = self.inner.lock().expect("storage poisoned");
        let prefix = format!("{}/", domain.as_dir());
        let mut out: Vec<String> = inner
            .files
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .map(|k| k[prefix.len()..].to_string())
            .collect();
        out.sort();
        Ok(out)
    }

    async fn read_legacy(&self, domain: Domain, legacy_rel: &str) -> Result<Vec<u8>, StorageError> {
        // 内存模拟：历史路径直接作 key（`domain/legacy_rel`）。
        let inner = self.inner.lock().expect("storage poisoned");
        let key = format!("{}/{}", domain.as_dir(), legacy_rel);
        inner
            .files
            .get(&key)
            .cloned()
            .ok_or_else(|| StorageError::Io(format!("内存历史文件不存在：{key}")))
    }

    async fn remove_legacy(&self, domain: Domain, legacy_rel: &str) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        let key = format!("{}/{}", domain.as_dir(), legacy_rel);
        if inner.files.remove(&key).is_none() {
            return Err(StorageError::Io(format!("内存历史文件不存在：{key}")));
        }
        Ok(())
    }
}

#[async_trait]
impl TmpIo for MemoryStorage {
    async fn read_staged(&self, path: &str) -> Result<Vec<u8>, StorageError> {
        let inner = self.inner.lock().expect("storage poisoned");
        inner
            .files
            .get(path)
            .cloned()
            .ok_or_else(|| StorageError::Io(format!("内存暂存不存在：{path}")))
    }

    async fn remove_staged(&self, path: &str) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().expect("storage poisoned");
        if inner.files.remove(path).is_none() {
            return Err(StorageError::Io(format!("内存暂存不存在：{path}")));
        }
        Ok(())
    }
}
