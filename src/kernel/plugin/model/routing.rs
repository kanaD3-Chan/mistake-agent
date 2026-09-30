use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;

use crate::kernel::message::{Attachment, MessageKind};
use crate::kernel::plugin::services::{
    AbortSignal, Domain, DomainIo, ModelError, ModelRequest, ModelResponse, ModelService,
    ModelStream, RelPath,
};
use crate::kernel::settings::{DEFAULT_SERVER_URL, ModelConfig, Settings, Transport};

use super::*;

/// 生效的模型配置（ADR-0048 决策 2）：`account.token` 非空 → 走平台中转，否则沿用 `main_model`。
///
/// **只换地址与密钥**：`model` / `transport` 保持用户的选择。中转面两种协议都认
/// （`/responses` 与 `/v1/chat/completions`），模型名由服务端强制覆盖，客户端无需跟着改。
pub fn effective_config(settings: &Settings) -> ModelConfig {
    let mut cfg = settings.main_model.clone();
    if settings.account.logged_in() {
        // 手工编辑过 settings.json 把地址清空时别拼出空 URL——用默认地址兜住。
        let url = settings.account.server_url.trim();
        cfg.api_url = if url.is_empty() {
            DEFAULT_SERVER_URL.to_string()
        } else {
            url.to_string()
        };
        cfg.api_key = settings.account.token.clone();
    }
    cfg
}

pub fn build_main_service(settings: &Settings) -> Arc<dyn ModelService> {
    let cfg = effective_config(settings);
    let model = cfg
        .model
        .clone()
        .unwrap_or_else(|| "deepseek-flash".into());
    match cfg.transport.unwrap_or_default() {
        Transport::Responses => Arc::new(ResponsesModelService::new(
            cfg.api_url.clone(),
            cfg.api_key.clone(),
            model,
        )),
        Transport::ChatCompletions => Arc::new(ChatCompletionsModelService::new(
            cfg.api_url.clone(),
            cfg.api_key.clone(),
            model,
        )),
    }
}

/// 配置热更新的模型服务（ADR-0015/0027/0045）：持有共享 Settings，按当前配置重建底层适配器。
/// `refresh()` 在 set_settings 保存成功后调用，下一次模型调用即用新配置；
/// 不重建时行为与构建期快照完全一致。
pub struct LiveSettingsModelService {
    settings: Arc<std::sync::RwLock<Settings>>,
    current: std::sync::RwLock<Arc<dyn ModelService>>,
}

impl LiveSettingsModelService {
    pub fn new(settings: Arc<std::sync::RwLock<Settings>>) -> Self {
        let snapshot = settings.read().expect("settings poisoned").clone();
        let current = build_main_service(&snapshot);
        Self {
            settings,
            current: std::sync::RwLock::new(current),
        }
    }

    /// 按当前 settings 重建底层适配器（set_settings 成功后调用）。
    pub fn refresh(&self) {
        let snapshot = self.settings.read().expect("settings poisoned").clone();
        let rebuilt = build_main_service(&snapshot);
        *self.current.write().expect("model service poisoned") = rebuilt;
    }
}

#[async_trait::async_trait]
impl ModelService for LiveSettingsModelService {
    async fn stream(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError> {
        let svc = self.current.read().expect("model service poisoned").clone();
        svc.stream(request, signal).await
    }

    async fn complete(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        let svc = self.current.read().expect("model service poisoned").clone();
        svc.complete(request, signal).await
    }
}

/// 图片附件解析包装层（ADR-0046）：用户消息只持久化 uploads/ 路径引用，
/// 构建模型请求时按引用读盘、base64 回填运行时 `attachments`，让图片直入 Responses `input_image`。
/// 读取结果按文件名进程内缓存（消息不可变，避免每回合重复磁盘 IO 与审计噪声）。
pub struct AttachmentResolvingModelService {
    inner: Arc<dyn ModelService>,
    io: Arc<dyn DomainIo>,
    cache: Mutex<HashMap<String, Attachment>>,
}

impl AttachmentResolvingModelService {
    pub fn new(inner: Arc<dyn ModelService>, io: Arc<dyn DomainIo>) -> Self {
        Self {
            inner,
            io,
            cache: Mutex::new(HashMap::new()),
        }
    }

