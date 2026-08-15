//! 守卫模型（Q17）：新消息预决策 / 回合末三动作决策。

use super::*;

// ---------- 守卫模型（Q17） ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardDecision {
    Continue,
    UpdateGoal(Goal),
    StartNew(Goal),
}

#[derive(Debug, Clone)]
pub struct GuardInput {
    pub goal: Option<Goal>,
    pub summary: String,
    pub new_text: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum GuardError {
    #[error("守卫模型调用失败：{0}")]
    Model(String),
    #[error("守卫输出无法解析：{0}")]
    Parse(String),
}

#[async_trait]
pub trait GuardModel: Send + Sync {
    async fn decide(&self, input: &GuardInput) -> Result<GuardDecision, GuardError>;
}

/// M1.5 确定性 stub 守卫：关键词命中开新会话，否则继续。
/// 生产实现 = 独立小模型调用（M2/M3），接口不变。
pub struct StubGuard {
    start_new_keywords: Vec<String>,
}

impl StubGuard {
    pub fn new() -> Self {
        Self {
            start_new_keywords: vec!["报告".into(), "周报".into(), "新会话".into()],
        }
    }
}

impl Default for StubGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GuardModel for StubGuard {
    async fn decide(&self, input: &GuardInput) -> Result<GuardDecision, GuardError> {
        if let Some(text) = &input.new_text
            && self
                .start_new_keywords
                .iter()
                .any(|k| text.contains(k.as_str()))
        {
            return Ok(GuardDecision::StartNew(Goal {
                text: text.chars().take(40).collect(),
            }));
        }
        Ok(GuardDecision::Continue)
    }
}

/// 回合结束决策器：由主模型在回合结束时判断 continue / update_goal / start_new。
/// 守卫模型退役（ADR-0030）：切换决策全部归主模型——回合内经 session::switch 工具主动发起，
/// 回合结束经本决策器判断；模型错误 / 输出无法解析 / 超时一律降级 Continue（存疑即继续）。
pub struct LlmTurnDecider {
    model: Arc<dyn ModelService>,
    settings: Option<Arc<std::sync::RwLock<crate::kernel::settings::Settings>>>,
    timeout: Duration,
    retries: usize,
    retry_delay: Duration,
    max_input_chars: usize,
}

impl LlmTurnDecider {
    pub fn new(model: Arc<dyn ModelService>) -> Self {
        Self {
            model,
            settings: None,
            timeout: Duration::from_secs(60),
            retries: 2,
            retry_delay: Duration::from_secs(2),
            max_input_chars: 12000,
        }
    }

    /// 自定义重试参数（测试/调优用）。
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
}

/// 带重试的模型 complete：对瞬时错误（503/限流/超时）退避重试；
/// 系统性错误（鉴权/余额/模型下架）与取消不重试。
pub(crate) async fn complete_with_retry(
    model: &Arc<dyn ModelService>,
    request: &ModelRequest,
    timeout: Duration,
    retries: usize,
    delay: Duration,
) -> Result<ModelResponse, ModelError> {
    let mut attempt = 0usize;
    loop {
        match tokio::time::timeout(timeout, model.complete(request, &AbortSignal::new())).await {
            Ok(Ok(resp)) => return Ok(resp),
            Ok(Err(e)) => {
                if e.is_systemic() || matches!(e, ModelError::Cancelled) {
                    return Err(e);
                }
                if attempt >= retries {
                    return Err(e);
                }
                attempt += 1;
                log::warn!("模型调用失败（{attempt}/{retries} 重试）：{e}");
                tokio::time::sleep(delay * attempt as u32).await;
            }
            Err(_) => {
                if attempt >= retries {
                    return Err(ModelError::Timeout);
                }
                attempt += 1;
                log::warn!("模型调用超时（{attempt}/{retries} 重试）");
                tokio::time::sleep(delay * attempt as u32).await;
            }
        }
    }
}

fn guard_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["continue", "update_goal", "start_new"]},
            "goal": {"type": "string"}
        },
        "required": ["action", "goal"],
        "additionalProperties": false
    })
}

/// 解析守卫输出：容忍 ```json 围栏与首尾空白；解析失败返回 None（上层降级 Continue）。
pub(crate) fn parse_guard_decision(text: &str) -> Option<GuardDecision> {
    let text = text.trim();
    let stripped = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(text);
    let v: Value = serde_json::from_str(stripped).ok()?;
    let action = v["action"].as_str()?;
    let goal = || Goal {
        text: v["goal"].as_str().unwrap_or_default().trim().to_string(),
    };
    match action {
        "continue" => Some(GuardDecision::Continue),
        "update_goal" => Some(GuardDecision::UpdateGoal(goal())),
        "start_new" => Some(GuardDecision::StartNew(goal())),
        _ => None,
    }
}

#[async_trait]
impl GuardModel for LlmTurnDecider {
    async fn decide(&self, input: &GuardInput) -> Result<GuardDecision, GuardError> {
        let mut transcript = input.summary.clone();
        if transcript.len() > self.max_input_chars {
            transcript = transcript.chars().take(self.max_input_chars).collect();
        }
        let payload = json!({
            "goal": input.goal.as_ref().map(|g| &g.text),
            "transcript": transcript,
            "new_text": input.new_text,
        });
        let request = ModelRequest {
            model: ModelKind::Main,
            messages: vec![
                Message::system(turn_decider_prompt(self.english_mode())),
                Message::user(serde_json::to_string(&payload).unwrap_or_default()),
            ],
            tools: None,
            reasoning_effort: Some("none".into()),
            tool_choice: None,
            response_format: Some(ResponseFormat::JsonSchema {
                name: "guard_decision".to_string(),
                schema: guard_schema(),
            }),
        };
        let text = match complete_with_retry(
            &self.model,
            &request,
            self.timeout,
            self.retries,
            self.retry_delay,
        )
        .await
        {
            Ok(resp) => resp.text,
            Err(e) => return Err(GuardError::Model(e.to_string())),
        };
        parse_guard_decision(&text)
            .ok_or_else(|| GuardError::Parse(text.chars().take(200).collect()))
    }
}
