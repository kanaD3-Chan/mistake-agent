# Mistake Agent v2 — API 文档（standalone 单文件）

> 本文档自包含：GUI ↔ kernel 的 RPC 协议、内核入口点/服务契约、真实模型 API 对接方式与验收命令。代码引用均为仓库内文件名，细节以代码为准（ADR 见 docs/adr/）。

## 1. 架构总览

单进程（standalone）：kernel 直接运行在 Tauri GUI 进程内，无 sidecar 依赖；UI 为 Vue 3（web/src，Vite 构建到 web/dist 由 Tauri 嵌入加载）：

```
┌─ mistake-agent（Tauri GUI 进程）──────────────────────────┐
│  web/ 静态 UI ── Tauri Channel/命令桥接 ── Kernel（进程内） │
└───────────────────────────────────────────────────────────┘
```

- GUI → kernel：前端经 `kernel_send` 提交**请求帧**（`RpcRequest`，见 `src/kernel/agent/rpc/protocol.rs`），进程内 mpsc 通道投递。
- kernel → GUI：`Kernel::handle` 返回**响应帧**（带 id 回执），`EventSink`（ChannelEventSink）发**事件帧**（无 id 播报，`Event`），统一经 Tauri `Channel<String>` 推给前端。
- Tauri 侧桥接（src/main.rs 的 Tauri 命令，非 RPC 方法）：`start_kernel`（进程内创建 Kernel + 请求循环）、`kernel_send`（投递一行 JSONL 请求）、`pick_homework_file`（rfd 文件对话框，返回作业路径）。
- 通信格式：JSON Lines（每行一个完整 JSON 对象），UTF-8。
- 无 sidecar：kernel 直接运行在 GUI 进程内（standalone，ADR-0029），协议帧格式与早期 sidecar 时代一致、前端零改动。

## 2. 帧格式（GUI ↔ kernel）

### 2.1 请求帧（GUI → kernel）

```json
{"id": 1, "method": "send_user_message", "text": "你好"}
```

`RpcRequest { id: u64, #[serde(flatten)] method: Method }`——方法参数平铺在顶层，没有 `params` 包装。

| method | 参数 | 状态 | 说明 |
|---|---|---|---|
| `send_user_message` | `text: string`, `force_tool?: {entry, hint?, display?}`, `file?: string[]`, `asset?: {path,name}[]` | ✅ M1 | 开新回合；`force_tool` = 显式工具调用：强制 LLM 首轮调用指定工具（tool_choice + 全程 thinking=none），输出仍由 LLM 生成；`display` = 前端原始展示文本，落盘为 user 消息的 `display_text`（模型上下文仍用拼好的指令 `text`）；`file` = 暂存路径列表（模型读图/判分作 file 参数，可多文件），`asset` = 持久副本列表（落进消息文本供前端展示附件，路径不出现在展示文本） |
| `trigger_command` | `entry: string`, `params: object` | ✅ M1 | 唯一命令通道，校验 EntryPoint + CallerPolicy |
| `abort` | — | ✅ M1 | 停止当前回合（SIGTERM → 宽限 → SIGKILL） |
| `get_state` | — | ✅ M1 | 返回 `{status: idle\|busy, session_key}` |
| `edit_message` | `message_id`, `text` | ✅ M5 | 消息树编辑：仅 user 消息可编辑，从被编辑消息的父节点派生新分支，返回 `{session_key, messages}`（新活跃路径）；编辑 = 改完重发，保存后自动开启新一轮回答 |
| `switch_branch` | `message_id` | ✅ M5 | 消息树切分支：设置 active_path，返回 `{session_key, messages}` |
| `get_settings` | — | ✅ M2/M5 | 返回设置公开视图（**不含 api_key**，只含 `key_set` 标记；含 `english_mode`） |
| `set_settings` | `patch` | ✅ M2/M5 | 应用设置补丁并持久化（含 `english_mode`）；模型配置变化时热替换双模型服务；成功后发 `settings_changed` 事件 |
| `list_sessions` | — | ✅ M5 | 返回 `{sessions:[{key,goal,status,created_at,last_activity_at}]}` |
| `read_session` | `key` | ✅ M5 | 返回 `{meta,messages}`（会话历史/消息树完整记录） |
| `compute_result` | `compute_id`, `stdout`, `stderr`, `duration_ms` | ✅ M4 | GUI/Pyodide 验算回执（compute 桥接）；`compute_id` 必须回填事件 `compute_request` 的 id |

