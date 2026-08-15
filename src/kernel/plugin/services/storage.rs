//! Storage 契约（Q8 + ADR-0042 域内文件 IO）。

use super::*;

// ---------- Storage 契约（Q8：角色拆分，插件只见 MistakeStore） ----------

use crate::kernel::agent::session::{Goal, SessionKey, SessionMeta};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("会话不存在：{0}")]
    SessionNotFound(SessionKey),
    #[error("错题不存在：{0}")]
    MistakeNotFound(String),
    #[error("已存在：{0}")]
    AlreadyExists(String),
    #[error("数据损坏：{0}")]
    Corrupt(String),
    #[error("IO 错误：{0}")]
    Io(String),
    #[error("路径非法：{0}")]
    InvalidPath(String),
    #[error("内部错误：{0}")]
    Internal(String),
}

// ---------- 域内文件 IO（ADR-0042 磁盘 IO 铁律） ----------

/// 数据根目录下的域（storage 拥有的子目录）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Mistakes,
    Sessions,
    Memory,
    Data,
    Uploads,
}

impl Domain {
    pub fn as_dir(self) -> &'static str {
        match self {
            Domain::Mistakes => "mistakes",
            Domain::Sessions => "sessions",
            Domain::Memory => "memory",
            Domain::Data => "data",
            Domain::Uploads => "uploads",
        }
    }
}

/// 相对路径：构造即校验（ADR-0042）。
///
/// 白名单字符校验，不做任何路径语义解析——段必须以 `[a-zA-Z0-9]` 开头和结尾，
/// 中间只允许 `[a-zA-Z0-9._-]`；空段、`.`、`..`、尾点、首点全部拒绝。
/// 构造后类型上不可能表示目录遍历（fail-closed，无规范化/替换通道）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelPath {
    segments: Vec<String>,
}

impl RelPath {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        if raw.is_empty() {
            return Err(StorageError::InvalidPath("路径为空".into()));
        }
        let mut segments = Vec::new();
        for seg in raw.split('/') {
            let ok = !seg.is_empty()
                && seg
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
                && seg
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
            if !ok {
                return Err(StorageError::InvalidPath(format!(
                    "非法路径段：{seg}（仅允许字母数字开头结尾，中间 [a-zA-Z0-9._-]）"
                )));
            }
            segments.push(seg.to_string());
        }
        Ok(Self { segments })
    }

    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    pub fn as_str(&self) -> String {
        self.segments.join("/")
    }
}

/// 数据根目录域内文件能力（ADR-0042）：内核插件的唯一磁盘通道。
///
/// 实现（storage）内部负责：域根拼接 + canonicalize 兜底（防符号链接逃逸）+ 原子写 + 审计。
/// 用户插件永远不持有本 trait——它们只见 `StorageHandle` 的语义方法。
#[async_trait]
pub trait DomainIo: Send + Sync {
    async fn read(&self, domain: Domain, rel: &RelPath) -> Result<Vec<u8>, StorageError>;
    async fn write(&self, domain: Domain, rel: &RelPath, bytes: &[u8]) -> Result<(), StorageError>;
    async fn remove(&self, domain: Domain, rel: &RelPath) -> Result<(), StorageError>;
    /// 递归删除子树（memory 的 remove 语义）。
    async fn remove_tree(&self, domain: Domain, rel: &RelPath) -> Result<(), StorageError>;
    /// 列出域内全部条目（递归，返回 `/` 分隔的相对路径）。
    async fn list(&self, domain: Domain) -> Result<Vec<String>, StorageError>;

    /// 读取域内**历史**文件（路径可能含非 ASCII 段，过不了 RelPath 白名单）。
    /// 仅启动引导迁移调用（ADR-0042 存储布局迁移），新代码一律走 RelPath；
    /// 实现侧做宽松校验（拒绝 `..`/`\`/绝对路径/空段）+ canonicalize 兜底 + 审计。
    async fn read_legacy(&self, domain: Domain, legacy_rel: &str) -> Result<Vec<u8>, StorageError>;

    /// 删除域内**历史**文件（同 read_legacy 的约束，仅迁移用）。
    async fn remove_legacy(&self, domain: Domain, legacy_rel: &str) -> Result<(), StorageError>;
}

/// 系统 temp 暂存文件能力（ADR-0042）：附件暂存（`mistake-agent-` 前缀白名单）。
///
/// 与 DomainIo 解耦：硬编码 `std::env::temp_dir()`，只管 temp 里自己前缀的文件，
/// 不做目录管理，只做受限读写；读删都记审计。
#[async_trait]
pub trait TmpIo: Send + Sync {
    async fn read_staged(&self, path: &str) -> Result<Vec<u8>, StorageError>;
    async fn remove_staged(&self, path: &str) -> Result<(), StorageError>;
}

