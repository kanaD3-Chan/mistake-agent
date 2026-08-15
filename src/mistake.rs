//! 错题领域模型（app 侧业务类型，不随 so-lite-agent 通用内核分发）。
//!
//! M1 解耦：`MistakeStore` 与错题数据结构从 `kernel::plugin::services`
//! 移到本模块；kernel 契约层经 `pub use` 兼容重导出，存储实现与业务插件
//! 以本模块为唯一事实源。提取 so-lite-agent 时，本模块留在使用方。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::kernel::plugin::services::StorageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MistakeId(pub Uuid);

impl std::fmt::Display for MistakeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mistake {
    pub id: MistakeId,
    pub subject: String,
    pub knowledge_point: String,
    pub question: String,
    pub student_answer: String,
    pub reference_answer: Option<String>,
    pub is_correct: bool,
    pub analysis: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MistakeFilter {
    pub subject: Option<String>,
    pub knowledge_point: Option<String>,
    pub is_correct: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MistakePatch {
    pub subject: Option<String>,
    pub knowledge_point: Option<String>,
    pub question: Option<String>,
    pub student_answer: Option<String>,
    pub reference_answer: Option<Option<String>>,
    pub analysis: Option<String>,
    pub is_correct: Option<bool>,
    pub pinned: Option<bool>,
}

/// 错题本：用户插件唯一可见的 storage 面。
#[async_trait]
pub trait MistakeStore: Send + Sync {
    async fn save(&self, mistake: &Mistake) -> Result<MistakeId, StorageError>;
    async fn get(&self, id: &MistakeId) -> Result<Option<Mistake>, StorageError>;
    async fn list(&self, filter: &MistakeFilter) -> Result<Vec<Mistake>, StorageError>;
    async fn update(&self, id: &MistakeId, patch: &MistakePatch) -> Result<(), StorageError>;
    async fn remove(&self, id: &MistakeId) -> Result<(), StorageError>;
    async fn remove_many(&self, ids: &[MistakeId]) -> Result<usize, StorageError> {
        let mut deleted = 0usize;
        for id in ids {
            match self.remove(id).await {
                Ok(()) => deleted += 1,
                Err(StorageError::MistakeNotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(deleted)
    }
}
