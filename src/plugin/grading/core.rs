//! grading 核心实现：上传 handler（图片理解 → 判分 → 归档）、进度播报。
//! 图片理解（读图/PDF）复用 vision 插件（crate::plugin::vision::read_content）。

use serde_json::{Value, json};

use crate::kernel::agent::dispatch::ToolCallContext;
use crate::kernel::contract::ToolError;
use crate::kernel::events::Event;
use crate::kernel::message::Message;
use crate::kernel::plugin::services::{
    AbortSignal, Mistake, MistakeId, MistakePatch, ModelHandle, ModelKind, ModelRequest,
    ResponseFormat, StorageHandle,
};
use crate::kernel::prompt::grading_system_prompt;
use crate::plugin::vision::{map_model_error, read_content};

use super::params::{GetParams, GradedItem, RemoveManyParams, RemoveParams, UpdateParams, UploadParams};

fn parse_mistake_id(raw: &str) -> Result<MistakeId, ToolError> {
    uuid::Uuid::parse_str(raw)
        .map(MistakeId)
        .map_err(|e| ToolError::invalid_params(format!("非法错题 id：{e}")))
}

pub(crate) async fn get_handler(
    storage: StorageHandle,
    params: Value,
) -> Result<Value, ToolError> {
    let p: GetParams =
        serde_json::from_value(params).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let id = parse_mistake_id(&p.id)?;
    let mistake = storage
        .get(&id)
        .await
        .map_err(|e| ToolError::handler(e.to_string()))?
        .ok_or_else(|| ToolError::handler(format!("错题不存在：{}", p.id)))?;
    Ok(json!({ "mistake": mistake }))
}

pub(crate) async fn update_handler(
    storage: StorageHandle,
    params: Value,
) -> Result<Value, ToolError> {
    let p: UpdateParams =
        serde_json::from_value(params).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let id = parse_mistake_id(&p.id)?;
    let patch = MistakePatch {
        subject: p.subject,
        knowledge_point: p.knowledge_point,
        question: p.question,
        student_answer: p.student_answer,
        reference_answer: p.reference_answer,
        analysis: p.analysis,
        is_correct: p.is_correct,
        pinned: p.pinned,
    };
    storage
        .update(&id, &patch)
        .await
        .map_err(|e| ToolError::handler(e.to_string()))?;
    let mistake = storage
        .get(&id)
        .await
        .map_err(|e| ToolError::handler(e.to_string()))?
        .ok_or_else(|| ToolError::handler(format!("错题不存在：{}", p.id)))?;
    Ok(json!({ "mistake": mistake }))
}

pub(crate) async fn remove_handler(
    storage: StorageHandle,
    params: Value,
) -> Result<Value, ToolError> {
    let p: RemoveParams =
        serde_json::from_value(params).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let id = parse_mistake_id(&p.id)?;
    storage
        .remove(&id)
        .await
        .map_err(|e| ToolError::handler(e.to_string()))?;
    Ok(json!({ "deleted": true, "id": id.to_string() }))
}

pub(crate) async fn remove_many_handler(
    storage: StorageHandle,
    params: Value,
) -> Result<Value, ToolError> {
    let p: RemoveManyParams =
        serde_json::from_value(params).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let ids = p
        .ids
        .iter()
        .map(|raw| parse_mistake_id(raw))
        .collect::<Result<Vec<_>, _>>()?;
    let deleted = storage
        .remove_many(&ids)
        .await
        .map_err(|e| ToolError::handler(e.to_string()))?;
    Ok(json!({ "deleted": deleted }))
}