/// 会话持久化：只给 kernel 内部（Session scheduler / loop / 压缩）。
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_session(
        &self,
        key: &SessionKey,
        meta: &SessionMeta,
    ) -> Result<(), StorageError>;
    async fn get_session(&self, key: &SessionKey) -> Result<Option<SessionMeta>, StorageError>;
    async fn append_message(&self, key: &SessionKey, msg: &Message) -> Result<(), StorageError>;
    async fn read_path(&self, key: &SessionKey) -> Result<Vec<Message>, StorageError>;
    async fn read_all(&self, key: &SessionKey) -> Result<Vec<Message>, StorageError>;
    /// 设置活跃路径末端（消息树分支切换；None = 退化为线性全链）。
    async fn set_active_path(
        &self,
        key: &SessionKey,
        message_id: Option<MessageId>,
    ) -> Result<(), StorageError>;
    /// 在 message_id 处派生新分支：消息复制新 id（parent 不变、文本替换），
    /// 编辑点之后的旧消息保留在 JSONL 但不再属于活跃路径（ADR-0007 历史不截断）。
    /// 返回新活跃路径。
    async fn derive_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
        text: &str,
    ) -> Result<Vec<Message>, StorageError>;
    /// 切换到以 message_id 为末端的活跃路径（沿 parent 链回溯）。
    async fn switch_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
    ) -> Result<Vec<Message>, StorageError>;
    /// 压缩接入：把摘要消息追加进会话，并把 tail_start（保留段首条）的 parent 改挂到摘要下，
    /// 使活跃路径变为 `摘要 → 保留段 → …`，旧前缀仍在 JSONL 但不进上下文。
    async fn splice_compaction(
        &self,
        key: &SessionKey,
        summary: &Message,
        tail_start: MessageId,
    ) -> Result<(), StorageError>;
    async fn set_goal(&self, key: &SessionKey, goal: &Goal) -> Result<(), StorageError>;
    async fn archive(&self, key: &SessionKey) -> Result<(), StorageError>;
    async fn list_sessions(&self) -> Result<Vec<SessionMeta>, StorageError>;
    async fn set_last_activity(
        &self,
        key: &SessionKey,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), StorageError>;
}

// 错题领域契约已移到 crate::mistake（app 侧）；services 层经 mod.rs 兼容重导出。

/// Storage 服务组合接口：kernel 持有全量，插件拿 StorageHandle 视图。
pub trait StorageService: SessionStore + MistakeStore + crate::kernel::audit::AuditSink {}

/// 注入插件的 storage 受控句柄：只有错题本 + 语义化文件 IO（附件暂存 / 数据文件）。
#[derive(Clone)]
pub struct StorageHandle {
    inner: Arc<dyn MistakeStore>,
    tmp: Option<Arc<dyn TmpIo>>,
    domain: Option<Arc<dyn DomainIo>>,
}

impl StorageHandle {
    pub fn new(inner: Arc<dyn MistakeStore>) -> Self {
        Self {
            inner,
            tmp: None,
            domain: None,
        }
    }

    /// 注入 IO 能力（Kernel::new 装配时调用；测试/回退时可缺省）。
    pub fn with_io(mut self, tmp: Arc<dyn TmpIo>, domain: Arc<dyn DomainIo>) -> Self {
        self.tmp = Some(tmp);
        self.domain = Some(domain);
        self
    }

    /// 附件暂存读取（TmpIo 语义方法，白名单在实现内）。
    pub async fn read_staged(&self, path: &str) -> Result<Vec<u8>, StorageError> {
        let tmp = self
            .tmp
            .as_ref()
            .ok_or_else(|| StorageError::Internal("storage 未注入 TmpIo 能力".into()))?;
        tmp.read_staged(path).await
    }

    /// 附件暂存删除（TmpIo 语义方法）。
    pub async fn remove_staged(&self, path: &str) -> Result<(), StorageError> {
        let tmp = self
            .tmp
            .as_ref()
            .ok_or_else(|| StorageError::Internal("storage 未注入 TmpIo 能力".into()))?;
        tmp.remove_staged(path).await
    }

    /// 教学数据文件读取（DomainIo data 域）。
    pub async fn read_data_file(&self, name: &str) -> Result<String, StorageError> {
        let domain = self
            .domain
            .as_ref()
            .ok_or_else(|| StorageError::Internal("storage 未注入 DomainIo 能力".into()))?;
        let rel = RelPath::parse(name)?;
        let bytes = domain.read(Domain::Data, &rel).await?;
        String::from_utf8(bytes)
            .map_err(|e| StorageError::Corrupt(format!("数据文件非 UTF-8：{e}")))
    }

    /// 教学数据文件写入（DomainIo data 域，原子写）。
    pub async fn write_data_file(&self, name: &str, content: &str) -> Result<(), StorageError> {
        let domain = self
            .domain
            .as_ref()
            .ok_or_else(|| StorageError::Internal("storage 未注入 DomainIo 能力".into()))?;
        let rel = RelPath::parse(name)?;
        domain.write(Domain::Data, &rel, content.as_bytes()).await
    }

    pub async fn save(&self, m: &Mistake) -> Result<MistakeId, StorageError> {
        self.inner.save(m).await
    }
    pub async fn get(&self, id: &MistakeId) -> Result<Option<Mistake>, StorageError> {
        self.inner.get(id).await
    }
    pub async fn list(&self, f: &MistakeFilter) -> Result<Vec<Mistake>, StorageError> {
        self.inner.list(f).await
    }
    pub async fn update(&self, id: &MistakeId, p: &MistakePatch) -> Result<(), StorageError> {
        self.inner.update(id, p).await
    }
    pub async fn remove(&self, id: &MistakeId) -> Result<(), StorageError> {
        self.inner.remove(id).await
    }
    pub async fn remove_many(&self, ids: &[MistakeId]) -> Result<usize, StorageError> {
        self.inner.remove_many(ids).await
    }
}
