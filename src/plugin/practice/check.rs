//! practice 插件：练习答案即时批改（场景二即时反馈）。
//!
//! 先按参考答案确定性对拍（填空/选择等封闭题型直接判分，不调模型）；
//! 对不上再走主模型判分（仿 grading 判分的 json_schema 强约束）；
//! 答错自动回写错题本（StorageHandle.save），为防重复刷题积累数据。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::kernel::contract::ToolError;
use crate::kernel::message::Message;
use crate::kernel::plugin::services::{
    AbortSignal, Mistake, MistakeId, ModelHandle, ModelKind, ModelRequest, ResponseFormat,
    MemoryHandle, StorageHandle,
};
use crate::kernel::prompt::practice_check_system_prompt;
use crate::plugin::vision::map_model_error;

use super::history::record_attempt;
use super::templates::Difficulty;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct CheckParams {
    /// 题目文本（必填）。
    pub question: String,
    /// 学生作答（必填）。
    pub student_answer: String,
    /// 参考答案（可选；可对拍时直接判分，否则由模型判分）。
    pub reference_answer: Option<String>,
    /// 学科（可选，答错回写错题本用；缺省"未分类"）。
    pub subject: Option<String>,
    /// 知识点（可选，答错回写错题本用；缺省"未标注"）。
    pub knowledge_point: Option<String>,
    /// 题型提示（可选：填空/选择/解答/计算等，帮助模型判分）。
    pub kind: Option<String>,
    /// 题目标识（可选；practice::generate 返回的 item.template_id，用于防重复记录；
    /// 缺省用题目文本哈希兜底）。
    pub item_id: Option<String>,
    /// 难度（可选，练习历史记录用；缺省 basic）。
    pub difficulty: Option<Difficulty>,
}

/// 模型判分结果：严格 JSON 对象。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CheckResult {
    pub correct: bool,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub total: Option<f64>,
    pub analysis: String,
}

/// 归一化：去首尾空白、统一小写、折叠连续空白。
fn normalize_answer(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// 确定性对拍：仅适用于填空/选择等封闭题型；数学等价（1/2 与 0.5）等交给模型判分。
fn exact_match(student: &str, reference: &str) -> bool {
    normalize_answer(student) == normalize_answer(reference)
}

/// 批改一道练习：对拍优先、模型兜底；答错回写错题本。
pub async fn check_handler(
    model: ModelHandle,
    storage: StorageHandle,
    memory: MemoryHandle,
    english_mode: bool,
    params: Value,
) -> Result<Value, ToolError> {
    let p: CheckParams =
        serde_json::from_value(params).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    if p.question.trim().is_empty() || p.student_answer.trim().is_empty() {
        return Err(ToolError::invalid_params(
            "question 与 student_answer 不能为空",
        ));
    }

    let reference = p
        .reference_answer
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // 第一步：参考答案确定性对拍（封闭题型直接判分，不消耗模型调用）。
    if let Some(ref_text) = reference
        && exact_match(&p.student_answer, ref_text)
    {
        let analysis = if english_mode {
            "The answer matches the reference answer and is correct."
        } else {
            "参考答案对拍一致，作答正确。"
        };
        return Ok(check_output(true, "exact_match", None, None, analysis, false));
    }

    // 第二步：模型判分（自由作答 / 数学等价等对拍覆盖不了的形态）。
    let result = model_check(&model, &p, english_mode).await?;
    let archived = if !result.correct {
        let mistake = Mistake {
            id: MistakeId(uuid::Uuid::new_v4()),
            subject: p
                .subject
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "未分类".into()),
            knowledge_point: p
                .knowledge_point
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "未标注".into()),
            question: p.question.clone(),
            student_answer: p.student_answer.clone(),
            reference_answer: reference.map(str::to_string),
            is_correct: false,
            analysis: result.analysis.clone(),
            created_at: chrono::Utc::now(),
            pinned: false,
            deleted_at: None,
        };
        match storage.save(&mistake).await {
            Ok(_) => true,
            Err(e) => return Err(ToolError::handler(format!("错题归档失败：{e}"))),
        }
    } else {
        false
    };

    // 无论对错都记录练习历史（防重复出题的数据源）；无 item_id 时用题目哈希兜底。
    let item_id = p.item_id.clone().unwrap_or_else(|| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        p.question.hash(&mut h);
        format!("custom:{:016x}", h.finish())
    });
    let knowledge_point = p
        .knowledge_point
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "未标注".into());
    let difficulty = p.difficulty.unwrap_or_default();
    record_attempt(&memory, &item_id, &knowledge_point, difficulty, result.correct).await;

    Ok(check_output(
        result.correct,
        "model",
        result.score,
        result.total,
        &result.analysis,
        archived,
    ))
}

