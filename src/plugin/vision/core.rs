//! vision 核心：读图/PDF → 图片理解（作业转写或内容描述）；只读不判分、不归档、
//! **不删除暂存文件**（模型读完可能还要调 grading::upload 判分归档，文件必须还在）。

use base64::Engine;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::kernel::agent::dispatch::ToolCallContext;
use crate::kernel::contract::ToolError;
use crate::kernel::events::{Event, EventSink};
use crate::kernel::message::{Attachment, Message, MessageKind};
use crate::kernel::plugin::services::{
    AbortSignal, ModelError, ModelHandle, ModelKind, ModelRequest, StorageHandle,
};
use crate::kernel::prompt::vision_prompt;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ReadParams {
    /// 图片/PDF 文件（应用上传后自动暂存的临时路径；不要要求用户手动输入路径）。
    pub file: String,
}

pub(crate) async fn read_handler(
    ctx: &ToolCallContext,
    params: Value,
    model: ModelHandle,
    storage: StorageHandle,
) -> Result<Value, ToolError> {
    let p: ReadParams =
        serde_json::from_value(params).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let content =
        read_content(&model, &storage, &p.file, &ctx.events, "vision::read", ctx.english_mode).await?;
    Ok(json!({
        "content": content,
        "chars": content.chars().count(),
        "graded": false,
        "archived": false,
    }))
}

/// 共用读文件：经 StorageHandle 读附件暂存（ADR-0042 磁盘 IO 铁律——
/// 白名单校验在 storage 实现内，本插件不持有文件句柄）→ 图片理解 / PDF 抽文 → 空内容兜底。
/// 不删除暂存文件（由调用方决定清理时机）。
pub(crate) async fn read_content(
    model: &ModelHandle,
    storage: &StorageHandle,
    file: &str,
    events: &Arc<dyn EventSink>,
    entry: &str,
    english_mode: bool,
) -> Result<String, ToolError> {
    let bytes = storage
        .read_staged(file)
        .await
        .map_err(|e| ToolError::handler(format!("读取暂存文件失败：{e}")))?;
    let ext = std::path::Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_lowercase();

    events.emit(Event::ToolProgress {
        entry: entry.into(),
        message: "正在理解图片…".into(),
        icon: Some("mdi:image-search".into()),
    });
    let content = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "bmp" => {
            let mime = match ext.as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "webp" => "image/webp",
                _ => "image/bmp",
            };
            understand_image(model, mime, &bytes, events, entry, english_mode).await?
        }
        "pdf" => extract_pdf_text(&bytes).await?,
        other => {
            return Err(ToolError::handler(format!(
                "不支持的文件类型：{other}（支持 png/jpg/jpeg/webp/bmp/pdf）"
            )));
        }
    };
    if content.trim().is_empty() {
        return Err(ToolError::handler(
            "未能识别到图片/PDF 内容（扫描版 PDF 请拍照上传图片）",
        ));
    }
    Ok(content)
}

/// 图片理解：视觉模型按内容类型处理（作业转写 / 图片描述），不判分（用户明确要求）。
async fn understand_image(
    model: &ModelHandle,
    mime: &str,
    bytes: &[u8],
    events: &Arc<dyn EventSink>,
    entry: &str,
    english_mode: bool,
) -> Result<String, ToolError> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let attachments = vec![Attachment {
        mime: mime.into(),
        data_base64: b64,
    }];
    let mut msg = Message::user(vision_prompt(english_mode));
    if let MessageKind::User { attachments: a, .. } = &mut msg.kind {
        *a = attachments;
    }
    let request = ModelRequest::chat(ModelKind::Vision, vec![msg]);
    let response = model
        .complete(&request, &AbortSignal::new())
        .await
        .map_err(map_model_error)?;
    events.emit(Event::ToolProgress {
        entry: entry.into(),
        message: format!("图片理解完成（{} 字）", response.text.chars().count()),
        icon: Some("mdi:image-search".into()),
    });
    Ok(response.text)
}

