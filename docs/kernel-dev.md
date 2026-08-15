# Kernel 开发手册

本文面向开发 Mistake Agent 内核的人，描述当前 `src/kernel/` 的模块职责、启动装配、调用链、扩展方式和验证要求。用户插件的入口点写法见 [插件开发手册](./plugin-dev/user.md)，内核插件的注册写法见 [内核插件手册](./plugin-dev/kernel.md)。

## 1. 内核定位

Kernel 是本地 Agent 的特权调度层，不实现批改、练习、报表等业务。它负责：

- Agent loop：组织模型请求、流式输出、工具调用、停止护栏和上下文压缩；
- Dispatch：执行入口点，统一做 CallerPolicy、schema、超时、取消和审计；
- Session scheduler：管理 SessionKey、Goal、消息树分叉、空闲超时和会话交接；
- Registry：注册 KernelPlugin/UserPlugin，校验入口点和服务能力；
- RPC：GUI 与 kernel 的唯一通信面；
- 内核插件：storage、memory、model、compute，以及 session 工具入口。

Kernel 不直接调用用户插件之间的内部函数。用户插件通过工具结果、事件流和受限服务句柄协作。

## 2. 模块地图

```text
src/kernel/
├── agent/
│   ├── dispatch.rs                 统一工具/命令执行
│   ├── loop_mod/                   Agent loop
│   │   ├── mod.rs                  主循环与 AgentLoop
│   │   ├── turn.rs                 回合类型与停止原因
│   │   └── tests.rs
│   ├── rpc/                        GUI ↔ kernel RPC
│   │   ├── mod.rs                  Kernel 组装与启动
│   │   ├── protocol.rs             Method/RpcFrame/RpcRequest
│   │   ├── handlers.rs             RPC 方法处理
│   │   └── tests.rs
│   └── session/                    Session scheduler
│       ├── mod.rs                  公共类型与重导出
│       ├── scheduler.rs            生命周期、Goal、分叉
│       ├── guard.rs                LLM 决策与重试
│       ├── summarize.rs             交接摘要
│       ├── interrupt.rs             InterruptBus
│       └── clock.rs                 时钟抽象
├── plugin/
│   ├── services/                   公共服务契约与句柄
│   │   ├── mod.rs                  ServiceId/ServiceHandles/重导出
│   │   ├── model.rs
│   │   ├── storage.rs
│   │   ├── memory.rs
│   │   └── compute.rs
│   ├── storage/                    会话、错题、审计和文件 IO
│   │   ├── mod.rs                  插件身份与公共导出
│   │   ├── core/                   AnyStorage 路由
│   │   ├── file/                   文件后端
│   │   └── mem.rs                  内存后端
│   ├── memory/                     记忆服务
│   ├── model/                      Responses/Chat Completions/路由
│   ├── compute/                    Pyodide bridge
│   └── session/                    session::switch 工具入口
├── registry/                       注册表与插件描述
├── audit.rs                        AuditRecord/Auditor
├── bootstrap.rs                    数据根目录初始化
├── context.rs                      PluginContext/KernelContext
├── contract.rs                     Info/EntryPoint/CallerPolicy
├── events.rs                       kernel → GUI 事件
├── message.rs                      Message/MessageKind/消息树辅助
├── prompt.rs                       系统提示与模型提示
└── settings.rs                     settings.json 与热更新配置
```

`mod.rs` 只负责公共面、装配和 `pub use` 重导出。职责实现放子模块；子模块之间共享的私有项经父模块 `pub(crate) use` 桥接。

## 3. 启动装配顺序

入口是 `Kernel::new(events)`（`agent/rpc/mod.rs`）。启动顺序不能随意交换，因为服务之间存在依赖：

1. 加载 `settings.json`，确定数据根目录和模型配置；
2. `bootstrap::init_data_root` 幂等创建 `sessions/`、`mistakes/`、`memory/`、`audit/`、`logs/`、`uploads/`、`data/`；
3. 初始化诊断日志；
4. 打开 `FileStorage`，失败时回退 `MemoryStorage`；
5. 创建 `FileMemoryService`，执行旧记忆布局迁移，失败不阻塞启动；
6. 创建 compute bridge、主模型服务、视觉模型服务和模型路由；
7. 用 `ServiceHandles` 装配四个服务句柄；
8. 创建 `Registry`，先注册内核插件，再注册用户插件；
9. 创建 `Dispatch`、共享 `InterruptBus`、`SessionScheduler` 和 `AgentLoop`；
10. 返回可接收 RPC 的 `Kernel`。

