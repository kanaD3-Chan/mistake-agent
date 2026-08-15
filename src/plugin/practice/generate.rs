//! practice 插件：LLM 自由出题（模板未命中时的兜底路径，P1 智能出题核心）。
//!
//! 与 docs/variants.md 协议一致：结构化规格（question_text / answer_spec /
//! diagram_spec），题目、答案、图纸三者同源；确定性模板仍是优先路径。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::kernel::contract::ToolError;
use crate::kernel::message::Message;
use crate::kernel::plugin::services::{
    AbortSignal, ComputeError, ComputeHandle, ModelHandle, ModelKind, ModelRequest,
    ResponseFormat,
};
use crate::kernel::prompt::practice_generate_system_prompt;
use crate::plugin::vision::map_model_error;

use super::geometry_check::verify_diagram;
use super::templates::{Difficulty, PracticeItem};

/// 模型自由出题结果：沿用 docs/variants.md 的结构化规格（不包含 template_id，
/// 该字段由调用方以固定值 llm_freeform 落盘，标识自由出题路径）。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GeneratedItem {
    pub knowledge_point: String,
    pub question_text: String,
    pub answer_spec: String,
    #[serde(default)]
    pub diagram_spec: Option<Value>,
}

/// 难度 → 出题提示中的语义描述（与 docs/variants.md 分层定义一致）。
fn difficulty_label(difficulty: Difficulty) -> &'static str {
    match difficulty {
        Difficulty::Basic => "basic（基础：直接套用公式/定理）",
        Difficulty::Variant => "variant（同类变式：条件隐藏或逆用）",
        Difficulty::Advanced => "advanced（综合拔高：多步组合、辅助线、跨知识点联动）",
        // 真题层由 exam_pool 处理，model_generate 不会收到 Exam；此处仅为 match 穷尽。
        Difficulty::Exam => "exam（高考真题：池内抽取，非模型生成）",
    }
}

/// 模板未命中时走 LLM 生成：json_schema 强约束 + 容灾解析。
pub async fn model_generate(
    model: &ModelHandle,
    english_mode: bool,
    knowledge_point: &str,
    difficulty: Difficulty,
    mastered: &[String],
    geometry_feedback: Option<&str>,
) -> Result<PracticeItem, ToolError> {
    let system = Message::system(practice_generate_system_prompt(english_mode));
    let mut lines = vec![
        format!("知识点：{}", knowledge_point.trim()),
        format!("难度：{}", difficulty_label(difficulty)),
    ];
    // 几何校验失败重试：把上一版失败原因注入 prompt，要求模型修正图形数据。
    if let Some(feedback) = geometry_feedback {
        lines.push(feedback.to_string());
    }
    // 防重复：近期已掌握清单注入 prompt，要求模型避开相同/相似题。
    if !mastered.is_empty() {
        lines.push(format!(
            "请避开以下近期已做且已掌握的题目（不要出相同或高度相似的题）：{}",
            mastered.join("、")
        ));
    }
    let user = Message::user(lines.join("\n"));
    let mut request = ModelRequest::chat(ModelKind::Main, vec![system, user]);
    request.response_format = Some(ResponseFormat::JsonSchema {
        name: "practice_generate".into(),
        schema: serde_json::to_value(schemars::schema_for!(GeneratedItem)).unwrap_or_default(),
    });
    request.reasoning_effort = Some("none".into());
    let response = model
        .complete(&request, &AbortSignal::new())
        .await
        .map_err(map_model_error)?;
    let item = parse_generate_json(&response.text)?;
    Ok(PracticeItem {
        knowledge_point: item.knowledge_point.trim().to_string(),
        template_id: "llm_freeform".into(),
        difficulty,
        question_text: item.question_text,
        answer_spec: item.answer_spec,
        diagram_spec: item.diagram_spec,
        source: None,
    })
}

/// 几何校验最大重试次数（variants.md §3：失败换参数重出，连续失败即停）。
pub const MAX_GEOMETRY_ATTEMPTS: u32 = 3;

