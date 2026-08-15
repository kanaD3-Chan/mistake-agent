//! M2 验收：默认 MockModelService + 用户插件跑通 hello 回合。

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use so_lite_agent::context::PluginContext;
use so_lite_agent::contract::{CallerPolicy, Info, PluginError, empty_params};
use so_lite_agent::dispatch::ToolCallContext;
use so_lite_agent::events::{Event, MemoryEventSink};
use so_lite_agent::registry::{PluginDescriptor, UserPlugin};
use so_lite_agent::rpc::{KernelBuilder, Method, RpcRequest};

struct HelloPlugin;

impl UserPlugin for HelloPlugin {
    fn info() -> Info {
        Info {
            namespace: "hello".into(),
            tools: vec![so_lite_agent::contract::ToolDef {
                name: "hi".into(),
                user_visible: true,
                title: Some("打招呼".into()),
                group: Some("演示".into()),
                description: "演示工具：返回问候。".into(),
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
            "hi",
            Arc::new(|_ctx: &ToolCallContext, _params: Value| {
                Box::pin(async move { Ok(json!({"reply": "hi from so-lite-agent"})) })
            }),
        )
    }
}

#[tokio::test]
async fn hello_turn_with_mock_model() {
    let events = Arc::new(MemoryEventSink::default());
    let kernel = KernelBuilder::new()
        .event_sink(events.clone())
        .register_plugin(PluginDescriptor::from_plugin::<HelloPlugin>())
        .build()
        .await
        .expect("kernel 构建失败");

    kernel
        .handle(RpcRequest {
            id: 1,
            method: Method::SendUserMessage {
                text: "你好，打个招呼".into(),
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
            .any(|e| matches!(e, Event::MessageDelta { .. })),
        "应有流式消息增量事件：{events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, Event::TurnEnd { .. })),
        "应有回合结束事件：{events:?}"
    );
    assert_eq!(kernel.registry().user_entries().len(), 1);
}
