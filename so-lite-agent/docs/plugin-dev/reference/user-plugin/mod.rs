// 用户插件参考模板（复制即开工，so-lite-agent 版）。

use std::sync::Arc;

use serde_json::{Value, json};

use so_lite_agent::context::PluginContext;
use so_lite_agent::contract::{CallerPolicy, Info, PluginError, ToolDef, empty_params};
use so_lite_agent::dispatch::ToolCallContext;
use so_lite_agent::registry::{PluginDescriptor, UserPlugin};

pub struct UserDemoPlugin;

impl UserPlugin for UserDemoPlugin {
    fn info() -> Info {
        Info {
            namespace: "user_demo".into(),
            requires: vec![],
            tools: vec![ToolDef {
                name: "ping".into(),
                user_visible: true,
                title: Some("示例工具".into()),
                group: Some("示例".into()),
                description: "示例工具：返回 pong。复制后改成你的工具。".into(),
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
            "ping",
            Arc::new(|_call_ctx: &ToolCallContext, _params: Value| {
                Box::pin(async move { Ok(json!({ "reply": "pong" })) })
            }),
        )
    }
}

pub fn descriptor() -> PluginDescriptor {
    PluginDescriptor::from_plugin::<UserDemoPlugin>()
}