> Tauri 侧命令（GUI 专属，见 src/main.rs）：`start_kernel`、`kernel_send`、`pick_homework_file`；前端经 `@tauri-apps/api` 的 `invoke` 调用（web/src/composables/useKernel.js）。
> `pick_homework_file` 返回 `{temp_path, asset_path, name}`：`temp_path` 是系统临时目录暂存（kernel 白名单，处理后删除），`asset_path` 是数据根目录 `uploads/` 的持久副本（Tauri asset 协议展示用，不随 temp 删除）。

### 2.2 响应帧（kernel → GUI）

```json
{"type":"response","id":1,"result":{"accepted":true}}
{"type":"response","id":1,"error":{"code":"turn_in_progress","message":"当前有回合在跑，请先停止再发送新消息"}}
```

`result` 与 `error` 二选一。错误码：`turn_in_progress` / `scheduler_error` / `tool_error` / `settings_error` / `not_implemented`。

### 2.3 事件帧（kernel → GUI，无 id）

```json
{"type":"event","event":{"event":"message_delta","message_id":"...","delta":"你"}}
```

| event | 负载 | 说明 |
|---|---|---|
| `message_delta` | `message_id`, `delta` | 打字机增量（气泡 = 一个输出 item，完成即落盘） |
| `reasoning_delta` | `delta` | 思维链增量（UI 默认折叠，点击展开） |
| `tool_start` / `tool_end` | `entry`, `ok?` | 工具生命周期 |
| `tool_progress` | `entry`, `message` | 长任务进度（如"正在识别第 3/12 页"） |
| `compute_request` | `id`, `code` | kernel → GUI：请求在 Pyodide 执行端运行 Python，GUI 回 `compute_result` |
| `turn_end` | `stop_reason` | `natural` / `tool_call_limit` / `consecutive_failures` / `turn_timeout` / `user_aborted` / `failed` / `internal_abort`；`failed` 表示回合失败，前端恢复可聊天状态 |
| `session_switched` | `from`, `to` | 会话切换（内部键，UI 不展示） |
| `memory_changed` | `path` | 记忆变更 |
| `compaction` | `session` | 上下文压缩 |
| `error` | `message` | 错误播报 |

## 3. 内核入口点契约

### 3.1 两段式插件契约（`src/kernel/context.rs`、`src/kernel/registry/`）

```rust
pub trait UserPlugin {
    fn info() -> Info;                                        // 静态元数据
    fn register(ctx: PluginContext<'_>) -> Result<(), PluginError>; // 绑定 handler
}
```

- `Info`：`namespace`（全局唯一）、`requires`（能力声明）、`load`（eager/lazy，默认 lazy）、`tools`/`commands`/`events`。
- `ToolDef`：短名 + `user_visible`（是否出现在用户功能中心，默认 true；false = 仅模型可调，如 demo::hello / compute::verify）+ `title`（用户显示名）+ `group`（功能分组，如"批改/学习/记忆"）+ 描述 + `Schema`（schemars，参数 description 即前端表单中文标签）+ `CallerPolicy`（`user_and_model` / `user_only`）+ 可选 `timeout`（秒）。
- 启动时 fail-fast 校验：namespace 唯一、全名跨 kind 唯一、wire name 全局唯一、requires 可满足、CallerPolicy 合法。
- lazy 插件首次命中任一入口时才执行 `register`；`EntryRegistrar` 只允许登记 info 声明过的短名。
- `list_tools` 只返回 `user_visible = true` 的入口点；`model_tools` 不受影响（模型仍可调用不可见工具）。
- **前端展示元数据唯一事实源是 `list_tools`**：标题/分组/图标/描述/参数 schema 全部由后端下发，前端不得硬编码工具名 → 标题/图标映射（web/src 里不允许维护工具表；不可见工具缺失元数据时回退显示 entry 名即可）。

### 3.2 命名：内部全名 vs wire name

- 内部全名：`namespace::tool`（如 `grading::upload`），用于注册表、审计、`trigger_command`。
- 模型可见名（wire name）：`::` → `__`（`grading__upload`），因为 Responses API 要求函数名匹配 `^[a-zA-Z0-9_-]+$`。注册时校验 wire name 全局唯一保证一一对应，模型回包经 dispatch 映射回全名（src/kernel/contract.rs `full_to_wire`，src/kernel/agent/dispatch.rs `resolve_wire`）。

### 3.3 当前入口点

