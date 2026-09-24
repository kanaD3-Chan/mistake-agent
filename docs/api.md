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
| `send_user_message` | `text: string`, `display_text?: string`, `force_tool?: {entry, hint?, display?}`, `file?: string[]`, `asset?: {path,name}[]` | ✅ M1 | 开新回合；`force_tool` = 显式工具调用：强制 LLM 首轮调用指定工具（tool_choice + 全程 thinking=none），输出仍由 LLM 生成；`display_text` = 前端展示文本（PDF 正文并入 `text` 时的干净展示）；`asset` = 上传附件（图片存为消息附件引用直入上下文，ADR-0046）；`file` 已弃用 |
| `trigger_command` | `entry: string`, `params: object` | ✅ M1 | 唯一命令通道，校验 EntryPoint + CallerPolicy |
| `abort` | — | ✅ M1 | 停止当前回合（SIGTERM → 宽限 → SIGKILL） |
| `get_state` | — | ✅ M1 | 返回 `{status: idle\|busy, session_key}` |
| `edit_message` | `message_id`, `text` | ✅ M5 | 消息树编辑：仅 user 消息可编辑，从被编辑消息的父节点派生新分支，返回 `{session_key, messages}`（新活跃路径）；编辑 = 改完重发，保存后自动开启新一轮回答 |
| `switch_branch` | `message_id` | ✅ M5 | 消息树切分支：设置 active_path，返回 `{session_key, messages}` |
| `get_settings` | — | ✅ M2/M5 | 返回设置公开视图（**不含 api_key**，只含 `key_set` 标记；含 `english_mode` / `nickname`） |
| `set_settings` | `patch` | ✅ M2/M5 | 应用设置补丁并持久化（含 `english_mode` / `nickname`，后者空串=清空）；模型配置变化时热替换模型服务；成功后发 `settings_changed` 事件 |
| `list_sessions` | — | ✅ M5 | 返回 `{sessions: [SessionMeta]}`（`key` / `goal` / `title?` / `status` / `created_at` / `last_activity_at` / `active_path`；`title` 缺失时省略，见 §7） |
| `read_session` | `key` | ✅ M5 | 返回 `{meta,messages}`（会话历史/消息树完整记录） |
| `create_session` | `carry_summary?: bool`, `goal?: string` | ✅ | 用户手动新建会话（ADR-0044）：归档当前活动会话并开启全新独立 `SessionKey`，返回 `{session_key, archived_session_key, summary_attached}`；`carry_summary` 显式控制是否把旧会话摘要作为新会话首条 system 消息；回合在飞时拒绝（`turn_in_progress`） |
| `open_session` | `key` | ✅ | 用户点击列表切换会话：归档全部活动会话 → 激活 `key`，返回 `{session_key}`；会话不存在报 `scheduler_error`；回合在飞时拒绝（`turn_in_progress`）。切换不算发言，不改 `last_activity_at`（空闲判定依赖它） |
| `rename_session` | `key`, `title` | ✅ | 用户重命名：`title` 两端空白裁掉后写盘，空串 = 清空（下一回合末可由模型重新生成）；返回 `{session_key, title}`（清空时 `title` 为 `null`）；不设回合守卫——改名不影响在飞回合；记审计 `session_renamed` 并（非空时）发 `session_title_updated` |
| `delete_session` | `key` | ✅ | 删除会话（含 JSONL 文件）；若删的是当前活动会话则随后补建一条空会话以保住单 Active 不变量，返回 `{replacement_session_key}`（未补建时 `null`）；记审计 `session_deleted`；回合在飞时拒绝（`turn_in_progress`） |
| `compute_result` | `compute_id`, `stdout`, `stderr`, `duration_ms` | ✅ M4 | GUI/Pyodide 验算回执（compute 桥接）；`compute_id` 必须回填事件 `compute_request` 的 id |
| `get_rules_status` | — | ✅ | 返回 `{loaded: bool, path, reason?, bytes?}`：数据根 AGENTS.md 教学规则加载状态（`reason` = `missing`/`too_large`/`invalid_utf8`，缺失/损坏/超限时系统提示已回退静态文本） |

> Tauri 侧命令（GUI 专属，见 src/main.rs）：`start_kernel`、`kernel_send`、`pick_homework_file`、`open_rules_file`；前端经 `@tauri-apps/api` 的 `invoke` 调用（web/src/composables/useKernel.js）。
> `pick_homework_file` 返回 `{temp_path, asset_path, name}`：`temp_path` 是系统临时目录暂存（kernel 白名单，处理后删除），`asset_path` 是数据根目录 `uploads/` 的持久副本（Tauri asset 协议展示用，不随 temp 删除）。
> `open_rules_file` 用系统默认程序打开数据根目录 `AGENTS.md`（教学规则编辑入口，路径固定不接收用户输入）。

### 2.2 响应帧（kernel → GUI）