    async fn resolve(&self, request: &ModelRequest) -> ModelRequest {
        let mut resolved = request.clone();
        for msg in &mut resolved.messages {
            if let MessageKind::User {
                attachment_refs,
                attachments,
                ..
            } = &mut msg.kind
            {
                if attachment_refs.is_empty() || !attachments.is_empty() {
                    continue;
                }
                for r in attachment_refs.iter() {
                    match self.load(&r.name).await {
                        Ok(mut att) => {
                            if !r.mime.is_empty() {
                                att.mime = r.mime.clone();
                            }
                            attachments.push(att);
                        }
                        Err(e) => log::warn!("附件读取失败（{}）：{e}", r.name),
                    }
                }
            }
        }
        resolved
    }

    async fn load(&self, name: &str) -> Result<Attachment, String> {
        if let Some(hit) = self.cache.lock().expect("cache poisoned").get(name) {
            return Ok(hit.clone());
        }
        let rel = RelPath::parse(name).map_err(|e| e.to_string())?;
        let bytes = self
            .io
            .read(Domain::Uploads, &rel)
            .await
            .map_err(|e| e.to_string())?;
        let att = Attachment {
            mime: mime_for_name(name).into(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        };
        self.cache
            .lock()
            .expect("cache poisoned")
            .insert(name.to_string(), att.clone());
        Ok(att)
    }
}

fn mime_for_name(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "image/png",
    }
}

#[async_trait::async_trait]
impl ModelService for AttachmentResolvingModelService {
    async fn stream(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError> {
        let resolved = self.resolve(request).await;
        self.inner.stream(&resolved, signal).await
    }

    async fn complete(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        let resolved = self.resolve(request).await;
        self.inner.complete(&resolved, signal).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::message::Message;
    use crate::kernel::plugin::model::responses::{ResponsesModelService, SseParser};
    use crate::kernel::plugin::services::{ItemKind, ModelChunk};
    use futures_util::StreamExt;

    fn settings_with_account(token: &str, server_url: &str) -> Settings {
        Settings {
            log_level: crate::kernel::logger::Level::Info,
            english_mode: false,
            nickname: String::new(),
            main_model: ModelConfig {
                api_url: "https://api.deepseek.com".into(),
                api_key: "sk-self".into(),
                model: Some("deepseek-v4-flash".into()),
                transport: Some(Transport::ChatCompletions),
            },
            vision_model: ModelConfig {
                api_url: String::new(),
                api_key: String::new(),
                model: None,
                transport: None,
            },
            account: crate::kernel::settings::AccountConfig {
                server_url: server_url.into(),
                token: token.into(),
                email: String::new(),
                role: String::new(),
                sync_enabled: false,
            },
        }
    }

    /// 模型链路走本地还是走平台，只由 `account.token` 决定（ADR-0048 决策 2）。
    #[test]
    fn effective_config_switches_on_token_only() {
        // 未登录：原样用自备 Key。
        let cfg = effective_config(&settings_with_account("", "https://platform.example.com"));
        assert_eq!(cfg.api_url, "https://api.deepseek.com");
        assert_eq!(cfg.api_key, "sk-self");

        // 已登录：地址与密钥换成平台，模型 ID 与接入方式保持用户选择
        // ——模型名由服务端强制覆盖，transport 两种面都认，客户端不跟着改。
        let cfg = effective_config(&settings_with_account("mka_tok", "https://platform.example.com"));
        assert_eq!(cfg.api_url, "https://platform.example.com");
        assert_eq!(cfg.api_key, "mka_tok");
        assert_eq!(cfg.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(cfg.transport, Some(Transport::ChatCompletions));

        // 手工把 settings.json 的地址编辑成空白：不能拼出空 URL。
        let cfg = effective_config(&settings_with_account("mka_tok", "   "));
        assert_eq!(cfg.api_url, DEFAULT_SERVER_URL);
    }

    #[test]
    fn sse_parser_handles_events() {
        let mut parser = SseParser::default();
        let events = parser.push_chunk(
            b"event: response.output_text.delta\ndata: {\"delta\":\"a\"}\n\nevent: response.output_text.delta\ndata: {\"delta\":\"b\"}\n\n",
        );
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].name, "response.output_text.delta");
        assert_eq!(events[0].data, r#"{"delta":"a"}"#);
    }

    #[test]
    fn responses_input_expands_tool_call() {
        let mut call =
            Message::tool_call("demo::hello", json!({"x": 1}), Ok(json!({"reply": "hi"})));
        let _ = &mut call;
        let user = Message::user("你好");
        let items = messages_to_responses_input(&[user, call]).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[1]["type"], "function_call");
        assert_eq!(items[1]["name"], "demo__hello");
        assert_eq!(items[2]["type"], "function_call_output");
    }

    /// 回归测试（ticket 02）：DeepSeek Responses 真实事件形状——
    /// item 的类型在 `item` 子对象里，usage 在 `response.usage`。
    /// 用本地假 HTTP 服务喂探针抓到的 SSE 序列，验证消息/工具调用/usage 映射。
    #[tokio::test]
    async fn responses_stream_maps_real_event_shapes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let sse = concat!(
            "event: response.created\ndata: {\"type\":\"response.created\"}\n\n",
            "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc1\",\"call_id\":\"call_1\",\"name\":\"get_weather\",\"arguments\":\"\"},\"output_index\":1}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc1\",\"output_index\":1,\"delta\":\"{\\\"city\\\":\\\"北京\\\"}\"}\n\n",
            "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc1\",\"call_id\":\"call_1\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"北京\\\"}\"},\"output_index\":1}\n\n",
            "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"message\",\"id\":\"m1\"},\"output_index\":2}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"m1\",\"output_index\":2,\"delta\":\"北京今天晴天\"}\n\n",
            "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"id\":\"m1\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"北京今天晴天\"}]},\"output_index\":2}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":84,\"input_tokens_details\":{\"cached_tokens\":0},\"output_tokens\":27,\"output_tokens_details\":{\"reasoning_tokens\":17}}}}\n\n",
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await;
            let header =
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            sock.write_all(header.as_bytes()).await.unwrap();
            sock.write_all(sse.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        });

        let svc = ResponsesModelService::new(
            format!("http://{addr}"),
            "test-key".into(),
            "deepseek-flash".into(),
        );
        let request = ModelRequest {
            messages: vec![Message::user("北京天气？")],
            tools: None,
            reasoning_effort: None,
            response_format: None,
            tool_choice: None,
        };
        let mut stream = svc
            .stream(&request, &AbortSignal::new())
            .await
            .expect("stream 应成功");

        let mut text = String::new();
        let mut tool_name = String::new();
        let mut tool_args = String::new();
        let mut message_done = false;
        let mut call_done = false;
        let mut usage = None;
        while let Some(chunk) = stream.next().await {
            match chunk.expect("chunk 无错误") {
                ModelChunk::TextDelta(d) => text.push_str(&d),
                ModelChunk::ToolCallStart { name, .. } => tool_name = name,
                ModelChunk::ToolCallDelta { data, .. } => tool_args.push_str(&data),
                ModelChunk::ItemDone {
                    kind: ItemKind::Message,
                } => message_done = true,
                ModelChunk::ItemDone {
                    kind: ItemKind::FunctionCall,
                } => call_done = true,
                ModelChunk::ItemDone {
                    kind: ItemKind::Reasoning,
                } => {}
                ModelChunk::Usage(u) => usage = Some(u),
                ModelChunk::ReasoningDelta(_)
                | ModelChunk::ReasoningItemStart { .. }
                | ModelChunk::Done => {}
            }
        }
        assert_eq!(tool_name, "get_weather");
        assert_eq!(tool_args, r#"{"city":"北京"}"#);
        assert!(call_done);
        assert_eq!(text, "北京今天晴天");
        assert!(message_done);
        let usage = usage.expect("usage 应解析到");
        assert_eq!(usage.input_tokens, Some(84));
        assert_eq!(usage.output_tokens, Some(27));
        assert_eq!(usage.cached_tokens, Some(0));
        assert_eq!(usage.reasoning_tokens, Some(17));
    }
}
