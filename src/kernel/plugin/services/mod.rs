//! 内核服务契约与受控句柄（ADR-0001/0014/0016；Q5/Q6/Q8/Q9/Q10 定稿）。

mod compute;
mod memory;
mod model;
mod storage;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::kernel::audit::{AuditRecord, Auditor};
use crate::kernel::events::{Event, EventSink};
use crate::kernel::message::{Message, MessageId};

/// 服务标识：v2 封闭集合（ADR-0014）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceId {
    Storage,
    Memory,
    Compute,
    Model,
}

// ---------- 取消信号（SIGTERM 通道；SIGKILL 由 dispatch 任务 abort 承担） ----------

#[derive(Clone)]
pub struct AbortSignal {
    token: CancellationToken,
}

impl AbortSignal {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    pub fn from_token(token: CancellationToken) -> Self {
        Self { token }
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub fn cancelled(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        Self::new()
    }
}

pub use crate::mistake::{Mistake, MistakeFilter, MistakeId, MistakePatch, MistakeStore};
pub use compute::{ComputeError, ComputeHandle, ComputeRequest, ComputeResult, ComputeService};
pub use memory::{MemoryError, MemoryHandle, MemoryPath, MemoryService, MemoryView};
pub use model::{
    ItemKind, ModelChunk, ModelError, ModelHandle, ModelKind, ModelRequest, ModelResponse,
    ModelService, ModelStream, ResponseFormat, TokenUsage, ToolCallSpec, ToolChoice, ToolSchema,
};
pub use storage::{
    Domain, DomainIo, RelPath, SessionStore, StorageError, StorageHandle, StorageService, TmpIo,
};

// ---------- ServiceHandles：类型化封闭容器（Q5 修订） ----------

#[derive(Default, Clone)]
pub struct ServiceHandles {
    storage: Option<StorageHandle>,
    memory: Option<MemoryHandle>,
    compute: Option<ComputeHandle>,
    model: Option<ModelHandle>,
}

impl ServiceHandles {
    pub fn storage(&self) -> Option<&StorageHandle> {
        self.storage.as_ref()
    }
    pub fn memory(&self) -> Option<&MemoryHandle> {
        self.memory.as_ref()
    }
    pub fn compute(&self) -> Option<&ComputeHandle> {
        self.compute.as_ref()
    }
    pub fn model(&self) -> Option<&ModelHandle> {
        self.model.as_ref()
    }

    pub fn with_storage(mut self, h: StorageHandle) -> Self {
        self.storage = Some(h);
        self
    }
    pub fn with_memory(mut self, h: MemoryHandle) -> Self {
        self.memory = Some(h);
        self
    }
    pub fn with_compute(mut self, h: ComputeHandle) -> Self {
        self.compute = Some(h);
        self
    }
    pub fn with_model(mut self, h: ModelHandle) -> Self {
        self.model = Some(h);
        self
    }

    pub fn available(&self) -> HashSet<ServiceId> {
        let mut set = HashSet::new();
        if self.storage.is_some() {
            set.insert(ServiceId::Storage);
        }
        if self.memory.is_some() {
            set.insert(ServiceId::Memory);
        }
        if self.compute.is_some() {
            set.insert(ServiceId::Compute);
        }
        if self.model.is_some() {
            set.insert(ServiceId::Model);
        }
        set
    }

    /// 按能力声明过滤：插件只拿到声明过的服务（结构上受限）。
    pub fn filter(&self, requires: &[ServiceId]) -> ServiceHandles {
        let mut out = ServiceHandles::default();
        for id in requires {
            match id {
                ServiceId::Storage => out.storage = self.storage.clone(),
                ServiceId::Memory => out.memory = self.memory.clone(),
                ServiceId::Compute => out.compute = self.compute.clone(),
                ServiceId::Model => out.model = self.model.clone(),
            }
        }
        out
    }
}