pub(crate) async fn upload_handler(
    ctx: &ToolCallContext,
    params: Value,
    storage: StorageHandle,
    model: ModelHandle,
) -> Result<Value, ToolError> {
    let p: UploadParams =
        serde_json::from_value(params).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    // 先读图（vision::read_content：图片理解/PDF 抽文），不删文件；读完确认内容后判分，
    // 再经 StorageHandle 清理暂存副本（ADR-0042 磁盘 IO 铁律，插件不持有文件句柄）。
    let content =
        read_content(&model, &storage, &p.file, &ctx.events, "grading::upload", ctx.english_mode).await?;
    storage
        .remove_staged(&p.file)
        .await
        .map_err(|e| ToolError::handler(format!("清理暂存文件失败：{e}")))?;

    emit_progress(ctx, "grading::upload", "正在逐题判分…");
    let grading_text = grade_content(&model, &content, ctx).await?;
    let items: Vec<GradedItem> = parse_grading_json(&grading_text)?;

    let mut wrong_count = 0usize;
    let mut archived = 0usize;
    for item in &items {
        if !item.correct {
            wrong_count += 1;
            let mistake = Mistake {
                id: MistakeId(uuid::Uuid::new_v4()),
                subject: item
                    .subject
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "未分类".into()),
                knowledge_point: item
                    .knowledge_point
                    .clone()
                    .unwrap_or_else(|| "未标注".into()),
                question: item.question.clone(),
                student_answer: item
                    .student_answer
                    .clone()
                    .unwrap_or_else(|| "（未作答）".into()),
                reference_answer: item
                    .reference_answer
                    .clone()
                    .filter(|s| !s.trim().is_empty()),
                is_correct: false,
                analysis: item.analysis.clone().unwrap_or_default(),
                created_at: chrono::Utc::now(),
                pinned: false,
                deleted_at: None,
            };
            match storage.save(&mistake).await {
                Ok(_) => archived += 1,
                Err(e) => {
                    return Err(ToolError::handler(format!("错题归档失败：{e}")));
                }
            }
        }
    }

    emit_progress(ctx, "grading::upload", "批改完成");
    Ok(json!({
        "total": items.len(),
        "correct_count": items.len() - wrong_count,
        "wrong_count": wrong_count,
        "archived_mistakes": archived,
        "items": items,
    }))
}

/// 判分：主模型按图片理解内容逐题批改，输出 JSON 数组。
async fn grade_content(
    model: &ModelHandle,
    content: &str,
    ctx: &ToolCallContext,
) -> Result<String, ToolError> {
    let system = Message::system(grading_system_prompt(ctx.english_mode));
    let user = Message::user(format!("作业 OCR 内容：\n{content}\n请逐题批改。"));
    let mut request = ModelRequest::chat(ModelKind::Main, vec![system, user]);
    // 内联扁平数组 schema：避免 $defs/$ref（DeepSeek json_schema 端不解析引用）。
    let item_schema = serde_json::to_value(schemars::schema_for!(GradedItem)).unwrap_or_default();
    let schema = json!({
        "type": "array",
        "items": item_schema,
    });
    request.response_format = Some(ResponseFormat::JsonSchema {
        name: "graded_items".into(),
        schema: serde_json::to_value(schema).unwrap_or_default(),
    });
    request.reasoning_effort = Some("none".into());
    let response = model
        .complete(&request, &AbortSignal::new())
        .await
        .map_err(map_model_error)?;
    ctx.events.emit(Event::ToolProgress {
        entry: "grading::upload".into(),
        message: "判分完成".into(),
        icon: Some("mdi:upload".into()),
    });
    Ok(response.text)
}

fn parse_grading_json(text: &str) -> Result<Vec<GradedItem>, ToolError> {
    let trimmed = text.trim();
    if let Ok(items) = serde_json::from_str::<Vec<GradedItem>>(trimmed) {
        return Ok(items);
    }
    // 容灾：单对象（模型可能没按数组输出）。
    if let Ok(item) = serde_json::from_str::<GradedItem>(trimmed) {
        return Ok(vec![item]);
    }
    // 容灾：从文本中截取第一个 [ 到最后一个 ]。
    if let (Some(s), Some(e)) = (trimmed.find('['), trimmed.rfind(']')) {
        let slice = &trimmed[s..=e];
        if let Ok(items) = serde_json::from_str::<Vec<GradedItem>>(slice) {
            return Ok(items);
        }
    }
    // 容灾：截取第一个 { 到最后一个 }（单对象）。
    if let (Some(s), Some(e)) = (trimmed.find('{'), trimmed.rfind('}')) {
        let slice = &trimmed[s..=e];
        if let Ok(item) = serde_json::from_str::<GradedItem>(slice) {
            return Ok(vec![item]);
        }
    }
    Err(ToolError::handler(format!(
        "判分结果无法解析：{}",
        text.chars().take(200).collect::<String>()
    )))
}

fn emit_progress(ctx: &ToolCallContext, entry: &str, message: &str) {
    ctx.events.emit(Event::ToolProgress {
        entry: entry.into(),
        message: message.into(),
        icon: Some("mdi:image-search".into()),
    });
}
