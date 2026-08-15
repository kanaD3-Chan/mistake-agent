//! DeepSeek reasoning 回传回归（ADR-0020 相关）：
//! 真实 API 验证「thinking 开启 + 工具调用」的历史回传能被接受。
//! - REPRO_MODE=per_call：每个 function_call 前都有 reasoning（主修序列化后的形态，应直接通过）；
//! - REPRO_MODE=none/between_calls：异常历史，应触发兜底（剥离 reasoning + effort=none）后通过。
//!
//! 运行：cargo test --test repro_reasoning -- --ignored --nocapture

use futures_util::StreamExt;
use mistake_agent::kernel::agent::rpc::{Kernel, Method, RpcRequest};
use mistake_agent::kernel::events::MemoryEventSink;
use mistake_agent::kernel::message::Message;
use mistake_agent::kernel::plugin::model::build_main_service;
use mistake_agent::kernel::plugin::services::{
    AbortSignal, ModelChunk, ModelKind, ModelRequest, ToolSchema,
};
use mistake_agent::kernel::settings::Settings;
use std::sync::Arc;

#[tokio::test]
#[ignore]
async fn replay_real_session_history() {
    let settings = Settings::load().expect("settings");
    let svc = build_main_service(&settings);
    let events = Arc::new(MemoryEventSink::default());
    let kernel = Kernel::new(events.clone()).await.expect("kernel 启动失败");
    let frame = kernel
        .handle(RpcRequest {
            id: 1,
            method: Method::ListTools.into(),
        })
        .await
        .expect("list_tools 失败")
        .expect("无响应");
    let tools_json = serde_json::to_value(&frame).unwrap();
    let entries = tools_json["result"]["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let tools: Vec<ToolSchema> = entries
        .iter()
        .filter(|t| t["kind"] == "tool" && t["policy"] == "UserAndModel")
        .map(|t| ToolSchema {
            name: t["entry"].as_str().unwrap_or_default().replace("::", "__"),
            description: t["description"].as_str().unwrap_or_default().to_string(),
            input_schema: t["params"].clone(),
        })
        .collect();
    eprintln!("real tool count: {}", tools.len());
    // 最小用例（不带历史），模式由 REPRO_MODE 控制：
    // none / one_reasoning / per_call / between_calls / thinking_off
    let mode = std::env::var("REPRO_MODE").unwrap_or_else(|_| "per_call".into());
    eprintln!("REPRO_MODE={mode}");
    let reasoning_text = "用户上传了多张图片让我查看。我应该逐个调用 vision__read 读取图片内容，然后根据每张图的内容给出解释。先读取第一张、第二张、第三张。".repeat(20);
    let make_reasoning = |id: &str| -> Message {
        let mut r = Message::system("占位");
        r.kind = mistake_agent::kernel::message::MessageKind::Reasoning {
            id: id.into(),
            text: reasoning_text.clone(),
        };
        r
    };
    let rid = "dd560ca3-b90a-4da3-b2a5-379930ecef71";
    let mut messages = vec![Message::system(
        "你是 Mistake Agent，一个面向中学生的学习助手。",
    )];
    messages.push(Message::user("把这几张图都看看"));
    let call1 = Message::tool_call_with_id(
        "vision::read",
        serde_json::json!({"file": "/tmp/mistake-agent-repro-1.png"}),
        Ok(serde_json::json!({"description": "第一张图：Linux 系统信息截图"})),
        "call_00_Fq6VcHpM9fKlTr7oPRkm2990".into(),
    );
    let call2 = Message::tool_call_with_id(
        "vision::read",
        serde_json::json!({"file": "/tmp/mistake-agent-repro-2.png"}),
        Ok(serde_json::json!({"description": "第二张图：游戏截图"})),
        "call_01_vzpFxQLut2CIdsQrAmFw2881".into(),
    );
    match mode.as_str() {
        "none" | "thinking_off" => {
            messages.push(call1);
            messages.push(call2);
        }
        "one_reasoning" => {
            messages.push(make_reasoning(rid));
            messages.push(call1);
            messages.push(call2);
        }
        "per_call" => {
            messages.push(make_reasoning(rid));
            messages.push(call1);
            messages.push(make_reasoning(rid));
            messages.push(call2);
        }
        "between_calls" => {
            messages.push(call1);
            messages.push(make_reasoning(rid));
            messages.push(call2);
        }
        other => panic!("unknown mode {other}"),
    }
    let req = ModelRequest {
        model: ModelKind::Main,
        messages,
        tools: Some(tools),
        reasoning_effort: if mode == "thinking_off" {
            Some("none".into())
        } else {
            None
        },
        response_format: None,
        tool_choice: None,
    };
    let mut stream = svc.stream(&req, &AbortSignal::new()).await.expect("stream");
    let mut err = None;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(ModelChunk::TextDelta(d)) => {
                eprintln!("TEXT {:?}", d.chars().take(40).collect::<String>())
            }
            Ok(ModelChunk::ReasoningDelta(d)) => {
                eprintln!("REASON {:?}", d.chars().take(20).collect::<String>())
            }
            Ok(ModelChunk::ToolCallStart { index, name, .. }) => {
                eprintln!("TOOL_START {index} {name}")
            }
            Ok(ModelChunk::ItemDone { kind }) => eprintln!("ITEM_DONE {kind:?}"),
            Ok(ModelChunk::Done) => eprintln!("DONE"),
            Ok(_) => {}
            Err(e) => {
                eprintln!("STREAM ERR {e:?}");
                err = Some(e);
                break;
            }
        }
    }
    assert!(err.is_none(), "回传失败：{err:?}");
    eprintln!("回传成功");
}
