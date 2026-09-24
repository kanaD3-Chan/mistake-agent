//! RPC 协议类型（ADR-0013 / Q15）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::kernel::agent::session::SessionKey;
use crate::kernel::events::Event;
use crate::kernel::message::MessageId;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Method {
    /// 通用子集：新 Agent 直接可用，不依赖使用方业务。
    SendUserMessage {
        text: String,
        /// 前端展示文本（与 `text` 不同时使用，如 PDF 正文并入 `text` 后的干净展示）。
        #[serde(default)]
        display_text: Option<String>,
        /// 显式工具调用：强制 LLM 首轮调用指定工具（不绕过 LLM）。
        #[serde(default)]
        force_tool: Option<ForcedToolRequest>,
        /// 兼容字段（已弃用，ADR-0046 后附件走 `asset` 引用直入上下文）。
        #[serde(default)]
        file: Vec<String>,
        /// 持久附件列表（数据根目录 uploads/ 副本）：作为图片引用存入消息并直入模型上下文。
        #[serde(default)]
        asset: Vec<AttachmentInfo>,
    },
    TriggerCommand {
        entry: String,
        params: Value,
    },
    EditMessage {
        message_id: MessageId,
        text: String,
    },
    SwitchBranch {
        message_id: MessageId,
    },
    Abort,
    GetState,
    /// 会话列表（GUI 会话历史页）。
    ListSessions,
    /// 读取指定会话完整消息树（GUI 历史浏览/分支回放）。
    ReadSession {
        key: SessionKey,
    },
    /// 用户可调工具/命令清单（GUI 工具面板）。
    ListTools,
    /// 教学规则（数据根 AGENTS.md）加载状态：前端展示「规则已加载/回退静态提示」。
    GetRulesStatus,
    /// 用户手动新建会话（GUI「新对话」，ADR-0044）：归档当前活动会话并开启全新会话。
    CreateSession {
        /// 是否携带交接摘要：true = 用旧会话生成摘要，作为新会话首条系统消息。
        #[serde(default)]
        carry_summary: bool,
        /// 新会话目标（可选；缺省时若携带摘要则取摘要前 40 字）。
        #[serde(default)]
        goal: Option<String>,
    },
    /// 用户点击会话列表切换到既有会话：归档全部 Active 后激活目标（ADR-0044）。
    OpenSession {
        key: SessionKey,
    },
    /// 会话重命名：`title` 为空串 = 清空（下个回合末可由模型重新生成）。
    RenameSession {
        key: SessionKey,
        title: String,
    },
    /// 删除会话（不可恢复）；若删除的是当前 Active，则由调度层补建一条空会话。
    DeleteSession {
        key: SessionKey,
    },
}

/// 自定义方法兜底：未知 method 名连同 params 与其余字段一起交给 `RpcExtension`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomMethod {
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

/// RPC 请求承载：通用子集优先匹配，其余走自定义兜底（保持既有 wire 兼容）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireMethod {
    Generic(Method),
    Custom(CustomMethod),
}

impl From<Method> for WireMethod {
    fn from(method: Method) -> Self {
        Self::Generic(method)
    }
}

impl WireMethod {
    pub fn custom(method: impl Into<String>, params: serde_json::Value) -> Self {
        Self::Custom(CustomMethod {
            method: method.into(),
            params,
            extra: std::collections::BTreeMap::new(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: u64,
    #[serde(flatten)]
    pub method: WireMethod,
}

impl RpcRequest {
    pub fn custom(id: u64, method: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            id,
            method: WireMethod::custom(method, params),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl RpcError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcFrame {
    Response {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<RpcError>,
    },
    Event {
        event: Event,
    },
}

/// 显式工具调用请求：entry 为内部全名（namespace::tool），hint 为用户输入的可选参数文本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForcedToolRequest {
    pub entry: String,
    #[serde(default)]
    pub hint: Option<String>,
    /// 前端原始展示文本（如「翻看记忆：数学/向量组…」）；缺省时 kernel 按 title＋hint 兜底，
    /// 落盘到 user 消息的 display_text，重开会话后渲染仍友好。
    #[serde(default)]
    pub display: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentInfo {
    pub path: String,
    pub name: String,
}

/// 自定义 RPC 扩展：通用子集之外的方法由使用方注册，返回 `Ok(None)` 表示不处理。
#[async_trait::async_trait]
pub trait RpcExtension: Send + Sync {
    async fn handle(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, RpcError>;
}

/// 把自定义请求的 `params` 与平铺多余字段合并，兼容新旧两种 wire 形状。
pub(crate) fn custom_params(c: &CustomMethod) -> serde_json::Value {
    if c.extra.is_empty() {
        return c.params.clone();
    }
    let mut map = match &c.params {
        serde_json::Value::Object(obj) => obj.clone(),
        serde_json::Value::Null => serde_json::Map::new(),
        other => {
            let mut obj = serde_json::Map::new();
            obj.insert("params".into(), other.clone());
            obj
        }
    };
    for (k, v) in &c.extra {
        map.insert(k.clone(), v.clone());
    }
    serde_json::Value::Object(map)
}
