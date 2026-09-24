//! storage 文件后端（M2 持久化）：会话 JSONL / 错题 JSON / 审计轮转 / 域内 IO。

mod mistakes;
mod io;
mod migrate;
mod tmp;
#[cfg(test)]
mod tests;

// 子模块间共享的辅助函数（rotate 在 tmp 段，mistakes 的审计落盘也用）。
pub(crate) use tmp::rotate_if_large;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::kernel::agent::session::{Goal, SessionKey, SessionMeta, SessionStatus};
use crate::kernel::audit::{AuditRecord, AuditSink};
use crate::kernel::message::{Message, MessageId, MessageKind};
use crate::kernel::plugin::services::{
    Mistake, MistakeFilter, MistakeId, MistakePatch, MistakeStore, SessionStore, StorageError,
    StorageService,
};
use std::path::{Path, PathBuf};

use super::Inner;

use super::active_chain;

#[derive(Clone)]
pub struct FileStorage {
    root: PathBuf,
    inner: Arc<Mutex<Inner>>,
}

impl FileStorage {
    pub fn open(root: &Path) -> Result<Self, StorageError> {
        let sessions_dir = root.join("sessions");
        let mistakes_dir = root.join("mistakes");
        // 子目录已由 bootstrap::init_data_root（Kernel::new 引导）创建，此处不再懒创建。

        let inner = Arc::new(Mutex::new(Inner::default()));
        // 加载错题本。
        let mistakes_file = mistakes_dir.join("mistakes.json");
        if let Ok(text) = std::fs::read_to_string(&mistakes_file) {
            let loaded: Vec<Mistake> = serde_json::from_str(&text)
                .map_err(|e| StorageError::Corrupt(format!("错题本解析失败：{e}")))?;
            inner.lock().expect("storage poisoned").mistakes = loaded;
        }
        // 存量会话迁移：老式「模型自动切换」留下的多话题文件拆成独立会话（幂等，非致命）。
        migrate::migrate_legacy_sessions(&sessions_dir);
        // 加载会话（每个 <key>.jsonl 首行为元数据）。
        let mut loaded_metas = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Some((meta_line, _rest)) = text.split_once('\n') else {
                    continue;
                };
                if let Ok(meta) = serde_json::from_str::<SessionMeta>(meta_line) {
                    let key = meta.key;
                    let messages: Vec<Message> = text
                        .lines()
                        .skip(1)
                        .filter(|l| !l.trim().is_empty())
                        .filter_map(|l| serde_json::from_str(l).ok())
                        .collect();
                    let mut inner = inner.lock().expect("storage poisoned");
                    inner.sessions.insert(key, meta);
                    inner.messages.insert(key, messages);
                    loaded_metas.push(key);
                }
            }
        }
        let _ = loaded_metas;
        Ok(Self {
            root: root.to_path_buf(),
            inner,
        })
    }

    fn session_path(&self, key: &SessionKey) -> PathBuf {
        self.root.join("sessions").join(format!("{key}.jsonl"))
    }

    fn mistakes_path(&self) -> PathBuf {
        self.root.join("mistakes").join("mistakes.json")
    }

    fn audit_path(&self) -> PathBuf {
        self.root.join("audit").join("audit.jsonl")
    }

    fn persist_mistakes(&self) -> Result<(), StorageError> {
        let mistakes = self
            .inner
            .lock()
            .expect("storage poisoned")
            .mistakes
            .clone();
        atomic_write_json(&self.mistakes_path(), &mistakes)
    }

    fn persist_session_meta(&self, key: &SessionKey) -> Result<(), StorageError> {
        let (meta, messages) = {
            let inner = self.inner.lock().expect("storage poisoned");
            let meta = inner
                .sessions
                .get(key)
                .ok_or(StorageError::SessionNotFound(*key))?
                .clone();
            let messages = inner.messages.get(key).cloned().unwrap_or_default();
            (meta, messages)
        };
        let path = self.session_path(key);
        let mut out = String::new();
        out.push_str(
            &serde_json::to_string(&meta)
                .map_err(|e| StorageError::Io(format!("会话元数据序列化失败：{e}")))?,
        );
        out.push('\n');
        for msg in &messages {
            out.push_str(
                &serde_json::to_string(msg)
                    .map_err(|e| StorageError::Io(format!("消息序列化失败：{e}")))?,
            );
            out.push('\n');
        }
        atomic_write_str(&path, &out)
    }
}

fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), StorageError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| StorageError::Io(format!("序列化失败：{e}")))?;
    atomic_write_str(path, &text)
}

fn atomic_write_str(path: &Path, text: &str) -> Result<(), StorageError> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| StorageError::Io(format!("写临时文件失败：{e}")))?;
    std::fs::rename(&tmp, path).map_err(|e| StorageError::Io(format!("原子改名失败：{e}")))?;
    Ok(())
}

#[async_trait]
impl SessionStore for FileStorage {
    async fn create_session(
        &self,
        key: &SessionKey,
        meta: &SessionMeta,
    ) -> Result<(), StorageError> {
        {
            let mut inner = self.inner.lock().expect("storage poisoned");
            if inner.sessions.contains_key(key) {
                return Err(StorageError::AlreadyExists(key.to_string()));
            }
            inner.sessions.insert(*key, meta.clone());
            inner.messages.insert(*key, Vec::new());
        }
        self.persist_session_meta(key)
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
        {
            let mut inner = self.inner.lock().expect("storage poisoned");
            let path = inner
                .messages
                .get_mut(key)
                .ok_or(StorageError::SessionNotFound(*key))?;
            path.push(msg.clone());
        }
        let path = self.session_path(key);
        let mut line = serde_json::to_string(msg)
            .map_err(|e| StorageError::Io(format!("消息序列化失败：{e}")))?;
        line.push('\n');
        append_line(&path, &line)
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
        self.inner
            .lock()
            .expect("storage poisoned")
            .sessions
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?
            .active_path = message_id;
        self.persist_session_meta(key)
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
        {
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
        }
        self.persist_session_meta(key)?;
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
        {
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
        }
        self.persist_session_meta(key)
    }

    async fn set_goal(&self, key: &SessionKey, goal: &Goal) -> Result<(), StorageError> {
        self.inner
            .lock()
            .expect("storage poisoned")
            .sessions
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?
            .goal = Some(goal.clone());
        self.persist_session_meta(key)
    }

    async fn set_title(&self, key: &SessionKey, title: Option<&str>) -> Result<(), StorageError> {
        self.inner
            .lock()
            .expect("storage poisoned")
            .sessions
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?
            .title = title
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string);
        self.persist_session_meta(key)
    }

    async fn archive(&self, key: &SessionKey) -> Result<(), StorageError> {
        {
            let mut inner = self.inner.lock().expect("storage poisoned");
            let meta = inner
                .sessions
                .get_mut(key)
                .ok_or(StorageError::SessionNotFound(*key))?;
            meta.status = SessionStatus::Archived;
            meta.archived_at = Some(chrono::Utc::now());
        }
        self.persist_session_meta(key)
    }

    async fn activate(&self, key: &SessionKey) -> Result<(), StorageError> {
        {
            let mut inner = self.inner.lock().expect("storage poisoned");
            let meta = inner
                .sessions
                .get_mut(key)
                .ok_or(StorageError::SessionNotFound(*key))?;
            meta.status = SessionStatus::Active;
            meta.archived_at = None;
        }
        self.persist_session_meta(key)
    }

    async fn remove_session(&self, key: &SessionKey) -> Result<(), StorageError> {
        {
            let mut inner = self.inner.lock().expect("storage poisoned");
            if inner.sessions.remove(key).is_none() {
                return Err(StorageError::SessionNotFound(*key));
            }
            inner.messages.remove(key);
        }
        // 会话文件删除失败不阻塞（内存态已移除，重启后残留文件仍可读回）。
        let path = self.session_path(key);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| StorageError::Io(format!("删除会话文件失败 {path:?}：{e}")))?;
        }
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
        self.inner
            .lock()
            .expect("storage poisoned")
            .sessions
            .get_mut(key)
            .ok_or(StorageError::SessionNotFound(*key))?
            .last_activity_at = at;
        self.persist_session_meta(key)
    }
}

fn append_line(path: &Path, line: &str) -> Result<(), StorageError> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| StorageError::Io(format!("打开失败 {path:?}：{e}")))?;
    file.write_all(line.as_bytes())
        .map_err(|e| StorageError::Io(format!("追加失败：{e}")))?;
    Ok(())
}