async fn extract_pdf_text(bytes: &[u8]) -> Result<String, ToolError> {
    match pdf_extract::extract_text_from_mem(bytes) {
        Ok(text) if !text.trim().is_empty() => Ok(text),
        Ok(_) => Err(ToolError::handler(
            "PDF 没有可提取的文字（可能是扫描版），请拍照上传图片",
        )),
        Err(e) => Err(ToolError::handler(format!("PDF 解析失败：{e}"))),
    }
}

pub(crate) fn map_model_error(e: ModelError) -> ToolError {
    match e {
        ModelError::Timeout => ToolError::timeout(),
        ModelError::Cancelled => ToolError::aborted(),
        other if other.is_systemic() => ToolError::model_unavailable(other.to_string()),
        other => ToolError::handler(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::audit::{Auditor, MemoryAuditSink};
    use crate::kernel::events::MemoryEventSink;
    use crate::kernel::plugin::services::{
        ModelError, ModelResponse, ModelService, ModelStream, StorageHandle,
    };
    use crate::kernel::plugin::storage::FileStorage;

    struct FakeVision;

    #[async_trait::async_trait]
    impl ModelService for FakeVision {
        async fn stream(
            &self,
            _request: &ModelRequest,
            _signal: &AbortSignal,
        ) -> Result<ModelStream, ModelError> {
            Err(ModelError::Transport("fake".into()))
        }
        async fn complete(
            &self,
            _request: &ModelRequest,
            _signal: &AbortSignal,
        ) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse {
                text: "银发少女，蓝瞳，穿着白色连衣裙，站在樱花树下。".into(),
                tool_calls: vec![],
                usage: None,
            })
        }
    }

    fn temp_storage() -> StorageHandle {
        let dir = std::env::temp_dir().join(format!(
            "mistake-agent-vision-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store: Arc<FileStorage> = Arc::new(FileStorage::open(&dir).unwrap());
        StorageHandle::new(store.clone()).with_io(store.clone(), store.clone())
    }

    #[tokio::test]
    async fn read_content_describes_image_without_deleting_file() {
        let storage = temp_storage();
        // 真实暂存文件（temp 目录 + mistake-agent- 前缀）。
        let temp =
            std::env::temp_dir().join(format!("mistake-agent-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&temp, b"x").unwrap();
        let model = ModelHandle::new(
            Arc::new(FakeVision),
            std::time::Duration::from_secs(10),
            Auditor::new(Arc::new(MemoryAuditSink::default())),
        );
        let events: Arc<dyn EventSink> = Arc::new(MemoryEventSink::default());
        let content = read_content(
            &model,
            &storage,
            &temp.to_string_lossy(),
            &events,
            "vision::read",
            false,
        )
        .await
        .expect("读图应成功");
        assert!(content.contains("银发少女"));
        assert!(temp.exists(), "read 不应删除暂存文件（判分还要用）");
        let _ = std::fs::remove_file(&temp);
    }

    #[tokio::test]
    async fn read_content_rejects_non_staged_path() {
        let storage = temp_storage();
        let model = ModelHandle::new(
            Arc::new(FakeVision),
            std::time::Duration::from_secs(10),
            Auditor::new(Arc::new(MemoryAuditSink::default())),
        );
        let events: Arc<dyn EventSink> = Arc::new(MemoryEventSink::default());
        let err = read_content(&model, &storage, "/etc/passwd", &events, "vision::read", false)
            .await
            .expect_err("非暂存路径应被拒绝");
        assert!(err.message.contains("读取暂存文件失败"));
    }

    #[tokio::test]
    async fn read_content_rejects_non_prefixed_temp_file() {
        let storage = temp_storage();
        let temp =
            std::env::temp_dir().join(format!("other-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&temp, b"x").unwrap();
        let model = ModelHandle::new(
            Arc::new(FakeVision),
            std::time::Duration::from_secs(10),
            Auditor::new(Arc::new(MemoryAuditSink::default())),
        );
        let events: Arc<dyn EventSink> = Arc::new(MemoryEventSink::default());
        let err = read_content(
            &model,
            &storage,
            &temp.to_string_lossy(),
            &events,
            "vision::read",
            false,
        )
        .await
        .expect_err("非 mistake-agent- 前缀应拒绝");
        assert!(err.message.contains("读取暂存文件失败"));
        let _ = std::fs::remove_file(&temp);
    }
}
