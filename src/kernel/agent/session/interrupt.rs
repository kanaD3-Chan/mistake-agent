//! 环境变更中断（Q17d）：Interrupt / InterruptBus（回合边界消费）。

use super::*;

// ---------- 环境变更中断（Q17d：内部中断 / InterruptBus） ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "interrupt", rename_all = "snake_case")]
pub enum Interrupt {
    SessionSwitched {
        from: SessionKey,
        to: SessionKey,
        goal: Goal,
    },
    GoalUpdated {
        goal: Goal,
    },
    ConfigChanged,
    MemoryChanged {
        path: String,
    },
    CompactionDone {
        session: SessionKey,
    },
}

#[derive(Clone)]
pub struct InterruptBus {
    queue: Arc<Mutex<std::collections::VecDeque<Interrupt>>>,
}

impl InterruptBus {
    pub fn new() -> Self {
        Self {
            queue: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        }
    }

    pub fn send(&self, interrupt: Interrupt) {
        self.queue
            .lock()
            .expect("interrupt bus poisoned")
            .push_back(interrupt);
    }

    /// 消费全部待处理中断（agent loop 在回合边界调用）。
    pub fn take_all(&self) -> Vec<Interrupt> {
        std::mem::take(&mut *self.queue.lock().expect("interrupt bus poisoned"))
            .into_iter()
            .collect()
    }
}

impl Default for InterruptBus {
    fn default() -> Self {
        Self::new()
    }
}
