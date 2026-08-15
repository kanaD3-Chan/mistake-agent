//! kernel → GUI/使用方的事件流。

use serde::{Deserialize, Serialize};

use crate::agent::StopReason;
use crate::message::MessageId;
use crate::services::SessionKey;

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
    },
    TurnEnd {
        stop_reason: StopReason,
    },
    SessionSwitched {
        from: SessionKey,
        to: SessionKey,
    },
    MemoryChanged {
        path: String,
    },
    Error {
        message: String,
    },
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event);
}

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
