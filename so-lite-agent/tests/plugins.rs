//! M4 验收：按迁移后的插件手册写一个内核插件 + 一个用户插件，注册并跑通真实工具调用。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use so_lite_agent::context::{KernelContext, PluginContext};
use so_lite_agent::contract::{CallerPolicy, Info, PluginError, ToolDef, empty_params};
use so_lite_agent::dispatch::ToolCallContext;
use so_lite_agent::events::{Event, MemoryEventSink};
use so_lite_agent::registry::{KernelDescriptor, KernelPlugin, PluginDescriptor, UserPlugin};
use so_lite_agent::rpc::{KernelBuilder, Method, RpcRequest};
use so_lite_agent::services::{
    AbortSignal, ItemKind, ModelChunk, ModelError, ModelRequest, ModelResponse, ModelService,
    ModelStream,
};

struct KernelDemoPlugin;

impl KernelPlugin for KernelDemoPlugin {
    fn info() -> Info {
        Info {
            namespace: "kernel_demo".into(),
            tools: vec![ToolDef {
                name: "ping".into(),
                user_visible: true,
                title: Some("内核示例".into()),
                group: Some("示例".into()),
                description: "内核插件示例工具。".into(),
                params: empty_params(),
                policy: CallerPolicy::UserAndModel,
                timeout: None,
                icon: None,
            }],
            ..Default::default()
        }
    }

    fn register(ctx: KernelContext<'_>) -> Result<(), PluginError> {
        ctx.registrar.tool(
            "ping",
            Arc::new(|_ctx: &ToolCallContext, _params: Value| {
                Box::pin(async move { Ok(json!({"reply": "kernel pong"})) })
            }),
        )
    }
}

struct UserDemoPlugin;

impl UserPlugin for UserDemoPlugin {
    fn info() -> Info {
        Info {
            namespace: "user_demo".into(),
            tools: vec![ToolDef {
                name: "hello".into(),
                user_visible: true,
                title: Some("用户示例".into()),
                group: Some("示例".into()),
                description: "用户插件示例工具。".into(),
                params: empty_params(),
                policy: CallerPolicy::UserAndModel,
                timeout: None,
                icon: None,
            }],
            ..Default::default()
        }
    }

    fn register(ctx: PluginContext<'_>) -> Result<(), PluginError> {
        ctx.registrar.tool(
            "hello",
            Arc::new(|_ctx: &ToolCallContext, _params: Value| {
                Box::pin(async move { Ok(json!({"reply": "user hello"})) })
            }),
        )
    }
}

#[derive(Default)]
struct ScriptedToolModel {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelService for ScriptedToolModel {
    async fn stream(
        &self,
        _request: &ModelRequest,
        _signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = match call {
            0 => vec![
                Ok(ModelChunk::ToolCallStart {
                    index: 0,
                    call_id: "call_0".into(),
                    name: "kernel_demo__ping".into(),
                }),
                Ok(ModelChunk::ItemDone {
                    kind: ItemKind::FunctionCall,
                }),
                Ok(ModelChunk::ToolCallStart {
                    index: 1,
                    call_id: "call_1".into(),
                    name: "user_demo__hello".into(),
                }),
                Ok(ModelChunk::ItemDone {
                    kind: ItemKind::FunctionCall,
                }),
                Ok(ModelChunk::Done),
            ],
            1 => vec![
                Ok(ModelChunk::TextDelta("完成".into())),
                Ok(ModelChunk::ItemDone {
                    kind: ItemKind::Message,
                }),
                Ok(ModelChunk::Done),
            ],
            _ => vec![],
        };
        Ok(Box::new(futures_util::stream::iter(chunks)))
    }

    async fn complete(
        &self,
        _request: &ModelRequest,
        _signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        Err(ModelError::Transport("测试模型不支持 complete".into()))
    }
}

#[tokio::test]
async fn kernel_and_user_plugins_register_and_run() {
    let events = Arc::new(MemoryEventSink::default());
    let kernel = KernelBuilder::new()
        .event_sink(events.clone())
        .main_model(Arc::new(ScriptedToolModel::default()))
        .register_kernel_plugin(KernelDescriptor::from_plugin::<KernelDemoPlugin>())
        .register_plugin(PluginDescriptor::from_plugin::<UserDemoPlugin>())
        .build()
        .await
        .expect("kernel 构建失败");

    assert_eq!(kernel.registry().user_entries().len(), 2, "双插件应都可见");
    kernel
        .handle(RpcRequest {
            id: 1,
            method: Method::SendUserMessage {
                text: "调用工具演示".into(),
                force_tool: None,
                attachments: vec![],
            }
            .into(),
        })
        .await
        .expect("请求失败")
        .expect("应有响应帧");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !kernel.is_idle().await && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(kernel.is_idle().await, "回合应在 10s 内结束");

    let events = events.take();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ToolStart { entry, .. } if entry == "kernel_demo::ping")),
        "内核插件工具应被调用：{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ToolStart { entry, .. } if entry == "user_demo::hello")),
        "用户插件工具应被调用：{events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::ToolEnd { entry, ok: true } if entry == "kernel_demo::ping"
        )),
        "内核插件工具应成功：{events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::ToolEnd { entry, ok: true } if entry == "user_demo::hello"
        )),
        "用户插件工具应成功：{events:?}"
    );
}
