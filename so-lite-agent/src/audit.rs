//! 通用审计：默认全覆盖，M2 骨架提供内存 sink；持久化 sink 由使用方实现。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::dispatch::Caller;
use crate::message::MessageId;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum AuditRecord {
    EntryPointCall {
        entry: String,
        caller: Caller,
        ok: bool,
        error: Option<String>,
    },
    LlmCall {
        provider: String,
        model: String,
        kind: String,
        tokens_in: Option<u64>,
        tokens_out: Option<u64>,
        duration_ms: u64,
        ok: bool,
    },
    MessageCompleted {
        message_id: MessageId,
    },
    Lifecycle {
        phase: String,
    },
    TurnEnded {
        stop_reason: String,
        tool_calls: usize,
    },
}

pub trait AuditSink: Send + Sync {
    fn append(&self, record: AuditRecord);
}

#[derive(Clone)]
pub struct Auditor {
    sink: Arc<dyn AuditSink>,
}

impl Auditor {
    pub fn new(sink: Arc<dyn AuditSink>) -> Self {
        Self { sink }
    }

    pub fn record(&self, record: AuditRecord) {
        self.sink.append(record);
    }
}

#[derive(Default)]
pub struct MemoryAuditSink {
    records: std::sync::Mutex<Vec<AuditRecord>>,
}

impl MemoryAuditSink {
    pub fn take(&self) -> Vec<AuditRecord> {
        std::mem::take(&mut *self.records.lock().expect("audit poisoned"))
    }
}

impl AuditSink for MemoryAuditSink {
    fn append(&self, record: AuditRecord) {
        self.records.lock().expect("audit poisoned").push(record);
    }
}