| 全名 | 类型 | 策略 | 说明 |
|---|---|---|---|
| `demo::hello` | tool | user_and_model | 链路自检 |
| `vision::read` | tool | user_and_model | `{file: 路径}` 图片理解：作业/试卷转写文字，角色/照片等描述内容；不判分不归档；上传后模型先调它理解内容，再根据内容与用户意图决定下一步（判分走 grading::upload） |
| `grading::upload` | tool | user_and_model | 场景一：`{file: 路径}` 图片(png/jpg/jpeg/webp/bmp)或文本型 PDF |
| `grading::list` | tool | user_and_model | `{subject?, knowledge_point?}` 列出错题本 |
| `grading::get` | command | user_only | `{id}` 获取单条错题详情，软删除后返回不存在 |
| `grading::update` | command | user_only | `{id, subject?, knowledge_point?, question?, student_answer?, reference_answer?, analysis?, is_correct?, pinned?}` 单题编辑、置顶/取消置顶、标记已掌握 |
| `grading::remove` | command | user_only | `{id}` 软删除单条错题 |
| `grading::remove_many` | command | user_only | `{ids: [uuid]}` 按 id 列表批量/全选软删除 |
| `memory::save` | tool | user_and_model | `{filename?, content?}` 保存记忆条目（可选参数；content 缺省时模型应总结当前会话要点填入） |
| `memory::show` | tool | user_and_model | `{filename?}` 无参数列出全部条目名，带参数看详情（用法：memory::show <记忆片段>） |
| `memory::remove` | tool | **user_only** | `{filename}` 强制参数，删除整棵子树；仅用户可调，不进模型工具列表 |
| `compute::verify` | tool | user_and_model | `{code}` 在 GUI WebView 内 Pyodide 跑 Python 验算，返回 stdout/stderr/duration |
| `practice::generate` | tool | user_and_model | `{knowledge_point, difficulty?}` 按模板生成变式题（几何题含 diagram_spec） |
| `practice::gaps` | tool | user_and_model | `{subject?, days?, limit?}` 聚合错题本薄弱知识点（错误次数排序 + 建议起点难度 basic/variant/advanced） |
| `practice::check` | tool | user_and_model | `{question, student_answer, reference_answer?, subject?, knowledge_point?, kind?}` 批改练习作答（对拍优先/模型兜底，答错回写错题本） |
| `report::weekly` | tool | user_and_model | `{days?}` 按错题本聚合周复盘（正确率/新增/薄弱知识点） |
| `exam::compose` | tool | user_and_model | `{subjects?, count?, minutes?}` 按薄弱知识点组卷 |
| `tracking::checkin` | tool | user_and_model | `{subject?}` 掌握度统计 + 7/14/30 天重测计划 |

> `grading::get/update/remove/remove_many` 是 UserOnly 命令且 `user_visible=false`，不进入聊天功能中心；由错题本页菜单经 `trigger_command` 调用。

> 会话历史经 RPC `list_sessions` / `read_session` 提供（GUI 会话历史页），不注册为模型工具；模型侧历史路由按需经 memory 或系统提示引导。

> `trigger_command` 找不到同名 Command 时，会回退放行同名 Tool（用户对 UserAndModel/UserOnly 工具均可调），因此 GUI 可直接触发 `grading::list` 等工具。

## 4. 服务契约（`src/kernel/plugin/services/`）

