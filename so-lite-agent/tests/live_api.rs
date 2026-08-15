//! M3 真实 API 验收（ignored）：`SO_LITE_API_URL` / `SO_LITE_API_KEY` / `SO_LITE_MODEL` 配置后运行：
//! `cargo test --test live_api -- --ignored`

use so_lite_agent::message::Message;
use so_lite_agent::model::build_provider;
use so_lite_agent::services::{AbortSignal, ModelKind, ModelRequest};

#[tokio::test]
#[ignore]
async fn deepseek_responses_turn() {
    let url =
        std::env::var("SO_LITE_API_URL").unwrap_or_else(|_| "https://api.deepseek.com".into());
    let key = std::env::var("SO_LITE_API_KEY").expect("缺少 SO_LITE_API_KEY");
    let model = std::env::var("SO_LITE_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".into());
    let svc = build_provider("responses", &url, &key, &model).expect("responses provider 构建失败");
    let request = ModelRequest {
        model: ModelKind::Main,
        messages: vec![Message::user("回复：ok")],
        tools: None,
        reasoning_effort: Some("none".into()),
        response_format: None,
        tool_choice: None,
    };
    let response = svc
        .complete(&request, &AbortSignal::new())
        .await
        .expect("DeepSeek 真实 API 回合失败");
    assert!(
        response.text.contains("ok") || response.text.contains("OK"),
        "回复应包含 ok：{}",
        response.text
    );
}
