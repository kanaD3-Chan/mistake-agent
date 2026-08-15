# so-lite-agent

通用轻量 Agent 运行时：agent loop、工具注册/调度、消息树、事件流、会话存储抽象、模型 Provider 抽象与通用 RPC，`cargo add` 后即可开发自己的 Agent。

> 状态：M2-M4 已落地（对应 [docs/plan/so-lite-agent.md](../docs/plan/so-lite-agent.md)）：默认服务为 `InMemorySessionStore` 与 `MockModelService`；M3 内置 OpenAI 兼容/Responses/Anthropic 适配器与 `register_provider()`；M4 插件手册已随 crate 提供（`docs/plugin-dev/`），并有内核/用户插件双注册验收测试。M5 的 crates.io 实际上传与 mistake-agent 切换待办。

## 快速验证

```bash
cargo run --example hello
cargo test
```

`examples/hello.rs` 注册一个 `hello::hi` 用户插件，用默认 mock 模型跑通一个完整回合。

## 最小用法

```rust
let kernel = KernelBuilder::new()
    .event_sink(events)
    .register_plugin(my_business_plugin())
    .system_prompt(|| agent_system_prompt())
    .build()
    .await?;
```

内核插件与用户插件由使用方编写；业务服务（存储、记忆、验算、双模型等）不随本 crate 分发。

## Provider

```rust
so_lite_agent::model::register_provider("my_provider", factory)?;
let svc = so_lite_agent::model::build_provider("responses", api_url, api_key, model)?;
```

内置 `openai`（Chat Completions）、`responses`（DeepSeek Responses）、`anthropic`（Messages）。
