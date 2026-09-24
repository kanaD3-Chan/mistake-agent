//! storage 服务：内存/文件后端统一路由（AnyStorage）+ 消息树链。

mod chain;

use std::collections::HashMap;

use async_trait::async_trait;

use super::file::FileStorage;
use super::mem::MemoryStorage;
use crate::kernel::agent::session::{Goal, SessionKey, SessionMeta, SessionStatus};
use crate::kernel::audit::{AuditRecord, AuditSink};
use crate::kernel::message::{Message, MessageId};
use crate::kernel::plugin::services::{
    Domain, DomainIo, Mistake, MistakeFilter, MistakeId, MistakePatch, MistakeStore, RelPath,
    SessionStore, StorageError, StorageService, TmpIo,
};

#[derive(Clone)]
pub enum AnyStorage {
    File(FileStorage),
    Mem(MemoryStorage),
}

#[async_trait]
impl SessionStore for AnyStorage {
    async fn create_session(
        &self,
        key: &SessionKey,
        meta: &SessionMeta,
    ) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => s.create_session(key, meta).await,
            AnyStorage::Mem(s) => s.create_session(key, meta).await,
        }
    }
    async fn get_session(&self, key: &SessionKey) -> Result<Option<SessionMeta>, StorageError> {
        match self {
            AnyStorage::File(s) => s.get_session(key).await,
            AnyStorage::Mem(s) => s.get_session(key).await,
        }
    }
    async fn append_message(&self, key: &SessionKey, msg: &Message) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => s.append_message(key, msg).await,
            AnyStorage::Mem(s) => s.append_message(key, msg).await,
        }
    }
    async fn read_path(&self, key: &SessionKey) -> Result<Vec<Message>, StorageError> {
        match self {
            AnyStorage::File(s) => s.read_path(key).await,
            AnyStorage::Mem(s) => s.read_path(key).await,
        }
    }
    async fn read_all(&self, key: &SessionKey) -> Result<Vec<Message>, StorageError> {
        match self {
            AnyStorage::File(s) => s.read_all(key).await,
            AnyStorage::Mem(s) => s.read_all(key).await,
        }
    }
    async fn set_active_path(
        &self,
        key: &SessionKey,
        message_id: Option<MessageId>,
    ) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => s.set_active_path(key, message_id).await,
            AnyStorage::Mem(s) => s.set_active_path(key, message_id).await,
        }
    }
    async fn derive_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
        text: &str,
    ) -> Result<Vec<Message>, StorageError> {
        match self {
            AnyStorage::File(s) => s.derive_branch(key, message_id, text).await,
            AnyStorage::Mem(s) => s.derive_branch(key, message_id, text).await,
        }
    }
    async fn switch_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
    ) -> Result<Vec<Message>, StorageError> {
        match self {
            AnyStorage::File(s) => s.switch_branch(key, message_id).await,
            AnyStorage::Mem(s) => s.switch_branch(key, message_id).await,
        }
    }
    async fn splice_compaction(
        &self,
        key: &SessionKey,
        summary: &Message,
        tail_start: MessageId,
    ) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => s.splice_compaction(key, summary, tail_start).await,
            AnyStorage::Mem(s) => s.splice_compaction(key, summary, tail_start).await,
        }
    }
    async fn set_goal(&self, key: &SessionKey, goal: &Goal) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => s.set_goal(key, goal).await,
            AnyStorage::Mem(s) => s.set_goal(key, goal).await,
        }
    }
    async fn set_title(&self, key: &SessionKey, title: Option<&str>) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => s.set_title(key, title).await,
            AnyStorage::Mem(s) => s.set_title(key, title).await,
        }
    }
    async fn archive(&self, key: &SessionKey) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => s.archive(key).await,
            AnyStorage::Mem(s) => s.archive(key).await,
        }
    }
    async fn activate(&self, key: &SessionKey) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => s.activate(key).await,
            AnyStorage::Mem(s) => s.activate(key).await,
        }
    }
    async fn remove_session(&self, key: &SessionKey) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => s.remove_session(key).await,
            AnyStorage::Mem(s) => s.remove_session(key).await,
        }
    }
    async fn list_sessions(&self) -> Result<Vec<SessionMeta>, StorageError> {
        match self {
            AnyStorage::File(s) => s.list_sessions().await,
            AnyStorage::Mem(s) => s.list_sessions().await,
        }
    }
    async fn set_last_activity(
        &self,
        key: &SessionKey,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => s.set_last_activity(key, at).await,
            AnyStorage::Mem(s) => s.set_last_activity(key, at).await,
        }
    }
}

