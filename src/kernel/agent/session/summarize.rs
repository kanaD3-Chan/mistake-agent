//! 交接摘要（LlmSummarizer / 会话分叉摘要）。

use super::*;

// ---------- 交接摘要 ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffSummary {
    pub text: String,
}

#[async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(&self, messages: &[Message], goal: Option<&Goal>) -> String;
}

/// M1.5 stub 摘要：真实实现 = 模型生成（M2）。
pub struct StubSummarizer;

#[async_trait]
impl Summarizer for StubSummarizer {
    async fn summarize(&self, messages: &[Message], goal: Option<&Goal>) -> String {
        let goal_text = goal
            .map(|g| g.text.clone())
            .unwrap_or_else(|| "（未记录目标）".into());
        format!(
            "上一个会话共 {} 条消息，会话目标：{}。",
            messages.len(),
            goal_text
        )
    }
}

// ---------- LlmSummarizer（交接摘要实现，原属守卫段） ----------

pub(crate) fn message_text(msg: &Message) -> String {
    use crate::kernel::message::MessageKind;
    match &msg.kind {
        MessageKind::User { text, .. } => format!("用户：{text}"),
        MessageKind::Assistant { text } => format!("助手：{text}"),
        MessageKind::System { text, .. } => format!("系统：{text}"),
        MessageKind::Reasoning { text, .. } => format!("推理：{text}"),
        MessageKind::ToolCall {
            entry,
            params,
            result,
            ..
        } => format!(
            "工具：{entry} 参数 {params} 结果 {:?}",
            result.as_ref().map(|v| v.to_string())
        ),
    }
}

/// 生产摘要器：LLM 生成 ≤300 字任务摘要；模型失败降级为 stub 式摘要。
pub struct LlmSummarizer {
    model: Arc<dyn ModelService>,
    settings: Option<Arc<std::sync::RwLock<crate::kernel::settings::Settings>>>,
    timeout: Duration,
    max_input_chars: usize,
    /// 消息数少于该值时直接走 stub 摘要，不调 LLM（短会话无需生成式摘要）。
    min_messages_for_llm: usize,
    retries: usize,
    retry_delay: Duration,
}

impl LlmSummarizer {
    pub fn new(model: Arc<dyn ModelService>) -> Self {
        Self {
            model,
            settings: None,
            timeout: Duration::from_secs(60),
            max_input_chars: 12000,
            min_messages_for_llm: 8,
            retries: 2,
            retry_delay: Duration::from_secs(2),
        }
    }

    pub fn with_retry(mut self, retries: usize, delay: Duration) -> Self {
        self.retries = retries;
        self.retry_delay = delay;
        self
    }

    pub fn with_settings(
        mut self,
        settings: Arc<std::sync::RwLock<crate::kernel::settings::Settings>>,
    ) -> Self {
        self.settings = Some(settings);
        self
    }

    fn english_mode(&self) -> bool {
        self.settings
            .as_ref()
            .map(|s| s.read().map(|x| x.english_mode).unwrap_or(false))
            .unwrap_or(false)
    }

    fn fallback(messages: &[Message], goal: Option<&Goal>) -> String {
        let goal_text = goal
            .map(|g| g.text.clone())
            .unwrap_or_else(|| "（未记录目标）".into());
        format!(
            "上一个会话共 {} 条消息，会话目标：{}。",
            messages.len(),
            goal_text
        )
    }
}

#[async_trait]
impl Summarizer for LlmSummarizer {
    async fn summarize(&self, messages: &[Message], goal: Option<&Goal>) -> String {
        if messages.len() < self.min_messages_for_llm {
            return Self::fallback(messages, goal);
        }
        let goal_text = goal
            .map(|g| g.text.clone())
            .unwrap_or_else(|| "（未记录目标）".into());
        let mut transcript = String::new();
        for msg in messages {
            let line = message_text(msg);
            if transcript.len() + line.len() > self.max_input_chars {
                transcript.push_str("…（已截断）");
                break;
            }
            transcript.push_str(&line);
            transcript.push('\n');
        }
        let request = ModelRequest {
            model: ModelKind::Main,
            messages: vec![
                Message::system(summarize_prompt(self.english_mode())),
                Message::user(format!("目标：{goal_text}\n\n对话：\n{transcript}")),
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
            Ok(resp) => {
                let summary = resp.text.trim();
                if summary.is_empty() {
                    Self::fallback(messages, goal)
                } else {
                    summary.chars().take(300).collect()
                }
            }
            Err(e) => {
                log::warn!("摘要模型重试后仍失败，降级 stub 摘要：{e}");
                Self::fallback(messages, goal)
            }
        }
    }
}
