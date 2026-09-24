//! 消息树（ADR-0007 修订）：气泡 = 一个输出 item，完成即落盘。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::kernel::contract::ToolError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageId(pub Uuid);

impl MessageId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for MessageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// 运行时附件（图片 base64；构建模型请求前由 `AttachmentResolvingModelService` 填充，不落盘）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub mime: String,
    pub data_base64: String,
}

/// 用户附件的持久引用（ADR-0046）：只存 uploads/ 域内文件名，消息树不落图片字节；
/// 构建模型请求时按引用读盘还原为 `Attachment`（图片直入 Responses `input_image`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentRef {
    /// uploads/ 域内相对文件名（RelPath 白名单内）。
    pub name: String,
    pub mime: String,
    /// 原始文件名（前端展示用）。
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageKind {
    User {
        text: String,
        /// 前端展示文本（force_tool 场景：原始输入 / 工具标题+参数），重开会话后仍友好；
        /// 缺省时前端回退渲染 `text`。模型上下文始终使用 `text`（拼好的指令）。
        #[serde(default)]
        display_text: Option<String>,
        /// 图片附件引用（路径持久化，ADR-0046）。
        #[serde(default)]
        attachment_refs: Vec<AttachmentRef>,
        /// 运行时解析出的图片字节：仅内存，不落盘（由模型服务包装层填充）。
        #[serde(skip, default)]
        attachments: Vec<Attachment>,
    },
    Assistant {
        text: String,
    },
    ToolCall {
        entry: String,
        params: serde_json::Value,
        result: Result<serde_json::Value, ToolError>,
        /// 第一轮模型返回的真实 call_id（Responses API 要求按原值回传；
        /// 旧数据/手动构造缺省为空，回传时回退消息 id）。
        #[serde(default)]
        call_id: String,
    },
    /// 模型推理（思维链）：id 用于 Responses API 后续轮次回传，text 供前端展示。
    Reasoning {
        id: String,
        text: String,
    },
    System {
        text: String,
        /// 前端展示文本（如会话切换提示）：模型上下文始终使用 `text`；
        /// 缺省时前端回退渲染 `text`。与 User.display_text 同一机制。
        #[serde(default)]
        display_text: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub parent_id: Option<MessageId>,
    pub kind: MessageKind,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self::user_with_display(text, None)
    }

    pub fn user_with_display(text: impl Into<String>, display_text: Option<String>) -> Self {
        Self::user_with_attachments(text, display_text, Vec::new())
    }

    /// 带图片附件引用的用户消息（ADR-0046：图片随消息进入上下文）。
    pub fn user_with_attachments(
        text: impl Into<String>,
        display_text: Option<String>,
        attachment_refs: Vec<AttachmentRef>,
    ) -> Self {
        Self {
            id: MessageId::new(),
            parent_id: None,
            kind: MessageKind::User {
                text: text.into(),
                display_text,
                attachment_refs,
                attachments: Vec::new(),
            },
            created_at: chrono::Utc::now(),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            parent_id: None,
            kind: MessageKind::Assistant { text: text.into() },
            created_at: chrono::Utc::now(),
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            parent_id: None,
            kind: MessageKind::System {
                text: text.into(),
                display_text: None,
            },
            created_at: chrono::Utc::now(),
        }
    }

    /// 带前端展示文本的 System 消息：模型看到完整 `text`，学生只看到 `display_text`。
    pub fn system_with_display(text: impl Into<String>, display_text: Option<String>) -> Self {
        Self {
            id: MessageId::new(),
            parent_id: None,
            kind: MessageKind::System {
                text: text.into(),
                display_text,
            },
            created_at: chrono::Utc::now(),
        }
    }

    pub fn tool_call(
        entry: impl Into<String>,
        params: serde_json::Value,
        result: Result<serde_json::Value, ToolError>,
    ) -> Self {
        Self::tool_call_with_id(entry, params, result, String::new())
    }

    /// 带真实 call_id 的工具调用消息（agent loop 使用）。
    pub fn tool_call_with_id(
        entry: impl Into<String>,
        params: serde_json::Value,
        result: Result<serde_json::Value, ToolError>,
        call_id: String,
    ) -> Self {
        Self {
            id: MessageId::new(),
            parent_id: None,
            kind: MessageKind::ToolCall {
                entry: entry.into(),
                params,
                result,
                call_id,
            },
            created_at: chrono::Utc::now(),
        }
    }
}

/// 把消息挂到链尾：parent = 最后一条消息 id。
pub fn append_to_path(messages: &mut Vec<Message>, mut msg: Message) {
    msg.parent_id = messages.last().map(|m| m.id);
    messages.push(msg);
}

/// 用户可见文本：`display_text`（前端展示文案）非空时优先，否则回退 `text`。
///
/// forced_tool 场景下 `text` 是发给模型的指令（"请调用工具 X 处理当前请求。"），
/// `display_text` 才是学生看到的那句话——派生会话标题一类**给人看**的文本时要用后者，
/// 否则标题会变成一串工具指令。非 User/System 消息无此机制，返回 `None`。
pub fn visible_text(msg: &Message) -> Option<&str> {
    let (text, display): (&str, Option<&String>) = match &msg.kind {
        MessageKind::User {
            text, display_text, ..
        }
        | MessageKind::System { text, display_text } => (text, display_text.as_ref()),
        _ => return None,
    };
    display
        .map(String::as_str)
        .filter(|d| !d.trim().is_empty())
        .or(Some(text))
}
