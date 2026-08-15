# 内核插件开发手册（so-lite-agent）

面向「写内核插件（特权子系统）」的开发者。内核插件运行在信任边界内，注册上下文注入全量服务句柄，并在 `info` 里用 `provides` 声明自己提供的服务。

## 1. 快速开始

1. 复制 `reference/kernel-plugin/mod.rs` 到你的 crate；
2. 编辑 `namespace`，如需提供服务则声明 `provides`（ServiceId）；
3. 在 `KernelBuilder` 里注册：`.register_kernel_plugin(KernelDescriptor::from_plugin::<MyKernelPlugin>())`。

## 2. 两段式契约（KernelPlugin）

```rust
pub struct MyKernelPlugin;
impl KernelPlugin for MyKernelPlugin {
    fn info() -> Info { /* namespace + provides + 入口点 */ }
    fn register(ctx: KernelContext<'_>) -> Result<(), PluginError> { /* 绑定 handler */ }
}
```

`ctx.handles` 直接给全量句柄；`provides` 声明的 ServiceId 至多由一个内核插件提供（重复 → `ServiceTaken`）。

## 3. 注册校验

与用户插件同表：namespace 唯一 → provides 唯一 → 入口名不重复 → wire name 全局唯一。常见错误：`NamespaceTaken`、`ServiceTaken`、`WireNameCollision`、`UndeclaredEntry`。

## 4. 参考

- 参考模板：[reference/kernel-plugin/mod.rs](./reference/kernel-plugin/mod.rs)；
- 用户插件写法：[user.md](./user.md)；
- 公共契约与句柄：`src/services.rs`；注册表：`src/registry.rs`。