新增一个需要启动依赖的内核服务，应在 `Kernel::new` 完成实例化和注入，再通过 `KernelPlugin` 暴露入口。不要在 handler 第一次调用时偷偷创建全局单例。

## 4. 插件注册与能力边界

### 4.1 两段式契约

插件先通过 `info()` 声明元数据，再由 `register(ctx)` 绑定 handler：

```rust
impl KernelPlugin for MyPlugin {
    fn info() -> Info {
        Info {
            namespace: "my_kernel".into(),
            provides: vec![],
            ..Default::default()
        }
    }

    fn register(ctx: KernelContext<'_>) -> Result<(), PluginError> {
        // ctx.handles 是内核插件可见的全量服务句柄。
        Ok(())
    }
}
```

`UserPlugin` 通过 `requires` 获取过滤后的句柄，不能填写 `provides`。`KernelPlugin` 在信任边界内，收到全量句柄，作为服务提供者不使用 `requires`。

### 4.2 Registry 校验

注册表会 fail-fast 校验：

- namespace 唯一；
- KernelPlugin 的 `ServiceId` 不重复；
- UserPlugin 不得声明 `provides`；
- `requires` 中的服务可用；
- info 声明的入口点与 register 实际绑定一致；
- 内部全名 `namespace::tool` 唯一；
- 模型 wire name（`::` → `__`）全局唯一；
- lazy 插件首次解析前不绑定 handler，首次调用时再加载。

构建脚本自动扫描插件目录。`src/kernel/plugin/services/` 是公共契约目录，不是插件，已在 `build.rs` 中明确排除。

## 5. 服务句柄

`ServiceHandles` 是封闭容器，目前包含四类服务：

| ServiceId | 句柄 | 责任 |
|---|---|---|
| `Storage` | `StorageHandle` | 错题本语义面、附件暂存、运行时数据文件 |
| `Memory` | `MemoryHandle` | 记忆路由、事件和审计 |
| `Model` | `ModelHandle` | 带超时、取消和审计的模型调用 |
| `Compute` | `ComputeHandle` | 验算请求与 GUI/Pyodide bridge |

用户插件只拿到 `requires` 声明的句柄。句柄不是底层实现的别名，不应把 `Arc<dyn Service>` 或文件路径泄漏到用户插件。

### 5.1 磁盘 IO

用户插件不能直接使用 `std::fs`。必须经 `StorageHandle` 语义方法：

- `read_staged/remove_staged`：系统 temp 中带 `mistake-agent-` 前缀的暂存附件；
- `read_data_file/write_data_file`：数据根目录 `data/` 内的运行时教学数据。

内核插件需要自己的域内文件能力时使用 storage 提供的 `DomainIo`；附件暂存使用 `TmpIo`。所有实现位于 storage，负责路径校验、canonicalize 兜底、原子写和审计。

`RelPath::parse` 构造即校验：拒绝空段、`.`、`..`、首尾点、反斜杠、冒号和非 ASCII；不做宽松规范化。旧数据迁移使用 `read_legacy/remove_legacy`，该接口只用于启动迁移，仍做宽松路径校验和域根校验。

## 6. Dispatch 调用链

所有模型工具和 GUI 命令都经过 `Dispatch`：

```text
Caller
  → Registry.ensure_tool/ensure_command（懒注册）
  → CallerPolicy 双墙校验
  → JSON Schema 参数校验
  → 超时/取消/回合预算
  → handler(ToolCallContext, params)
  → 结构化 ToolError / JSON 结果
  → AuditRecord::EntryPointCall
```

`ToolCallContext` 提供：

- `AbortSignal`：取消链；
- `DeadlineHandle`：只能在当前回合预算内延长工具截止时间；
- `TurnControl`：请求内部中断；
- `LoggerHandle`；
- `english_mode`：英语练习模式开关，插件据此选择提示词语言；
- `EventSink`：工具进度和 GUI 事件。

工具执行默认串行。新增并发执行前必须先解决工具依赖拓扑、结果回填顺序和审计顺序问题。

## 7. Agent loop

`AgentLoop::run_turn` 是 LLM 唯一决策循环：