| 服务 | 角色 trait | 注入视图 | 说明 |
|---|---|---|---|
| Storage | `SessionStore` + `MistakeStore` + `AuditSink` + `DomainIo` + `TmpIo` | `StorageHandle`（错题本、附件暂存、运行时数据文件语义面） | 会话/错题/审计；文件持久化（sessions/*.jsonl、mistakes.json、audit.jsonl，10MB 轮转） |
| Memory | `MemoryService`（save/show/remove，remove 删子树） | `MemoryHandle` | 路径类型化校验；文件持久化到数据根目录 memory/（失败回退内存实现） |
| Compute | `ComputeService::run` | `ComputeHandle` | BridgeCompute：经 `compute_request` 事件把代码发给 GUI，等待 `compute_result` 回执；超时/取消由 kernel 侧负责 |
| Model | `ModelService::stream/complete` | `ModelHandle`（仅 complete + 超时/abort/审计） | 路由主/视觉模型；设置变更时经共享持有器热替换，已注册插件的句柄同步生效 |

## 5. 真实模型 API 对接

### 5.1 主模型：DeepSeek Responses API（第一方，ADR-0020）

- Endpoint：`POST https://api.deepseek.com/responses`（无状态：每次请求全量历史，不支持 `previous_response_id`/`conversation`/`store`）。
- 模型：`deepseek-v4-flash`（2026-08 起官方支持；v4-pro 待官方放开）。
- 流式：语义 SSE 事件（`event:`/`data:` 行，空行分隔），结束事件 `response.completed` / `response.incomplete` / `response.failed`，**没有 `data: [DONE]`**（src/kernel/plugin/model/responses.rs `SseParser`）。
- 事件映射：`output_text.delta`→TextDelta、`reasoning_text.delta`→ReasoningDelta、`function_call_arguments.delta`→ToolCallDelta、`output_item.done`→ItemDone（气泡/工具调用边界）、`response.completed`→Usage+Done。
- JSON 严格要求：`text.format` 支持 `json_object` 与 `json_schema`（判分用 json_schema 数组，schema 必须内联扁平、避免 `$defs/$ref`，DeepSeek 端不解析引用）。
- 思考模式默认开启：`reasoning.effort` 可传 `none`（判分用 none 提速）；thinking 下 temperature/top_p 无效。
- 工具：function 名约束 `^[a-zA-Z0-9_-]+$`（wire name）；`parallel_tool_calls` 恒开启（参数被忽略），loop 串行执行。
- 强制工具调用：`tool_choice` 支持 `auto` / `required` / `{type:"function", name}`；**thinking 模式不支持 tool_choice**，强制调用时整回合 `reasoning.effort = "none"`（否则下一轮 API 要求回传 reasoning_text 会协议报错）。
- **thinking 模式 reasoning 回传**：普通回合（thinking 开启）只要发生工具调用，下一轮请求必须把上一轮的推理 item **连同推理文本**回传（`{"type":"reasoning","id":...,"content":[{"type":"reasoning_text","text":...}]}`，另附 `summary` 兜底）；DeepSeek 只消费明文 `content`（并入相邻 assistant 消息），`summary`/`encrypted_content` 不消费。loop 以 `MessageKind::Reasoning` 保存（含 id+text），`messages_to_responses_input` 原样回传。
- **并行调用的 reasoning 复制**：DeepSeek 回放校验要求 thinking 开启时**每个 `function_call` 前都紧跟一条 reasoning item**；模型一次输出可带一个 reasoning + 多个并行调用，回放时 `messages_to_responses_input` 会按调用复制该 reasoning（同 id 同文本，实测必要）。Chat Completions 兼容端忽略推理消息。
- **reasoning 回传兜底**：若请求仍被拒（`reasoning_text must be passed back`），`ResponsesModelService` 自动重试一次：剥离全部 reasoning item + `reasoning.effort=none`（关闭 thinking）。宁可丢思考连续性也不让回合失败；不做 LLM 改写，因为校验要求原样回传。
- 传输兜底：客户端强制 IPv4 本地地址（无 IPv6 环境稳定连通）。

### 5.2 视觉模型：SiliconFlow Chat Completions（仅 OCR，不判分）

- Endpoint：`POST https://api.siliconflow.cn/v1/chat/completions`。
- 模型：`Qwen/Qwen3-VL-32B-Instruct`（settings 可配 `SILICONFLOW_MODEL`）。
- 图片：`content` 数组 `{"type":"image_url","image_url":{"url":"data:<mime>;base64,...","detail":"high"}}` + `{"type":"text","text":"仅转写，不要解题"}`（`src/plugin/grading/core.rs` OCR 流程）。
- PDF：文本型 PDF 用 `pdf-extract` 提取文字；扫描版 PDF 明确报错提示拍照上传。

### 5.3 settings.json（数据根目录 `~/Documents/.mistake-agent/`）

```json
{
  "log_level": "info",
  "english_mode": false,
  "main_model": { "api_url": "https://api.deepseek.com", "api_key": "...", "model": "deepseek-v4-flash", "transport": "responses" },
  "vision_model": { "api_url": "https://api.siliconflow.cn/v1", "api_key": "...", "model": "Qwen/Qwen3-VL-32B-Instruct" }
}
```

环境变量回退：`DEEPSEEK_API_KEY` / `DEEPSEEK_API_URL` / `SILICONFLOW_API_KEY` / `SILICONFLOW_API_URL` / `SILICONFLOW_MODEL` / `MISTAKE_AGENT_LOG_LEVEL`。

## 6. 超时与取消模型（ADR-0022）

- 两级取消：SIGTERM（合作式，handler 自主收尾，宽限 5s）→ SIGKILL（dispatch 掐任务）。
- 三层超时：工具级（ToolDef.timeout，默认 30s）< 回合级（10min）< 活性超时（流式 60s 无增量断）。
- 延期后门：`DeadlineHandle::extend`，受回合预算钳制 + 审计。
- OCR 页级失败：重试 2 次 → 页级错误记结果继续；系统性模型错误直接 `ToolError::model_unavailable` 撂挑子。

## 7. 会话与消息树（ADR-0006/0007）

- `SessionKey` = UUID；守卫模型（生产实现 = 主模型 + guard_prompt 独立调用）在"新消息到达"时决策 continue/update_goal/start_new；**start_new 只在有新消息时触发**，回合结束只允许 continue/update_goal；守卫失败/不确定时默认 continue（存疑即继续）。
- 会话分叉摘要：`start_new` / 空闲超时 / `session::switch` 均树内分叉——当前消息节点下挂「上一会话梗概」摘要节点（生产实现 = 主模型 + summarize_prompt 生成），新用户消息随后，旧分支保留为兄弟版本；摘要节点是模型上下文边界（scope_session_context 从摘要起算）。完整历史经 `session::history` 可查。
- 空闲超时 12h：超时后新消息在同一棵树内分叉出新会话子树（摘要节点开头）。
- 消息气泡：一个输出 item = 一个气泡，**完成即落盘**（含 assistant 回复与工具调用）；中断只丢半截，已完整气泡保留。工具调用气泡只展示状态（工具名 + 完成/失败徽章），通用 JSON/Markdown 返回详情不再渲染；仅 practice 练习卡片、薄弱点列表等交互组件保留结果内容。
- 消息树：`edit_message` 从被编辑消息的父节点派生新消息并更新 active_path（旧分支完整保留）；仅 user 消息可编辑（改完重发，自动重新回答），assistant 等模型消息不可编辑；`switch_branch` 切换 active_path；`read_path` 只读活跃路径，旁支不进入 LLM 上下文。
- `InterruptBus`（内部中断，ADR-0023）：环境变更信号队列（会话切换/目标更新/设置变更/记忆变更/压缩），RPC 回合任务在消息进入后与回合收尾后各消费一次，转成 GUI 事件并写审计。

## 8. 运行与验收

```bash
cd web && npm install && npm run build    # 前端构建（改过 web/ 后必须执行）
cargo test                                 # 单元测试（142 项）
cargo test --test live_api -- --ignored   # 真实 API 验收：hello + samples/ 三套样例
cargo run --bin mistake-agent             # Tauri GUI（Wayland/X11 均可）
```

门禁：`cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test`。

## 9. 代码文件索引

| 文件 | 内容 |
|---|---|
| src/kernel/contract.rs | 入口点元数据、CallerPolicy、ToolError、wire name |
| src/kernel/plugin/services/ | 四服务契约、受控句柄、ServiceHandles、MemoryPath、DomainIo/TmpIo |
| src/kernel/plugin/model/ | Responses API / Chat Completions 适配器、SSE 解析、路由服务 |
| src/kernel/registry/ / context.rs | 注册表校验、两段式契约（UserPlugin + KernelPlugin）、EntryRegistrar |
| src/kernel/agent/dispatch.rs | Caller 检查、jsonschema 校验、两级取消、延期后门 |
| src/kernel/agent/loop_mod/ | agent loop、护栏、气泡完成落盘 |
| src/kernel/agent/session/ | SessionScheduler、LlmTurnDecider、InterruptBus、空闲超时 |
| src/kernel/plugin/storage/ · memory/ · compute/ · session/ | 内核插件（服务实现 + 工具入口）；plugin/mod.rs 聚合内核插件清单（ADR-0035） |
| src/kernel/agent/rpc/ | 帧类型、Kernel 组装与请求路由 |
| src/main.rs | Tauri 壳：进程内 Kernel + Channel 桥接（standalone，唯一二进制） |
| web/ | Vue 3 UI（src/App.vue、composables/useKernel.js，构建产物 web/dist） |
| src/plugin/grading/ | 场景一：上传/OCR/判分/归档（含学科与参考答案） |
| src/plugin/ | 业务用户插件：hello/grading/practice/report/exam/tracking |
| tests/live_api.rs | 真实 API 验收测试 |
| samples/ | 三套作业样例（1 真实照片 + 2 合成卷） |