```json
{"type":"response","id":1,"result":{"accepted":true}}
{"type":"response","id":1,"error":{"code":"turn_in_progress","message":"当前有回合在跑，请先停止再发送新消息"}}
```

`result` 与 `error` 二选一。主要错误码：`turn_in_progress` / `scheduler_error` / `storage_error` / `tool_error` / `unknown_tool` / `invalid_params` / `branch_error` / `invalid_settings` / `connection_failed` / `not_implemented`。

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
| `session_idle` | `session`, `idle_seconds` | 会话空闲超时提示（ADR-0044）：用户沉寂超过 12h 后再次发言时发出。**仅提示**，不自动切换会话——是否开新话题由用户决定 |
| `session_title_updated` | `session`, `title` | 会话标题已更新（首回合结束后模型生成 / 用户 `rename_session`）：侧栏列表刷新用 |
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
| `grading::upload` | tool | user_and_model | 场景一：`{items:[GradedItem]}` 归档模型读图判分结果（ADR-0046；题干逐字保留） |
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
| Model | `ModelService::stream/complete` | `ModelHandle`（仅 complete + 超时/abort/审计） | 单份 DeepSeek 配置承担对话/调度/图片理解（ADR-0045）；设置变更时经共享持有器热替换，已注册插件的句柄同步生效 |

## 5. 真实模型 API 对接

### 5.1 主模型：DeepSeek Responses API（第一方，ADR-0020）

- Endpoint：`POST https://api.deepseek.com/responses`（无状态：每次请求全量历史，不支持 `previous_response_id`/`conversation`/`store`）。
- 模型：`deepseek-flash`（V4.1-Flash；旧名 `deepseek-v4-flash` 已退役）。
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

### 5.2 图片输入：Responses `input_image`（图片直入上下文，ADR-0045/0046）

- 图片理解不需要独立视觉端点，也没有独立读图工具：上传图片以 `uploads/` 路径引用存进用户消息（`MessageKind::User.attachment_refs`，消息树不落图片字节）。
- 请求构建时 `AttachmentResolvingModelService` 按引用读盘、base64 回填运行时 `attachments`，再在 `messages_to_responses_input` 中展开为 `input_image`：`{"type":"input_image","image_url":"data:<mime>;base64,...","detail":"high"}` + `{"type":"input_text","text":"..."}`（`src/kernel/plugin/model/mod.rs` / `routing.rs`；Chat Completions 回退走 `messages_to_cc` 的 `image_url`）。
- 仅允许出现在 `user`/`developer` 消息与 `function_call_output`；base64 data URL 或 http(s) URL 均可。
- PDF：Responses API 不支持文件输入，文本型 PDF 由 GUI 边界（`stage_files`）用 `pdf-extract` 抽取正文并入消息文本；扫描版 PDF 提示拍照上传。

### 5.3 settings.json（数据根目录 `~/Documents/.mistake-agent/`）

```json
{
  "log_level": "info",
  "english_mode": false,
  "nickname": "",
  "main_model": { "api_url": "https://api.deepseek.com", "api_key": "...", "model": "deepseek-flash", "transport": "responses" }
}
```

`nickname` 是侧栏左下角的称呼（≤24 字符，空串=用前端默认「同学」）。`vision_model` 字段仅为兼容旧配置保留、运行时不再读取（ADR-0045）。环境变量回退：`DEEPSEEK_API_KEY` / `DEEPSEEK_API_URL` / `MISTAKE_AGENT_LOG_LEVEL`。

## 6. 超时与取消模型（ADR-0022）

- 两级取消：SIGTERM（合作式，handler 自主收尾，宽限 5s）→ SIGKILL（dispatch 掐任务）。
- 三层超时：工具级（ToolDef.timeout，默认 30s）< 回合级（10min）< 活性超时（流式 60s 无增量断）。
- 延期后门：`DeadlineHandle::extend`，受回合预算钳制 + 审计。
- OCR 页级失败：重试 2 次 → 页级错误记结果继续；系统性模型错误直接 `ToolError::model_unavailable` 撂挑子。

## 7. 会话与消息树（ADR-0007/0044）

