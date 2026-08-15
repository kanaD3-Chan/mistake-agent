//! M2 验收示例：`cargo run --example hello` 用默认 MockModelService
//! 注册一个 hello 用户插件并跑通一个完整回合。

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use so_lite_agent::context::PluginContext;
use so_lite_agent::contract::{CallerPolicy, Info, PluginError, empty_params};
use so_lite_agent::dispatch::ToolCallContext;
use so_lite_agent::events::MemoryEventSink;
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

#[tokio::main]
async fn main() -> Result<(), String> {
    let events = Arc::new(MemoryEventSink::default());
    let kernel = KernelBuilder::new()
        .event_sink(events.clone())
        .register_plugin(PluginDescriptor::from_plugin::<HelloPlugin>())
        .build()
        .await?;

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
        .map_err(|e| e.message)?
        .expect("应有响应帧");

    while !kernel.is_idle().await {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let events = events.take();
    println!("hello 回合完成，事件数：{}", events.len());
    println!("工具清单：{}", kernel.registry().user_entries().len());
    Ok(())
}
