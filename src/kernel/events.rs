//! kernel → GUI 事件流（ADR-0013 事件清单）。

use serde::{Deserialize, Serialize};

use crate::kernel::agent::loop_mod::StopReason;
use crate::kernel::agent::session::SessionKey;
use crate::kernel::message::MessageId;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    MessageDelta {
        message_id: MessageId,
        delta: String,
    },
    ReasoningDelta {
        delta: String,
    },
    ToolStart {
        entry: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    ToolEnd {
        entry: String,
        ok: bool,
    },
    ToolProgress {
        entry: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    TurnEnd {
        stop_reason: StopReason,
    },
    /// 会话空闲超时（ADR-0044）：用户在该会话沉寂超过阈值后再次发言。
    /// 仅作提示——不再自动切换会话，是否开新话题由用户决定。
    SessionIdle {
        session: SessionKey,
        idle_seconds: i64,
    },
    MemoryChanged {
        path: String,
    },
    /// 会话标题已更新（模型首回合生成 / 用户重命名）：侧栏列表刷新用。
    SessionTitleUpdated {
        session: SessionKey,
        title: String,
    },
    Compaction {
        session: SessionKey,
    },
    /// 缓存命中统计更新：回合 usage 落盘后实时推送（载荷 = get_cache_stats 快照）。
    CacheStatsUpdated {
        stats: serde_json::Value,
    },
    /// 验算请求（kernel → GUI/Pyodide 执行端）：GUI 执行后回 Method::ComputeResult。
    ComputeRequest {
        id: u64,
        code: String,
    },
    Error {
        message: String,
    },
}

/// 事件消费者：非 async、fire-and-forget（M3 由 RPC 写线程实现背压）。
pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event);
}

/// 测试/控制台 sink：内存收集。
#[derive(Default)]
pub struct MemoryEventSink {
    events: std::sync::Mutex<Vec<Event>>,
}

impl MemoryEventSink {
    pub fn take(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().expect("sink poisoned"))
    }
}

impl EventSink for MemoryEventSink {
    fn emit(&self, event: Event) {
        self.events.lock().expect("sink poisoned").push(event);
    }
}