/// LLM 出题 + 几何可解性对拍：
/// - 代数/填空（无 diagram_spec）直接放行（先上线路径）；
/// - 有图形走 compute 校验，失败带原因重出（最多 MAX_GEOMETRY_ATTEMPTS 次）；
/// - 执行端不可用/超时降级放行（图形未校验，由前端渲染兜底）。
///
/// 返回 (条目, 是否已通过几何校验)。
pub async fn generate_with_check(
    compute: &ComputeHandle,
    model: &ModelHandle,
    english_mode: bool,
    knowledge_point: &str,
    difficulty: Difficulty,
    mastered: &[String],
    signal: &AbortSignal,
) -> Result<(PracticeItem, bool), ToolError> {
    let mut feedback: Option<String> = None;
    for attempt in 0..MAX_GEOMETRY_ATTEMPTS {
        let item = model_generate(
            model,
            english_mode,
            knowledge_point,
            difficulty,
            mastered,
            feedback.as_deref(),
        )
        .await?;
        let Some(spec) = item.diagram_spec.as_ref() else {
            // 代数/填空：无图形规格，直接放行。
            return Ok((item, false));
        };
        match verify_diagram(compute, spec, signal).await {
            Ok(None) => return Ok((item, true)),
            Ok(Some(reason)) => {
                feedback = Some(format!(
                    "上一版几何图形未通过可解性校验（第 {} 次）：{reason}。请修正点坐标/线段/圆/直角标记等图形数据后重新出题",
                    attempt + 1
                ));
            }
            Err(ComputeError::BackendUnavailable) | Err(ComputeError::Timeout) => {
                // 执行端未连接/超时：降级放行（图形未校验，前端渲染兜底）。
                return Ok((item, false));
            }
            Err(e) => return Err(ToolError::handler(format!("几何校验执行失败：{e}"))),
        }
    }
    Err(ToolError::handler(format!(
        "几何图形连续 {MAX_GEOMETRY_ATTEMPTS} 次未通过可解性校验，请调整题目表述或更换知识点后重试"
    )))
}

/// 容灾解析：先整段解析，失败则截取第一个 { 到最后一个 }（模型偶尔带前后缀）。
fn parse_generate_json(text: &str) -> Result<GeneratedItem, ToolError> {
    let trimmed = text.trim();
    if let Ok(r) = serde_json::from_str::<GeneratedItem>(trimmed) {
        return Ok(r);
    }
    if let (Some(s), Some(e)) = (trimmed.find('{'), trimmed.rfind('}')) {
        let slice = &trimmed[s..=e];
        if let Ok(r) = serde_json::from_str::<GeneratedItem>(slice) {
            return Ok(r);
        }
    }
    Err(ToolError::handler(format!(
        "模型出题结果无法解析：{}",
        text.chars().take(200).collect::<String>()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn difficulty_label_maps_all_tiers() {
        assert!(difficulty_label(Difficulty::Basic).contains("basic"));
        assert!(difficulty_label(Difficulty::Variant).contains("variant"));
        assert!(difficulty_label(Difficulty::Advanced).contains("advanced"));
    }

    #[test]
    fn parse_accepts_plain_json() {
        let item = parse_generate_json(
            r#"{"knowledge_point":"一元二次方程","question_text":"解方程 $x^2-3x+2=0$。","answer_spec":"$x=1$ 或 $x=2$","diagram_spec":null}"#,
        )
        .unwrap();
        assert_eq!(item.knowledge_point, "一元二次方程");
        assert!(item.diagram_spec.is_none());
    }

    #[test]
    fn parse_recovers_fenced_json() {
        let item = parse_generate_json(
            r#"好的，题目如下：
```json
{"knowledge_point":"圆与切线","question_text":"如图，PA 是圆 O 的切线，A 为切点。","answer_spec":"PA⊥OA","diagram_spec":{"points":{"O":[0,0],"A":[3,0]},"objects":[{"type":"circle","center":"O","radius":3},{"type":"segment","ends":["O","A"]}],"labels":["O","A"]}}
```"#,
        )
        .unwrap();
        assert_eq!(item.knowledge_point, "圆与切线");
        let spec = item.diagram_spec.unwrap();
        assert_eq!(spec["points"]["O"][0], 0);
        assert_eq!(spec["objects"].as_array().unwrap()[0]["type"], "circle");
    }

    #[test]
    fn parse_rejects_garbage() {
        let err = parse_generate_json("抱歉，我无法出题。").unwrap_err();
        assert!(err.message.contains("无法解析"));
    }
}
