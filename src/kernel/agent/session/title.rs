//! 会话标题生成（模型按首条对话概括，供侧栏会话列表展示）。
//!
//! 与摘要器同形：失败一律降级为截断文本，绝不因辅助调用失败影响主链路。
//! 调用点是回合结束（`Kernel::start_turn` 的收尾段），不在用户首答路径上。

use super::*;

use super::summarize::complete_with_retry;
use crate::kernel::message::{MessageKind, visible_text};
use crate::kernel::prompt::session_title_prompt;
use crate::kernel::settings::Settings;
use std::sync::RwLock;

/// 模型侧标题输出上限（提示词要求 ≤12 字，此处防跑偏）。
const MAX_TITLE_CHARS: usize = 40;

/// 转录里最多放几条「人说的话」（用户可见文本 / 助手正文）。
/// 标题只取决于开场在讲什么，多喂无益，反而稀释掉题目的信息。
const MAX_TITLE_MESSAGES: usize = 6;

/// 标题兜底：首条 user 消息（去空白）前 40 字；没有用户消息时用固定文案。
pub(crate) fn fallback_title(messages: &[Message]) -> String {
    messages
        .iter()
        .find_map(|m| match &m.kind {
            // 用可见文本：forced_tool 的 `text` 是给模型的指令，做标题只会得到一串工具名。
            MessageKind::User { .. } => {
                let t: String = visible_text(m)
                    .unwrap_or_default()
                    .trim()
                    .chars()
                    .take(MAX_TITLE_CHARS)
                    .collect();
                (!t.is_empty()).then_some(t)
            }
            _ => None,
        })
        .unwrap_or_else(|| "新会话".to_string())
}

/// 会话标题生成器。
#[async_trait]
pub trait Titler: Send + Sync {
    /// 生成标题；模型不可用或输出为空时返回空串（调用方用 [`fallback_title`] 兜底）。
    async fn title(&self, messages: &[Message]) -> String;
}

/// 降级标题器：不调模型，直接截断首条用户消息（测试与无模型场景用）。
pub struct StubTitler;

#[async_trait]
impl Titler for StubTitler {
    async fn title(&self, messages: &[Message]) -> String {
        fallback_title(messages)
    }
}

/// 生产标题器：与 `LlmSummarizer` 共用 `complete_with_retry` 与 settings（english_mode）。
pub struct LlmTitler {
    model: Arc<dyn ModelService>,
    settings: Option<Arc<RwLock<Settings>>>,
    timeout: Duration,
    max_input_chars: usize,
    retries: usize,
    retry_delay: Duration,
}

impl LlmTitler {
    pub fn new(model: Arc<dyn ModelService>) -> Self {
        Self {
            model,
            settings: None,
            timeout: Duration::from_secs(20),
            max_input_chars: 4000,
            retries: 1,
            retry_delay: Duration::from_secs(2),
        }
    }

    pub fn with_settings(mut self, settings: Arc<RwLock<Settings>>) -> Self {
        self.settings = Some(settings);
        self
    }

    /// 测试用：缩短超时/重试。
    pub fn with_retry(mut self, retries: usize, delay: Duration) -> Self {
        self.retries = retries;
        self.retry_delay = delay;
        self
    }

    fn english_mode(&self) -> bool {
        self.settings
            .as_ref()
            .map(|s| s.read().map(|x| x.english_mode).unwrap_or(false))
            .unwrap_or(false)
    }

    /// 清理模型输出：取首行、去引号与空白、截断。
    fn sanitize(raw: &str) -> String {
        let line = raw
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default();
        let unquoted = line.trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\'' | '“' | '”' | '‘' | '’' | '「' | '」' | '《' | '》' | '。' | '.'
            )
        });
        unquoted.trim().chars().take(MAX_TITLE_CHARS).collect()
    }
}

#[async_trait]
impl Titler for LlmTitler {
    async fn title(&self, messages: &[Message]) -> String {
        // 转录**只取「人说的话」**：用户可见文本 + 助手正文。
        // 工具调用的参数/结果是 JSON 管道（`工具：x 参数 {...} 结果 Some("{...}")`），
        // 推理与系统消息同理——喂进去模型只会抄出工具名和字段名，这正是侧栏标题变成
        // 「上传作业批改」的来源。题目的实质内容在助手讲解里，那里才是该归纳的东西。
        let mut transcript = String::new();
        let mut used = 0usize;
        for msg in messages {
            let line = match &msg.kind {
                MessageKind::User { .. } => {
                    let text = visible_text(msg).unwrap_or_default().trim();
                    (!text.is_empty()).then(|| format!("用户：{text}"))
                }
                MessageKind::Assistant { text } => {
                    let text = text.trim();
                    (!text.is_empty()).then(|| format!("助手：{text}"))
                }
                _ => None,
            };
            let Some(line) = line else { continue };
            if used >= MAX_TITLE_MESSAGES || transcript.len() + line.len() > self.max_input_chars {
                break;
            }
            transcript.push_str(&line);
            transcript.push('\n');
            used += 1;
        }
        // 一条人话都没有（纯工具回合）：没有可归纳的内容，交兜底标题，别拿空转录去问模型。
        if transcript.trim().is_empty() {
            return String::new();
        }
        let request = ModelRequest {
            // 辅助调用并入**当前**回合（ADR-0047 修订 R14）：标题/摘要不该另起一次扣次。
            turn_id: crate::kernel::plugin::model::turn::current(),
            messages: vec![
                Message::system(session_title_prompt(self.english_mode())),
                Message::user(transcript),
            ],
            tools: None,
            reasoning_effort: Some("none".into()),
            tool_choice: None,
            response_format: None,
        };
        match complete_with_retry(
            &self.model,
            &request,
            self.timeout,
            self.retries,
            self.retry_delay,
        )
        .await
        {
            Ok(resp) => Self::sanitize(&resp.text),
            Err(e) => {
                log::warn!("会话标题模型调用失败，降级截断标题：{e}");
                String::new()
            }
        }
    }
}