fn check_output(
    correct: bool,
    method: &str,
    score: Option<f64>,
    total: Option<f64>,
    analysis: &str,
    archived: bool,
) -> Value {
    json!({
        "correct": correct,
        "method": method,
        "score": score,
        "total": total,
        "analysis": analysis,
        "archived_mistake": archived,
    })
}

async fn model_check(
    model: &ModelHandle,
    p: &CheckParams,
    english_mode: bool,
) -> Result<CheckResult, ToolError> {
    let system = Message::system(practice_check_system_prompt(english_mode));
    let mut lines = vec![format!("题目：{}", p.question.trim())];
    lines.push(format!("学生作答：{}", p.student_answer.trim()));
    if let Some(r) = p.reference_answer.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        lines.push(format!("参考答案：{r}"));
    }
    if let Some(k) = p.kind.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        lines.push(format!("题型：{k}"));
    }
    let user = Message::user(lines.join("\n"));
    let mut request = ModelRequest::chat(ModelKind::Main, vec![system, user]);
    request.response_format = Some(ResponseFormat::JsonSchema {
        name: "practice_check".into(),
        schema: serde_json::to_value(schemars::schema_for!(CheckResult)).unwrap_or_default(),
    });
    request.reasoning_effort = Some("none".into());
    let response = model
        .complete(&request, &AbortSignal::new())
        .await
        .map_err(map_model_error)?;
    parse_check_json(&response.text)
}

