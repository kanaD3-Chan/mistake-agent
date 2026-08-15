# 用户插件开发手册（so-lite-agent）

面向「给使用 so-lite-agent 的 Agent 写业务插件」的开发者。用户插件提供业务能力，只能通过注册时声明（`requires`）拿到的受限服务句柄访问内核能力。

## 1. 快速开始（三步）

1. 复制 `reference/user-plugin/mod.rs` 到你的 crate（如 `src/plugins/myplugin.rs`）；
2. 编辑 `namespace`（全局唯一）、`requires`、`tools/commands/events` 与 handler；
3. 在 `KernelBuilder` 里注册：`.register_plugin(PluginDescriptor::from_plugin::<MyPlugin>())`。

## 2. 两段式契约（UserPlugin）

```rust
pub struct MyPlugin;
impl UserPlugin for MyPlugin {
    fn info() -> Info { /* 静态元数据 */ }
    fn register(ctx: PluginContext<'_>) -> Result<(), PluginError> { /* 绑定 handler */ }
}
```

`ctx.handles` 只包含 `requires` 声明过的服务；`ctx.registrar.tool("短名", handler)` 只允许登记 `info` 里声明过的短名。

## 3. 入口点与 CallerPolicy

- Tool：`UserAndModel` 或 `UserOnly`（后者不出现在模型工具列表，调度层再拒一次）。
- Command：恒为 `UserOnly`，使用方 GUI 经 `trigger_command` 触发。
- Event：kernel 生命周期回调。
- 模型可见 wire name：`namespace::tool` → `namespace__tool`。

## 4. 参考

- 参考模板：[reference/user-plugin/mod.rs](./reference/user-plugin/mod.rs)；
- 内核插件写法：[kernel.md](./kernel.md)；
- 契约与注册表：`src/contract.rs`、`src/registry.rs`。