#[async_trait]
impl MistakeStore for AnyStorage {
    async fn save(&self, mistake: &Mistake) -> Result<MistakeId, StorageError> {
        match self {
            AnyStorage::File(s) => s.save(mistake).await,
            AnyStorage::Mem(s) => s.save(mistake).await,
        }
    }
    async fn get(&self, id: &MistakeId) -> Result<Option<Mistake>, StorageError> {
        match self {
            AnyStorage::File(s) => s.get(id).await,
            AnyStorage::Mem(s) => s.get(id).await,
        }
    }
    async fn list(&self, filter: &MistakeFilter) -> Result<Vec<Mistake>, StorageError> {
        match self {
            // UFCS：消除与 DomainIo::list 的方法名冲突。
            AnyStorage::File(s) => MistakeStore::list(s, filter).await,
            AnyStorage::Mem(s) => MistakeStore::list(s, filter).await,
        }
    }
    async fn update(&self, id: &MistakeId, patch: &MistakePatch) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => s.update(id, patch).await,
            AnyStorage::Mem(s) => s.update(id, patch).await,
        }
    }
    async fn remove(&self, id: &MistakeId) -> Result<(), StorageError> {
        match self {
            // UFCS：消除与 DomainIo::remove 的方法名冲突。
            AnyStorage::File(s) => MistakeStore::remove(s, id).await,
            AnyStorage::Mem(s) => MistakeStore::remove(s, id).await,
        }
    }
    async fn remove_many(&self, ids: &[MistakeId]) -> Result<usize, StorageError> {
        match self {
            AnyStorage::File(s) => s.remove_many(ids).await,
            AnyStorage::Mem(s) => s.remove_many(ids).await,
        }
    }
}

impl AuditSink for AnyStorage {
    fn append(&self, record: AuditRecord) {
        match self {
            AnyStorage::File(s) => s.append(record),
            AnyStorage::Mem(s) => s.append(record),
        }
    }
}

#[async_trait]
impl DomainIo for AnyStorage {
    async fn read(&self, domain: Domain, rel: &RelPath) -> Result<Vec<u8>, StorageError> {
        match self {
            AnyStorage::File(s) => s.read(domain, rel).await,
            AnyStorage::Mem(s) => s.read(domain, rel).await,
        }
    }
    async fn write(&self, domain: Domain, rel: &RelPath, bytes: &[u8]) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => s.write(domain, rel, bytes).await,
            AnyStorage::Mem(s) => s.write(domain, rel, bytes).await,
        }
    }
    async fn remove(&self, domain: Domain, rel: &RelPath) -> Result<(), StorageError> {
        match self {
            // UFCS：消除与 MistakeStore::remove 的方法名冲突。
            AnyStorage::File(s) => DomainIo::remove(s, domain, rel).await,
            AnyStorage::Mem(s) => DomainIo::remove(s, domain, rel).await,
        }
    }
    async fn remove_tree(&self, domain: Domain, rel: &RelPath) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => DomainIo::remove_tree(s, domain, rel).await,
            AnyStorage::Mem(s) => DomainIo::remove_tree(s, domain, rel).await,
        }
    }
    async fn list(&self, domain: Domain) -> Result<Vec<String>, StorageError> {
        match self {
            // UFCS：消除与 MistakeStore::list 的方法名冲突。
            AnyStorage::File(s) => DomainIo::list(s, domain).await,
            AnyStorage::Mem(s) => DomainIo::list(s, domain).await,
        }
    }
    async fn read_legacy(&self, domain: Domain, legacy_rel: &str) -> Result<Vec<u8>, StorageError> {
        match self {
            AnyStorage::File(s) => DomainIo::read_legacy(s, domain, legacy_rel).await,
            AnyStorage::Mem(s) => DomainIo::read_legacy(s, domain, legacy_rel).await,
        }
    }
    async fn remove_legacy(&self, domain: Domain, legacy_rel: &str) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => DomainIo::remove_legacy(s, domain, legacy_rel).await,
            AnyStorage::Mem(s) => DomainIo::remove_legacy(s, domain, legacy_rel).await,
        }
    }
}

#[async_trait]
impl TmpIo for AnyStorage {
    async fn read_staged(&self, path: &str) -> Result<Vec<u8>, StorageError> {
        match self {
            AnyStorage::File(s) => s.read_staged(path).await,
            AnyStorage::Mem(s) => s.read_staged(path).await,
        }
    }
    async fn remove_staged(&self, path: &str) -> Result<(), StorageError> {
        match self {
            AnyStorage::File(s) => s.remove_staged(path).await,
            AnyStorage::Mem(s) => s.remove_staged(path).await,
        }
    }
}

impl StorageService for AnyStorage {}


pub use chain::{active_chain, active_session, last_message_id};

#[cfg(test)]
mod tests;