fn parse_check_json(text: &str) -> Result<CheckResult, ToolError> {
    let trimmed = text.trim();
    if let Ok(r) = serde_json::from_str::<CheckResult>(trimmed) {
        return Ok(r);
    }
    // 容灾：截取第一个 { 到最后一个 }。
    if let (Some(s), Some(e)) = (trimmed.find('{'), trimmed.rfind('}')) {
        let slice = &trimmed[s..=e];
        if let Ok(r) = serde_json::from_str::<CheckResult>(slice) {
            return Ok(r);
        }
    }
    Err(ToolError::handler(format!(
        "判分结果无法解析：{}",
        text.chars().take(200).collect::<String>()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::audit::{Auditor, MemoryAuditSink};
    use crate::kernel::contract::ToolErrorCode;
    use crate::kernel::plugin::services::{
        Mistake, MistakeFilter, MistakeId, MistakePatch, MistakeStore, ModelError, ModelRequest,
        ModelResponse, ModelService, ModelStream, StorageError,
    };
    use crate::plugin::practice::history::tests::FakeMemory;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeStore {
        items: Mutex<Vec<Mistake>>,
    }

    #[async_trait::async_trait]
    impl MistakeStore for FakeStore {
        async fn save(&self, m: &Mistake) -> Result<MistakeId, StorageError> {
            self.items.lock().expect("poisoned").push(m.clone());
            Ok(m.id)
        }
        async fn get(&self, id: &MistakeId) -> Result<Option<Mistake>, StorageError> {
            Ok(self
                .items
                .lock()
                .expect("poisoned")
                .iter()
                .find(|m| m.id == *id)
                .cloned())
        }
        async fn list(&self, f: &MistakeFilter) -> Result<Vec<Mistake>, StorageError> {
            let inner = self.items.lock().expect("poisoned");
            Ok(inner
                .iter()
                .filter(|m| {
                    f.subject
                        .as_deref()
                        .map(|s| m.subject == s)
                        .unwrap_or(true)
                        && f.knowledge_point
                            .as_deref()
                            .map(|k| m.knowledge_point == k)
                            .unwrap_or(true)
                        && f.is_correct.map(|c| m.is_correct == c).unwrap_or(true)
                })
                .cloned()
                .collect())
        }
        async fn update(&self, _id: &MistakeId, _p: &MistakePatch) -> Result<(), StorageError> {
            Err(StorageError::Internal("fake".into()))
        }
        async fn remove(&self, _id: &MistakeId) -> Result<(), StorageError> {
            Err(StorageError::Internal("fake".into()))
        }
    }

    struct FakeModel {
        reply: Mutex<String>,
    }

    #[async_trait::async_trait]
    impl ModelService for FakeModel {
        async fn stream(
            &self,
            _request: &ModelRequest,
            _signal: &AbortSignal,
        ) -> Result<ModelStream, ModelError> {
            unreachable!("FakeModel 只服务于 complete")
        }

        async fn complete(
            &self,
            _request: &ModelRequest,
            _signal: &AbortSignal,
        ) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse {
                text: self.reply.lock().expect("poisoned").clone(),
                tool_calls: Vec::new(),
                usage: None,
            })
        }
    }

    fn handles(
        reply: &str,
        store: Arc<FakeStore>,
    ) -> (ModelHandle, StorageHandle, MemoryHandle) {
        let model: Arc<dyn ModelService> = Arc::new(FakeModel {
            reply: Mutex::new(reply.into()),
        });
        let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
        (
            ModelHandle::new(model, std::time::Duration::from_secs(5), auditor),
            StorageHandle::new(store),
            MemoryHandle::new(Arc::new(FakeMemory::default())),
        )
    }

    #[test]
    fn exact_match_normalizes_and_compares() {
        assert!(exact_match(" 3 ", "3"));
        assert!(exact_match("ABC", "abc"));
        assert!(exact_match("a  b", "a b"));
        assert!(!exact_match("3", "4"));
    }

    #[tokio::test]
    async fn check_exact_match_skips_model_and_archives_nothing() {
        let store = Arc::new(FakeStore::default());
        let (model, storage, memory) = handles(
            r#"{"correct":false,"analysis":"不应走到模型判分"}"#,
            store.clone(),
        );
        let out = check_handler(
            model,
            storage,
            memory,
            false,
            json!({
                "question": "|-3| = ?",
                "student_answer": " 3 ",
                "reference_answer": "3",
                "subject": "数学",
                "knowledge_point": "绝对值",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["correct"], true);
        assert_eq!(out["method"], "exact_match");
        assert_eq!(out["archived_mistake"], false);
        assert!(store.items.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn check_exact_match_english_mode_returns_english() {
        let store = Arc::new(FakeStore::default());
        let (model, storage, memory) = handles(
            r#"{"correct":false,"analysis":"不应走到模型判分"}"#,
            store.clone(),
        );
        let out = check_handler(
            model,
            storage,
            memory,
            true,
            json!({
                "question": "The sun is bright.",
                "student_answer": "sunny",
                "reference_answer": "sunny",
                "subject": "英语",
                "knowledge_point": "词性转换",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["correct"], true);
        assert_eq!(out["method"], "exact_match");
        assert_eq!(
            out["analysis"],
            "The answer matches the reference answer and is correct."
        );
    }

    #[tokio::test]
    async fn check_wrong_answer_goes_model_and_archives() {
        let store = Arc::new(FakeStore::default());
        let (model, storage, memory) = handles(
            r#"{"correct":false,"score":0,"total":5,"analysis":"正确答案是 3，负数的绝对值应为正数"}"#,
            store.clone(),
        );
        let out = check_handler(
            model,
            storage,
            memory,
            false,
            json!({
                "question": "|-3| = ?",
                "student_answer": "-3",
                "reference_answer": "3",
                "subject": "数学",
                "knowledge_point": "绝对值",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["correct"], false);
        assert_eq!(out["method"], "model");
        assert_eq!(out["archived_mistake"], true);
        let items = store.items.lock().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].knowledge_point, "绝对值");
        assert_eq!(items[0].subject, "数学");
        assert!(!items[0].is_correct);
        assert_eq!(items[0].reference_answer.as_deref(), Some("3"));
    }

    #[tokio::test]
    async fn check_correct_free_form_archives_nothing() {
        let store = Arc::new(FakeStore::default());
        let (model, storage, memory) = handles(
            r#"{"correct":true,"analysis":"SAS 判定正确"}"#,
            store.clone(),
        );
        let out = check_handler(
            model,
            storage,
            memory,
            false,
            json!({
                "question": "证明 △ABC ≅ △DEF",
                "student_answer": "由 AB=DE、∠B=∠E、BC=EF，SAS 判定全等",
                "knowledge_point": "三角形全等判定",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["correct"], true);
        assert_eq!(out["archived_mistake"], false);
        assert!(store.items.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn check_rejects_empty_question_or_answer() {
        let store = Arc::new(FakeStore::default());
        let (model, storage, memory) = handles("", store);
        let err = check_handler(
            model,
            storage,
            memory,
            false,
            json!({ "question": "1+1=?", "student_answer": "" }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, ToolErrorCode::InvalidParams);
    }

    #[tokio::test]
    async fn check_records_history_with_item_id() {
        let store = Arc::new(FakeStore::default());
        let (model, storage, memory) = handles(
            r#"{"correct":false,"analysis":"绝对值应为正数"}"#,
            store.clone(),
        );
        let out = check_handler(
            model,
            storage,
            memory.clone(),
            false,
            json!({
                "question": "|-3| = ?",
                "student_answer": "-3",
                "reference_answer": "3",
                "subject": "数学",
                "knowledge_point": "绝对值",
                "item_id": "abs_evaluate",
                "difficulty": "basic",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["archived_mistake"], true);
        // 练习历史已落 memory：读回并断言记录了对错与标识。
        let mastered = crate::plugin::practice::history::recent_mastered(&memory).await;
        assert!(mastered.is_empty()); // 答错不算已掌握
        let path = crate::kernel::plugin::services::MemoryPath::parse(
            crate::plugin::practice::history::HISTORY_PATH,
        )
        .unwrap();
        let view = memory.show(Some(&path)).await.unwrap();
        let content = match view {
            crate::kernel::plugin::services::MemoryView::Entry { content, .. } => content,
            _ => panic!("练习历史应以单键 Entry 落盘"),
        };
        assert!(content.contains("abs_evaluate"));
        assert!(content.contains("false"));
    }
}