1. 回合边界消费 `InterruptBus` 中断并记录审计；
2. 注入系统提示、当前消息、模型可见工具列表；
3. 流式调用主模型；
4. 增量输出 reasoning/token/tool 事件；
5. 串行执行模型产生的工具调用；
6. 将结构化工具结果回填对话；
7. 模型自然停止或触发护栏后返回 `TurnOutcome`。

当前护栏包括：

- 单回合最多 25 次工具调用；
- 相同错误连续达到阈值后停止；
- 单回合总超时；
- 用户取消；
- 模型不可用、传输失败和协议错误分类处理；
- 上下文达到阈值时，在回合边界执行 compaction，原文仍保留。

`session::switch` 是 loop 特殊处理的工具：不走普通用户插件 handler，而是调用 `SessionSwitch`，切换后的后半段消息使用新的上下文边界。当前设计是树内分叉，不新建 `SessionKey`。

## 8. Session scheduler

`SessionScheduler` 是独立的内核级模块，不占 `ServiceId`。

- `SessionKey`：内部路由键；
- `Goal`：当前学习目标；
- Active path：消息树中送入模型的当前路径；
- Session handoff：在当前叶子下挂「上一会话梗概」摘要节点，旧内容保留但不进入新上下文；
- 空闲超时：在回合边界按策略分叉；
- `InterruptBus`：环境变化信号，回合边界消费，不抢占当前工具。

会话切换与消息编辑都采用追加式消息树，不物理截断历史。持久化由 `SessionStore` 负责，scheduler 不直接打开文件。

## 9. RPC 与事件

`agent/rpc/` 是 GUI 唯一通信面。主要请求包括：

- `SendUserMessage`、`TriggerCommand`、`Abort`；
- `GetState`、`ListSessions`、`ReadSession`、`ListTools`；
- `EditMessage`、`SwitchBranch`；
- `GetSettings`、`SetSettings`、`TestConnection`、`CheckBalance`、`GetCacheStats`；
- `ComputeResult`：GUI/Pyodide 回执。

内核向 GUI 输出 `Event`：消息增量、reasoning、工具开始/结束/进度、回合结束、会话切换、审计错误、压缩和缓存统计等。

新增 GUI 能力优先扩展 `Method`/`RpcFrame` 和 handler；不要另开任意文本命令通道。工具/命令触发统一走 `trigger_command` 或现有 RPC 方法。

## 10. 扩展路径

### 新增内核插件

1. 在 `src/kernel/plugin/<name>/` 创建 `mod.rs`；
2. 先规划职责，多个职责直接创建子模块目录；
3. 在 `info()` 声明 namespace/provides/入口点；
4. 在 `Kernel::new` 装配服务实例和依赖；
5. 在 `register()` 绑定 handler；
6. 用 `pub(crate)` 隐藏实现，只从 `services` 暴露稳定契约；
7. 补单测、注册表测试和必要的 live_api 场景。

### 新增用户插件能力

用户插件应只添加业务工具/命令/事件，不直接修改 loop、session 或文件后端。需要内核能力时：

1. 在 `info().requires` 声明最小服务集合；
2. 通过受限句柄调用；
3. 不持有文件句柄、不访问 kernel 私有类型；
4. 入口点遵守 `namespace::tool` 和 CallerPolicy。

### 修改现有内核机制

先定位职责模块：

- 工具执行策略：`agent/dispatch.rs`；
- 模型循环和护栏：`agent/loop_mod/`；
- 会话和消息树：`agent/session/`；
- 协议和组装：`agent/rpc/`；
- 服务契约：`plugin/services/`；
- 文件持久化：`plugin/storage/`；
- 注册校验：`registry/`。

不要把新职责塞进最近修改过的 `mod.rs`。如果一个功能同时涉及两个职责，先拆出职责模块，再实现功能。

## 11. 验证清单

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
cargo test --test live_api -- --ignored
```

涉及模型、协议、存储、会话或插件注册的改动，不能只依赖 mock 单测；必须补真实链路或现有 `live_api` 覆盖。真实 API key 只从本地 settings 读取，测试输出不得打印密钥。

## 12. 设计红线

- 单 crate，不以模块重构为理由新增 crate；
- Kernel 核心不实现业务；
- 用户插件不直接碰文件系统；
- `UserOnly` 工具不进入模型工具列表；
- 所有入口点经 Registry/Dispatch；
- 审计默认全覆盖，敏感值脱敏；
- `mod.rs` 保持薄，职责实现放子模块；
- 设计改变必须同步 CONTEXT.md 或新增 ADR。