- `SessionKey` = UUID。**会话新建只由用户发起**（ADR-0044）：经 `create_session` RPC 归档当前活动会话并开启独立 `SessionKey`。没有任何模型侧的自动判断——`SessionScheduler::on_new_message` 的预决策（原 ADR-0032）、回合末 `LlmTurnDecider`（原 ADR-0030）、`session::switch` 工具（原 ADR-0034）三处已一并删除，`GuardModel` / `turn_decider_prompt` 随之退役。
- **单 Active 不变量**：任一时刻至多一个 `status == active` 的会话；新建会话（`create_session`）与切换会话（`open_session`）都归档**全部** Active 会话（`MemoryStorage::list_sessions` 遍历 HashMap 顺序不定，两个 Active 会让"当前会话"变成随机）；删除活动会话时补建一条空会话。存量迁移同样不新增 Active（见下）。
- 会话标题：`SessionMeta.title`（`Option<String>`，旧 JSONL 无此字段仍可解析）= 侧栏展示的**用户可见标题**，与 `goal`（学习目标，摘要器输入）语义分离。首回合落盘 + `TurnEnd` 之后由独立任务异步生成（`LlmTitler` + `session_title_prompt`，≤12 字；<8 条等条件不满足则跳过），失败降级为首条用户消息前 40 字；用户可经 `rename_session` 覆盖，已有标题时不再调模型（用户改名不会被下一回合盖掉）。生成/改名后发 `session_title_updated`。
- 交接摘要：`create_session` 的 `carry_summary` 为真且旧会话有非空内容时，把「上一会话梗概：…」（生产实现 = 主模型 + summarize_prompt 生成，<8 条消息走计数 stub 不调模型）作为**新会话首条** system 消息，后续用户消息挂在其下；旧会话本身不因携带摘要而被写入。
- 空闲超时 12h：检测保留，但**不再自动分叉**——发 `session_idle` 事件提示用户后继续当前会话。
- 消息气泡：一个输出 item = 一个气泡，**完成即落盘**（含 assistant 回复与工具调用）；中断只丢半截，已完整气泡保留。工具调用气泡只展示状态（工具名 + 完成/失败徽章），通用 JSON/Markdown 返回详情不再渲染；仅 practice 练习卡片、薄弱点列表等交互组件保留结果内容。
- 消息树：`edit_message` 从被编辑消息的父节点派生新消息并更新 active_path（旧分支完整保留）；仅 user 消息可编辑（改完重发，自动重新回答），assistant 等模型消息不可编辑；`switch_branch` 切换 active_path；`read_path` 只读活跃路径，旁支不进入 LLM 上下文。分支切换**只在当前会话内**发生，不再承担会话边界语义（ADR-0044）。
- `InterruptBus`（内部中断，ADR-0023）：环境变更信号队列（设置变更/记忆变更/压缩），RPC 回合任务在消息进入后与回合收尾后各消费一次，转成 GUI 事件并写审计。
- **存量数据已迁移**（2026-09-23，[file/migrate.rs](../src/kernel/plugin/storage/file/migrate.rs)）：ADR-0044 之前的会话由模型自动切换话题，多个话题挤在一个文件里、以「上一会话梗概：」系统消息为边界（含兄弟分支）。`FileStorage::open` 在加载会话**之前**扫描 `sessions/*.jsonl`：按边界节点切分并沿 parent 链把后代（含兄弟分支）归入最近的边界祖先 → 每段分配新 `SessionKey`、段首 `parent_id = None`、消息 id 保留 → 仅含老 `active_path` 的段继承老状态（**不新增 Active**），其余 `Archived`；随后把原文件改名为 `<key>.jsonl.bak`（完整字节，可手工回退）。`.bak` 扩展名不是 `.jsonl`，二次启动自然跳过（幂等）；任何一步失败只 `log::warn`，不阻塞启动、保留原文件待下次重试。边界判定明确排除 `上下文压缩摘要：`（压缩节点）与老一代 `交接摘要：`（旧会话尾标记）——二者不是话题边界。**是否拆分按「非空段数 ≥ 2」判定**（段数 = 边界数 + 头部非空则 1）：老数据的第一个话题可能就以边界节点开头，不能据此整篇跳过；而迁移后的每段最多含一个边界节点、必在段首，二次运行天然无可拆之段。归档段的标题取首个用户消息的**可见文本**（`display_text` 优先——forced_tool 消息的 `text` 是给模型的指令，会得到「请调用工具 X 处理当前请求。」这种标题）。迁移前的老会话会把摘要节点与其祖先内容一并送给模型（token 上升），拆分后每条会话只含一个话题。

## 8. 运行与验收

```bash
cd web && npm install && npm run build    # 前端构建（改过 web/ 后必须执行）
cargo test                                 # 单元测试（146 项）
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
| src/kernel/agent/session/ | SessionScheduler、InterruptBus、空闲超时、交接摘要 |
| src/kernel/plugin/storage/ · memory/ · compute/ | 内核插件（服务实现 + 工具入口）；plugin/mod.rs 聚合内核插件清单（ADR-0035） |
| src/kernel/agent/rpc/ | 帧类型、Kernel 组装与请求路由 |
| src/main.rs | Tauri 壳：进程内 Kernel + Channel 桥接（standalone，唯一二进制） |
| web/ | Vue 3 UI（src/App.vue、composables/useKernel.js，构建产物 web/dist） |
| src/plugin/grading/ | 场景一：上传/OCR/判分/归档（含学科与参考答案） |
| src/plugin/ | 业务用户插件：hello/grading/practice/report/exam/tracking |
| tests/live_api.rs | 真实 API 验收测试 |
| samples/ | 三套作业样例（1 真实照片 + 2 合成卷） |
