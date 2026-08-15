# 用户插件开发手册

面向「给 Mistake Agent 写业务插件」的开发者。用户插件提供业务能力（批改、练习、复盘、组卷、追踪），只能通过注册时声明（`requires`）拿到的**受限服务句柄**访问内核能力，看不到内核内部与文件系统。

## 1. 快速开始（三步）

1. 复制参考模板：把 `reference/user-plugin/` 整个目录复制到 `src/plugin/<你的插件名>/`（目录名 = 模块名，小写蛇形，如 `src/plugin/myplugin/`）；
2. 编辑 `mod.rs`：改 `namespace`（全局唯一）、`requires`（需要哪些服务）、`tools/commands/events` 与 handler；
3. 构建即自动收录：`cargo check` / `cargo build`，无需改 `src/plugin.rs` 或任何聚合文件（ADR-0036）。

> 注意：目录名就是模块名，也决定了工具全名 `namespace::tool`。`namespace` 不要用 `user_demo`（那是模板占位），避免与他人撞车。

## 2. 目录形态与规则

- 一个插件一个文件夹：`src/plugin/<name>/`，入口必须是 `mod.rs`；内部可拆子模块（如 `grading/core.rs`、`practice/templates/mod.rs`）。
- **职责先行**：开发时先规划职责；预计有两个及以上职责时，直接创建对应子模块或同名文件夹，不要先把不同职责堆进 `mod.rs` 再被动拆分。入口 `mod.rs` 只负责插件契约、装配和公共重导出；~400 行只是审查预警线，不是拆分触发条件。
- **目录即插件**：build.rs 扫描一层子目录里存在 `mod.rs` 的文件夹即收录，字母序生成清单。
- **禁用/WIP**：目录根部放一个空文件 `disabled`，整个目录**不参与编译、不注册**；删掉即恢复。写一半的代码可以安全放这里。
- 公共辅助代码**不要**作为 `src/plugin/` 的兄弟目录出现（会被当成插件收录），请放进插件内部或放 `src/` 其他位置。

## 3. 两段式契约（UserPlugin）

每个插件实现 `UserPlugin` 并导出 `descriptor()`：

```rust
pub struct MyPlugin;
impl UserPlugin for MyPlugin {
    fn info() -> Info { /* 静态元数据 */ }
    fn register(ctx: PluginContext<'_>) -> Result<(), PluginError> { /* 绑定 handler */ }
}
pub fn descriptor() -> PluginDescriptor {
    PluginDescriptor::from_plugin::<MyPlugin>()
}
```

### 第一段 `info()`：静态元数据（不执行也能被读取）

| 字段 | 说明 |
|---|---|
| `namespace` | 命名空间，全局唯一；工具全名 = `namespace::tool` |
| `requires` | 需要的服务句柄清单（`ServiceId::Storage/Model/Memory/Compute`），决定能拿到哪些句柄 |
| `load` | `LoadPolicy::Eager`（启动即注册）或 `Lazy`（默认，首次使用才注册） |
| `tools` | 工具定义（模型/用户可调） |
| `commands` | 命令定义（GUI/用户触发，恒为 UserOnly） |
| `events` | 事件定义（kernel 生命周期回调） |

`ToolDef` 关键字段：`name`（短名）、`user_visible`（是否出现在用户功能中心）、`title`/`group`（展示用）、`description`（给模型看）、`params`（schemars 生成的 JSON Schema）、`policy`（CallerPolicy）、`timeout`（工具级超时秒数）、`icon`（Iconify 图标名）。

### 第二段 `register()`：绑定 handler

`ctx.handles` 只包含 `requires` 声明过的服务（受控句柄）；`ctx.registrar.tool("短名", handler)` 只允许登记 `info` 里声明过的短名（声明与实现一致，`UndeclaredEntry` 兜底）。handler 签名：

```rust
Arc::new(|call_ctx: &ToolCallContext, params: Value| Box::pin(async move { ... }))
```

`call_ctx.english_mode` 提供英语练习模式开关；插件需要生成模型提示时应据此选择语言（ADR-0043）。

## 4. 服务句柄（能力边界）

| 句柄 | 可见能力 | 用途示例 |
|---|---|---|
| `StorageHandle` | 错题本五操作（save/get/list/update/remove）**+ 附件暂存读写 + 教学数据文件读写**（见下） | grading 归档、vision 读图、practice 真题池 |
| `ModelHandle` | 带超时/abort/审计的 `complete` | OCR、判分、组卷 |
| `MemoryHandle` | 记忆 save/show/remove | 跨会话记忆 |
| `ComputeHandle` | Python 验算 `run` | 数学验算 |

句柄过滤是结构性的：没在 `requires` 声明就**拿不到**，运行时没有检查可绕。

### 磁盘 IO 铁律（ADR-0042）

**插件不持有任何文件句柄**——需要读写文件时，只能在 `requires` 里声明 `ServiceId::Storage`，经 `StorageHandle` 的语义方法调用（白名单校验、原子写、审计都在 storage 实现内，插件不可绕过）：

| 方法 | 对应能力 | 用途 |
|---|---|---|
| `read_staged(path)` / `remove_staged(path)` | 系统 temp 附件暂存（`mistake-agent-` 前缀白名单） | vision 读图、grading 判分后清理暂存 |
| `read_data_file(name)` / `write_data_file(name, content)` | 数据根目录 `data/` 教学数据文件（原子写） | 真题池 `gaokao_pool.json`、先验依赖表 |

路径安全由 storage 保证（`RelPath` 类型校验 + canonicalize 兜底），插件传的是**文件名/暂存路径字符串**，永远不拼接真实路径、不触碰文件系统 API。不要试图用 `std::fs` 自己读文件——那正是铁律禁止的，且会被评审拦下。

## 5. 入口点与 CallerPolicy

- **Tool**：模型可调 + 用户可调（`UserAndModel`），或仅用户可调（`UserOnly`，不出现在模型工具列表、调度层再拒一次——双墙）。
- **Command**：恒为 `UserOnly`，GUI 经 `trigger_command` 触发；找不到 Command 时回退放行同名 Tool。
- **Event**：kernel 生命周期回调，不对外暴露。
- 模型看到的工具名是 wire name：`namespace::tool` → `namespace__tool`（`::` 变 `__`，双下划线；插件内部的下划线不会与分隔符撞名）。
- **前端展示元数据唯一事实源是 `list_tools`**：新增/修改工具后前端零改动，标题、分组、图标、描述、参数标签全部由后端 `Info` 下发；不要在前端维护工具名 → 标题/图标的映射（web/src/lib/messages.js 等渲染库也不例外），渲染时缺失元数据回退显示 entry 名即可。

## 6. 注册校验（启动时 fail-fast）

注册表按序校验：namespace 唯一 → 用户插件不得声明 `provides` → `requires` 可满足 → 插件内入口名不重复 → wire name 全局唯一（跨用户/内核插件）。常见错误：

- `NamespaceTaken`：namespace 或目录名撞车；
- `CapabilityUnavailable`：`requires` 声明了不存在的服务；
- `UndeclaredEntry`：register 里登记了 info 没声明的短名；
- `WireNameCollision`：两个入口映射到同一个 wire name（如 `a::b__c` 与 `a__b::c`）。

## 7. 参考

- 参考模板：[reference/user-plugin/](./reference/user-plugin/mod.rs)（有编译锚定测试保证与契约同步）；
- 内核插件（特权子系统）写法：[kernel.md](./kernel.md)；
- 契约与 RPC 细节：[docs/api.md](../api.md)；服务契约：`src/kernel/plugin/services/`。
