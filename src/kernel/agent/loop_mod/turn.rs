//! 回合输入/输出与停止条件类型。

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::kernel::agent::session::SessionKey;
use crate::kernel::message::{Message, MessageId};
use crate::kernel::plugin::services::{AbortSignal, TokenUsage, ToolSchema};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptReason {
    ModelUnavailable,
    ConfigChanged,
    AuditFailure,
    PluginRequested(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Natural,
    ToolCallLimit,
    ConsecutiveFailures,
    TurnTimeout,
    UserAborted,
    /// 回合失败（模型/协议/内部错误），前端应恢复可聊天状态。
    Failed,
    InternalAbort {
        reason: InterruptReason,
    },
}

pub struct TurnInput {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub signal: AbortSignal,
    pub turn_budget: Duration,
    /// 强制首轮调用的工具（wire name）；执行后后续轮次恢复 auto。
    pub forced_tool: Option<String>,
}

#[derive(Debug)]
pub struct TurnOutcome {
    pub messages: Vec<Message>,
    pub stop_reason: StopReason,
    pub tool_calls: usize,
    /// 本回合发生的上下文压缩（None = 未压缩）。
    pub compaction: Option<CompactionInfo>,
    /// 本回合所有主模型流调用的累计 token 用量（缓存命中统计用）。
    pub usage: Option<TokenUsage>,
    /// 回合内经 session::switch 切换后的新会话（None = 未切换，仍用原会话）。
    pub session_key: Option<SessionKey>,
}

#[derive(Debug, Clone)]
pub struct CompactionInfo {
    /// 已写入 messages 的摘要消息（RPC 需要跳过重复落盘并接入存储链）。
    pub summary: Message,
    /// 保留段首条消息 id（其 parent 应改挂到 summary 下）。
    pub tail_start: MessageId,
    /// 压缩掉的旧消息条数。
    pub summarized: usize,
    /// 压缩时会话末尾消息 id（活跃路径推进目标）。
    pub tail_end: MessageId,
}

#[derive(Debug, thiserror::Error)]
pub enum LoopError {
    #[error("模型错误：{0}")]
    Model(String),
    #[error("内部错误：{0}")]
    Internal(String),
}
