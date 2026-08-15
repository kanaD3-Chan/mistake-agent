# Mistake Agent v2 — 项目总览

> 本文档自包含：只看这一份文件即可了解项目全貌、技术决策与分工方式。详细决策留痕见 `docs/adr/`（43 条 ADR）与 `CONTEXT.md`（术语表），但理解本项目不要求先读它们。

## 1. 项目一句话

面向中学生的**本地智能错题管理 + 辅助学习 Agent**：Windows 桌面应用，双击安装即用，无服务器、无 Docker、数据全本地。形态类似 Codex / Claude Code 那样的本地 Agent，但用户完全不接触命令行——一切通过图形界面完成。

## 2. 背景与动机

- v1 是课程项目（网站服务 + Docker Compose + LangChain），体验差、部署门槛高；答辩已完成，v2 推倒重来。
- 目标用户是中学生：不会用 CLI，所以要 GUI；家长在意隐私，所以本地优先；未来要加功能，所以 kernel-plugin 可扩展。
- 设计蓝本：借鉴 Codex / Pi 的 Agent 架构（agent loop、工具调用、指令文件、事件流、RPC、消息树），但**内核用 Rust 自研**，不依赖任何第三方 Agent 框架。
- **开源借鉴策略：允许直接抄工程机制**。stdio RPC 协议、JSONL 会话格式、工具调用流式解析、Tauri 进程/通道桥接、Pyodide 集成等机制类源码，可直接参考或复制自开源项目（如 openai/codex，Apache-2.0；earendil-works/pi，MIT；其余项目按其 LICENSE 为准），前提是保留原许可证/版权声明并在文档注明来源。业务逻辑（教学流程、错题模型、记忆路由策略）仍由本项目自己写。

## 3. 产品能力（五个场景）

| 场景 | 一句话 | 主要入口 |
|---|---|---|
| 1. 上传作业 / 自动批改 | 图片或 PDF 上传 → 视觉模型理解图片（`vision::read`：作业转写 / 图片描述）→ 模型按内容与用户意图决定：讲解、描述或判分归档 | `vision::read` + `grading::*` |
| 2. 薄弱点定位 / 分层练习 | 基于错题定位知识漏洞 → 基础补漏 → 变式 → 拔高 → 真题 | `practice::generate` / `practice::gaps` / `practice::check` |
| 3. 多周期复盘 | 按日 / 周 / 单元 / 月考 / 学期生成可视化报告 | `report::*` |
| 4. 阶段性考核 | 按薄弱点自动组卷、限时作答、判分、掌握度判定 | `exam::*` |
| 5. 长效追踪 | 知识点图谱、掌握度状态、7/14/30 天强制重测 | `tracking::*` |

辅助能力：`vision::read`（图片理解：作业转写 / 内容描述）、`compute::verify`（数学/物理验算，跑 Python 代码验证答案）、`memory::*`（跨会话记忆）。

## 4. 总体架构：OS 式三层

```
┌─ GUI 壳（Tauri + WebView）───────────────────────────┐
│  聊天 / 消息树 / 上传 / 报告 / 设置向导                │
│  （唯一入口：结构化 RPC，进程内桥接）                  │
└──────────────────────┬──────────────────────────────┘
                       │ RPC（Tauri Channel/命令，同进程）
┌──────────────────────▼──────────────────────────────┐
│ Kernel（内核）— 核心调度                              │
│  agent loop · 工具注册与调度 · 会话/消息树            │
│  指令加载 · 事件流 · 护栏 · 审计 · 懒注册             │
│  会话调度（Session scheduler，独立内核级模块，        │
│  主模型决策：新消息先判断、回合内/末可切换）                 │
├─────────────────────────────────────────────────────┤
│ 内核插件（信任边界内，处理敏感能力）                  │
│  storage 会话/错题/审计 · compute 验算契约             │
│  memory 记忆路由 · model 模型服务（双模型）           │
├─────────────────────────────────────────────────────┤
│ 用户插件（业务，只见受限服务句柄）                    │
│  vision · grading · practice · report               │
│  exam · tracking                                    │
└─────────────────────────────────────────────────────┘
```

### 信任模型（核心设计）

- **内核插件之间**：同一信任边界内，可直接调用类型化接口；插件本身经 `KernelPlugin` 两段式契约注册（info + register，ADR-0035），与用户插件同表校验。
- **用户插件**：只能在注册时通过能力声明（requires）获得受限**服务句柄**（StorageHandle、ModelHandle…），看不到完整服务接口、内核内部或文件系统。
- **用户插件之间**：不直接通信，只通过工具结果和 kernel 事件流协作。
- **调用方向**：kernel 会主动调用用户插件的回调（工具/命令/事件），但能力边界不因调用方向改变。
- **CallerPolicy（调用方策略）**：每个入口点要么 `UserAndModel`（模型可调，用户必可调），要么 `UserOnly`（仅用户可调）。UserOnly 工具**不出现在模型的工具列表**，调度层再拒绝一次（双墙）。不存在"仅模型可调"的工具。
- **配置**：settings.json 用户独占写（App 设置界面）、kernel 独占读；模型和插件没有任何配置访问通道。

## 5. 内核机制（要点）

### Agent loop
单 agent、单 loop：LLM 是唯一决策者，kernel 执行工具调用并保证安全边界。全程向 GUI 输出事件流（token 增量、工具开始/结束、回合结束）。停止条件：模型自然停止 / 工具调用上限（默认 25）/ 相同失败连续 N 次（默认 3）/ 单轮总超时 / 用户取消。不做子 agent（列后续）。

### 工具调用流程
```
CallerPolicy 过滤工具 schema → 流式解析（容灾）→ 调度守卫 + 懒注册
→ 参数校验（必填/类型严格，多余字段宽容）→ handler（超时 + abort）
→ 结构化结果回填 → 审计
```
v2 同一轮多个工具调用**串行执行**；并行列入后续（按依赖拓扑排序）。

**显式 tool-calling（用户发起，不绕过 LLM）**：用户在输入框输入 `namespace::tool`（如 `practice::generate`），前端弹候选框、按 Tab 确认后进入待调用状态（工具徽章 + `<可选参数>` 占位）；发送时 RPC 携带 `force_tool {entry, hint, display?}`（`display` = 前端原始展示文本，落盘为 user 消息的 `display_text`，重开会话后渲染仍友好；模型上下文仍用拼好的指令文本），kernel 开回合并让模型**首轮强制调用该工具**（Responses API `tool_choice`，整回合 `thinking=none`），工具结果回填后由模型继续生成回复——所有内容输出都走聊天框 LLM 侧。工具清单/标题/分组/图标/参数说明/用法示例全部来自 `list_tools`（后端唯一事实源），前端不写死（工具名 → 标题/图标的映射一律不允许在前端维护）。

### 会话与消息树
- 会话调度由独立内核级模块（Session scheduler）承担：任务层由**主模型**决策（ADR-0030/0032）——**新消息到达先判断要不要切换上下文，再进入回合回答**；回合中可经 `session::switch` 工具主动切换；回合结束由 LlmTurnDecider 判断 continue / update_goal / start_new。判断依据是**会话目标（Goal）**（start_new 时主模型生成并写入会话元数据）。三动作：`continue`（目标不变）、`update_goal`（同会话内改写/细化 Goal，如"录入错题"→"讲解已录入的错题"）、`start_new`（仅当新目标明显无关且不依赖当前会话上下文）；偏向规则为**存疑即继续**。**start_new 是树内分叉**：不新建会话，在当前叶子节点下挂一棵「会话子树」（以「上一会话梗概」摘要节点开头，新用户消息随后），旧分支保留为兄弟版本（GUI `< / >` 可切回）；摘要节点同时是模型上下文边界——新会话上下文 = 摘要起算到当前，旧会话内容不进模型上下文；根会话无摘要，直接从首条用户消息开始。带频率护栏（1 小时 5 次），决策失败默认 continue。
- 每次模型请求注入**当前会话 ID**（分叉会话 = 摘要节点 UUID，根会话 = 链首消息 UUID）：会话内保持不变、分叉后变化，模型可据此确认切换是否完成。
- 消息树：每条消息有 id/parentId，JSONL 追加式、永不截断；**编辑消息或"重新生成"会从该点派生新分支**，用户可用 GUI 的 `<` / `>` 翻看旧分支。LLM 上下文只包含活跃路径。
- 会话是过程记录，业务真相在错题本（storage）——对话历史用完即弃，错题数据长期保留。

### 压缩（compaction）
上下文用量达模型窗口 75% 时，在回合边界自动压缩活跃路径：最近 15 条不压，其余旧消息由 LLM 生成任务摘要（保留错题 id、知识点、未完成事项），摘要作为特殊条目写入 JSONL，**原始消息全量保留**。失败重试一次，再失败下回合再试。

**会话切换 = 树内分叉（新版）**：整个历史是一棵消息树，`start_new` / 空闲超时 / `session::switch` 都在**当前消息节点下分叉出一棵会话子树**——以「上一会话梗概」摘要节点开头（摘要整条链，模型上下文从该节点起算），新用户消息随后；旧分支保留为兄弟版本，GUI `< / >` 即可切换旧会话/新会话。改错别字等消息编辑仍是消息级版本切换，与开会话无关。

### 记忆路由（memory）
记忆是第三个内核插件，按层级路径组织（`学科/知识点/条目`）。工具：`memory::save(path, content)`（模型自动保存，用户也可调）、`memory::show(path?)`（无参数列出全部条目名，带参数看详情）、`memory::remove(path)`（强制参数，**仅用户可调**）。上下文只注入一行入口提示，模型自行浏览（show 无参数 = 列目录）；路径由 memory 插件校验（拒绝越界）。

### 指令加载
数据根目录单文件 `AGENTS.md`（教学规则，家长/老师可编辑）全文进系统提示。**v2 无技能系统**：学习工作流（错因分析、分层出题等）全部内化到 AGENTS.md、系统提示和工具描述。

### 验算运行时（compute）
`compute::verify` 让模型跑 Python 验算（解方程、数值验证、单位换算）。执行端为 GUI WebView 内的 **Pyodide**（Python + SymPy/NumPy 的 WASM 构建），WASM 即沙箱（默认无文件、无网络），经 RPC 桥接；超时、审计由 kernel 侧 compute 插件负责。GUI 离线时验算不可用（可接受）。

### GUI 通信协议
GUI → kernel：`send_user_message`、`trigger_command(entry, params)`、`edit_message`、`switch_branch`、`abort`、`get_state`、`get_settings/set_settings`、`list_sessions`、`read_session`、`list_tools`、`test_connection`、`check_balance`、`get_cache_stats`、`compute_result`（Pyodide 验算回执）。kernel → GUI：事件流（message_delta、reasoning_delta、tool_start/end、tool_progress、turn_end、session_switched、memory_changed、compaction、cache_stats_updated、compute_request、error）。**命令唯一通道是 trigger_command**：GUI 不传可任意执行的文本命令，前端门禁由此结构性成立；找不到同名 Command 时回退放行同名 Tool（用户对 UserAndModel/UserOnly 工具均可调）。

**Standalone（ADR-0029）**：kernel 直接运行在 Tauri GUI 进程内（mpsc + Channel 桥接），mistake-agent 不依赖任何外部进程/二进制；sidecar CLI 已彻底移除（2026-08-05），单二进制即全部交付物。

### 审计与日志
- **审计（Audit）**：默认全覆盖——任何操作都记录（工具调用、消息完成、编辑、会话切换、记忆变更、配置变更、LLM 调用、compute 执行、越权拒绝、生命周期）。写 `audit/` JSONL，记元数据与引用（大内容不复制）；compute 的代码与结果全量记录。10MB 归档轮转。
- **日志（Diagnostic log）**：分级 DEBUG < INFO < WARN < ERROR < CRITICAL < PANIC，写 `logs/`；敏感值脱敏；panic hook 先记 PANIC 再退出，GUI 提示恢复。

## 6. 数据与配置

**数据根目录**：`~/Documents/.mistake-agent`（Windows 上 `~` = `%USERPROFILE%`）。无"项目"概念，一切数据都在此目录：

```
~/Documents/.mistake-agent/
├── AGENTS.md         教学规则（可编辑）
├── sessions/         消息树 JSONL
├── memory/           记忆路由目录
├── mistakes/         错题本（storage 服务）
├── audit/            审计 JSONL
├── logs/             分级日志
└── settings.json     配置（用户独占写）
```

**模型方案（双模型，均 OpenAI 兼容端点）**：

```json
{
  "log_level": "INFO",
  "english_mode": false,
  "main_model":  { "api_url": "https://api.deepseek.com",  "api_key": "...", "transport": "responses" },
  "vision_model":{ "api_url": "https://api.siliconflow.cn/v1", "api_key": "..." }
}
```

- 主模型：deepseek-v4-flash，负责调度与对话（agent loop）；默认经 **DeepSeek Responses API**（`POST /responses`，官方 2026-08 起支持、为 agent 优化）接入，`transport: "responses"`；Ollama 等不兼容端点可配 `"chat_completions"`。
- 视觉模型：硅基流动（SiliconFlow）的 qwen3-VL，负责 OCR / 图片理解（grading 插件经 ModelHandle 调用，模型选择 `Main | Vision`）；Responses API 不支持图片输入，视觉模型固定走 Chat Completions。

**Responses API 速览（详见 ADR-0020）**：

| 项 | 现状 |
|---|---|
| Endpoint | `POST https://api.deepseek.com/responses`（base_url 与 Chat Completions 相同） |
| 模型支持 | 仅 `deepseek-v4-flash`；v4-pro 官方计划 2026-08 初支持 |
| 会话状态 | 无状态：不支持 `previous_response_id`/`conversation`/`store`，每回合发全量历史 |
| 流式 | SSE 语义事件，`response.completed`/`incomplete`/`failed` 结束，无 `data: [DONE]` |
| 思考模式 | 默认开启（`reasoning` 可调 effort）；thinking 下 `temperature`/`top_p` 无效 |
| 工具 | `function` / `web_search`；function 名限 `^[a-zA-Z0-9_-]+$` → 内部 `namespace::tool` 经 wire name 映射（`::`→`__`） |
| 并行工具调用 | 恒开启（参数被忽略）；v2 loop 仍串行执行（ADR-0010） |
| 图片输入 | 不支持（占位替换）→ 视觉模型走 Chat Completions |
| 来源 | [官方指南（英）](https://api-docs.deepseek.com/guides/responses_api/) / [（中）](https://api-docs.deepseek.com/zh-cn/guides/responses_api/)，2026-08-04 核对 |

- 会话切换决策归主模型（LlmTurnDecider：新消息先判断 / 回合末三动作 / `session::switch` 工具）；会话分叉摘要与上下文压缩摘要由 LlmSummarizer 生成（≤300 字，保留错题 id/知识点/未完成事项，模型错误降级为计数摘要）。
- 可选 Ollama 本地模型（离线场景，不填 key）。
- 首次运行由设置向导引导填写。
- 设置热更新：`set_settings` 落盘后双模型服务热替换（LiveSettingsModelService），下一轮模型调用即用新端点/模型/key；settings.json 仍为唯一持久事实。

## 7. 技术栈

| 层 | 选型 |
|---|---|
| 内核 | Rust 2024 edition（单 crate，纯自研） |
| GUI | Tauri（Rust 壳 + WebView2） |
| 通信 | 进程内 RPC（Tauri Channel/命令桥接，standalone 单二进制） |
| 存储 | 本地文件：JSONL（会话/审计/记忆）+ 错题本 JSON |
| 验算 | Pyodide（WASM Python + SymPy/NumPy，跑在 WebView） |
| LLM | 主模型走 DeepSeek Responses API；视觉模型走 OpenAI 兼容 Chat Completions（SiliconFlow / Ollama） |
| 参数 schema | serde + schemars（JSON Schema 派生） |
| 安装包 | Tauri bundler / NSIS（Windows setup.exe） |

## 8. 工程结构（参考）

```
mistake-agent/
├── CONTEXT.md / docs/adr/        ← 术语表与决策留痕（本项目历史）
├── docs/plan/                    ← 后续计划（so-lite-agent 剥离，ADR-0037）
├── Cargo.toml                    ← 单 crate（edition = "2024"，唯一 bin：mistake-agent GUI）
├── src/
│   ├── lib.rs                    ← 库出口（kernel 公开面 + plugin 注册聚合）
│   ├── main.rs                   ← Tauri GUI 入口（进程内 kernel，standalone）
│   ├── kernel.rs                 ← kernel 模块入口（mod kernel;）
│   ├── kernel/                   ← 内核（目录即模块，职责先行）
│   │   ├── agent/                ← Agent 核心调度层
│   │   │   ├── loop_mod/         ← agent loop（主循环/回合类型/测试）
│   │   │   ├── session/          ← 会话调度（guard/summarize/interrupt/scheduler）
│   │   │   ├── rpc/              ← RPC（协议/处理器/组装）
│   │   │   └── dispatch.rs  balance.rs  cache.rs
│   │   ├── plugin/               ← 内核插件层（一插件一文件夹，mod.rs 承载插件 info）
│   │   │   ├── services/         ← 公共契约（model/storage/memory/compute）
│   │   │   └── storage/  memory/  compute/  model/  session/
│   │   ├── registry/             ← 注册表（entry/mod/tests）
│   │   ├── contract.rs  context.rs
│   │   └── events.rs  audit.rs  logger.rs  message.rs  prompt.rs  settings.rs
│   └── plugin.rs                 ← 用户插件入口（mod plugin;）
│       └── plugin/               ← 用户插件（hello/ vision/ grading/ practice/ report/ exam/ tracking/）
├── web/                          ← GUI 前端资源（Tauri 加载）
└── assets/
```

**边界约束**：单 crate 内没有 Cargo 依赖图边界，能力边界靠两层纪律保证：可见性（kernel 只公开 trait 与句柄类型，服务实现与内核内部用 pub(crate) 隐藏）+ 运行时调度（CallerPolicy、句柄注入、注册校验）。用户插件只允许经公开 API 面与内核交互。

**文件组织（开发约定）**：职责先于实现规划；新功能预计存在两个及以上职责时，直接创建同名文件夹与 `mod.rs`，不先堆成单文件再拆。`mod.rs` 只负责公共面、装配与 `pub use` 重导出，职责实现放子模块；子模块间共享的私有项经父模块 `pub(crate) use` 桥接。~400 行只是审查预警线，不是拆分触发条件。已按此拆分的：`agent/session/`（guard/summarize/interrupt/scheduler/clock）、`agent/rpc/`（protocol/handlers）、`agent/loop_mod/`（turn）、`plugin/services/`（model/storage/memory/compute）、`plugin/storage/file/`（mistakes/io/tmp）、`plugin/storage/core/`（chain）、`registry/`（entry）、`plugin/memory/`（store/inmem）、`plugin/practice/templates/`（geometry/algebra/english）、`plugin/grading/`（tests.rs 拆出）。

## 9. 当前状态

- **M1–M6 全部完成**（含 Windows 打包实测：`错题 Agent_0.1.0_x64-setup.exe` 在 Windows 环境安装运行通过），设计文档 43 条 ADR（0001–0043）+ 术语表（CONTEXT.md）。
- **磁盘 IO 铁律 + 数据运行时化落地**（2026-08-10，ADR-0042）：`DomainIo`（数据根目录域内文件：域枚举 + canonicalize 兜底 + 原子写 + 审计）+ `TmpIo`（系统 temp 暂存：`mistake-agent-` 前缀白名单）+ `RelPath`（类型层无目录遍历，fail-closed）；memory 收编（中文路径 base64url 段编码经 DomainIo 落盘）；vision/grading 附件读写、practice 真题池全经 StorageHandle 语义方法（插件零文件句柄）；`data/` 子目录 + 真题池运行时化（`gaokao_pool.json` 文件优先、内置种子兜底，`read_pool_json` 真实链路测试）；verify_geometry.py 维持 include_str!（执行代码非数据）。
- kernel：注册表/两段式契约（用户插件 UserPlugin + 内核插件 KernelPlugin，ADR-0035）/dispatch/loop/RPC/session 调度全链路；四服务全部生产实现——storage（文件持久化：会话 JSONL/错题 JSON/审计 JSONL 轮转）、memory（文件持久化 + MemoryHandle 事件/审计）、model（Responses API + Chat Completions，LiveSettingsModelService 热更新）、compute（BridgeCompute → GUI Pyodide）。
- 构建期插件自动发现（ADR-0036）：插件目录 `mod.rs` 即插件描述、`disabled` 标记即禁用（不编译不注册）；插件开发手册 + 参考模板（docs/plugin-dev/，复制即开工，include! 编译锚定测试保证与契约同步）。
- **ADR-0037 剥离落地中**：M1（本仓库解耦）与 M2（本地独立 crate `so-lite-agent/`）已落地；M3 Provider 层（`register_provider` + 内置适配器）与 M4 插件手册/双插件验收已落地；M5（crates.io 发布与 mistake-agent 切换）待办，详见 [docs/plan/so-lite-agent.md](plan/so-lite-agent.md)。
- DeepSeek thinking 回传修复：并行工具调用回放按调用复制 reasoning item（实测 DeepSeek 要求每个 function_call 前都有 reasoning），仍被拒时兜底剥离 reasoning + `effort=none` 重试；真实 API 复验通过。
- 会话切换决策归主模型（ADR-0030/0032）：新消息先判断是否切换上下文、回合内 session::switch、回合末 LlmTurnDecider；消息树编辑/切分支（derive_branch/switch_branch）、上下文压缩（75% 阈值、最近 15 条保留）、InterruptBus 回合边界消费全部落地；审计记录点补齐（含 SessionSwitched/Memory*/SettingsChanged/Interrupt/MessageEdited/BranchSwitched）。
- 聊天页上下文缓存命中率（ADR-0033）：get_cache_stats 按会话 + 全局聚合主模型回合 usage（Responses `cached_tokens` / Chat Completions `prompt_cache_*`）；真实链路实测命中率 97.3%（命中 4864 / 未命中 190 tokens）。
- 会话切换防污染（ADR-0034）：session::switch 控制消息不落会话树、不随历史携带，切换后回答归新会话；真实链路实测后续回合不再反复切换。
- Pyodide 验算执行端完整化：numpy/sympy（符号计算/物理单位）离线打包（`npm run fetch:pyodide` 预热，vite 构建校验存在性）；前端自检真实执行解方程/求导/积分/单位换算/运动学/numpy；live_api 覆盖 kernel→桥→回执→模型续答全链路。
- 用户插件 7 个：hello、vision（看图理解：上传→读图→模型决定讲解/描述或判分归档）、grading（场景一：判分归档，输出 subject/reference_answer，含 get/update/remove/remove_many 错题管理命令，ADR-0038）、practice（场景二：生成/gaps/check，含智能出题与几何对拍）、report、exam、tracking；内核插件 5 个（storage/memory/compute/model/session），`memory::*`、`compute::verify`、`session::switch` 由内核模块经 KernelPlugin 契约注册（ADR-0035）——五个场景工具均可从会话内触达。
- 场景二 practice 智能出题全链路落地（2026-08-09，设计见 docs/variants.md）：确定性模板库 15 个初高中知识点（几何模板带 diagram_spec 与前端渲染器同源协议）+ 高考真题池（data/gaokao_pool.json include_str! 编译期嵌入，difficulty=exam 走池内抽取）+ LLM 自由出题（json_schema 强约束，模板未命中时）；LLM 生成的几何图经 compute::verify（verify_geometry.py）做存在性/自洽性对拍，失败注入 prompt 重出（连续 3 次停，执行端不可用降级放行）；practice::check 把练习记录落 memory（practice/history），generate 出题前读近 30 天已掌握集合避重复（prompt 注入避开清单 + 真题池过滤）。
- 场景一真实链路复验通过（2026-08-04）：图片/文本 PDF → Qwen3-VL OCR → deepseek-v4-flash（Responses API json_schema）判分 → 错题归档；assistant 消息落盘与 usage 解析已修复并有 live_api 断言。
- Tauri GUI 正式化（Vue 3 + Vite，按 ui-ux-pro-max 设计系统）：聊天/错题本/会话历史/设置四页 + **OOBE 首次引导**（test_connection 连通性自检）；思维链默认折叠、流式打字机、工具进度、停止、消息树编辑与分支切换、Pyodide 验算执行端（本地 WASM）、Iconify 图标、Markdown+KaTeX+DOMPurify 防 XSS、附件（图片/PDF 持久展示）、错题本搜索/排序。
- 英语练习模式（2026-08-15，ADR-0043）：settings.json `english_mode` 开关，开启后主对话/判分/出题/即时批改/图片理解/会话决策/摘要全链路模型输出切英文，GUI 文案保持中文。
- 设置页余额卡片（`check_balance` RPC）：DeepSeek `/user/balance` + SiliconFlow `/user/info` 真实查询，只读不落盘（ADR-0031）。
- **Standalone**：kernel 内嵌 GUI 进程，mistake-agent 单二进制即可运行（sidecar 已彻底移除）。
- 验收命令：`cd web && npm install && npm run fetch:pyodide && npm run build`；`cd web && npm run check:pyodide`；`cargo test`（142 项单元）；`cargo test --test live_api -- --ignored`（真实 API：hello 落盘+usage、三套样例、memory 往返、reasoning 回传回归 repro_reasoning、compute::verify 全链路）；`cargo run --bin mistake-agent`（GUI）。

## 10. 里程碑

| 里程碑 | 内容 | 验收标准 |
|---|---|---|
| M1 | 单 crate 骨架 + kernel 模块 | ✅ 完成：trait、注册表、dispatch、loop，hello 回合真实跑通 |
| M1.5 | kernel 的 session 模块 | ✅ 完成：生命周期、切换决策、会话分叉摘要（LlmSummarizer） |
| M2 | services：storage / model / memory | ✅ 完成：会话/审计文件持久化、双模型可调用（热更新）、记忆目录可读写 |
| M3 | RPC + Tauri 壳 | ✅ 完成：GUI ↔ kernel 进程内 RPC 闭环（standalone） |
| M4 | 五个插件 + compute::verify | ✅ 完成：7 用户插件 + 5 内核插件注册；场景一全链路 + Pyodide 验算桥接 |
| M5 | 消息树 / 记忆路由 / 设置向导 / 审计日志 | ✅ 完成：编辑/切分支、memory 工具、设置页、审计补全 |
| M6 | Windows 打包 + 测试 + 文档 | ✅ 完成：142 单测 + 真实 API 链路 + 文档同步；Windows setup.exe 安装运行实测通过（2026-08-09） |

后续计划：**M7 = Agent core 剥离为 `so-lite-agent` crate**（ADR-0037，进行中）——M1-M4 已落地，M5 待办；参考 Pi 分层，开箱即用，内核/用户插件由使用方编写；实施顺序见 [docs/plan/so-lite-agent.md](plan/so-lite-agent.md)。

产品路线图（规划中，未排期）：

| 阶段 | 内容 | 关键点 |
|---|---|---|
| 近期（桌面输入增强） | 剪贴板粘贴截图（Ctrl+V）；摄像头拍题 | 走现有附件暂存管线（vision__read → 判分归档）；WebView2 摄像头权限 |
| 中期（Android 手机/平板） | Tauri v2 Android target：移动壳 + 触控/窄屏响应式 + 相册/摄像头/剪贴板输入 + Pyodide 移动端验证 | 移动存储路径与权限模型、离线包体积、性能；Windows 装 Android SDK 即可构建，不依赖 macOS |
| 长期（iOS / iPadOS） | Android 落地后追加 iOS/iPadOS target | 本机无 macOS：构建/签名/发布走云 macOS（GitHub Actions macOS runner——公开仓库免费额度，优先；备选 Codemagic / MacStadium）、Apple 权限模型 |

## 11. 分工建议（3-5 人）

| 角色 | 负责 | 对应里程碑 |
|---|---|---|
| A. Kernel 工程师 ×1-2 | kernel crate：trait、loop、dispatch、注册表、会话调度、RPC、护栏、审计 | M1、M1.5、M3 内核侧 |
| B. 服务工程师 ×1 | storage（会话/审计/错题）、model（双模型）、memory 路由 | M2 |
| C. 插件工程师 ×1-2 | grading（含 OCR 流程）优先，其余四个随后；compute::verify 契约 | M4 |
| D. GUI 工程师 ×1 | Tauri 壳、聊天/消息树 UI、设置向导、事件渲染 | M3、M5 |
| E. 测试/打包（可兼任） | Windows 安装包、端到端样例、文档 | M6 |

依赖关系：A+B 先行（M1-M2），D 可与 A 并行搭壳（M3 联调），C 依赖 M2 的 service 句柄，E 全程可跟进。

插件开发手册：docs/plugin-dev/user.md（用户插件）与 docs/plugin-dev/kernel.md（内核插件）；参考模板 docs/plugin-dev/reference/（复制即开工，编译锚定测试保证与契约同步，ADR-0036）。

## 12. 命名规范

- 入口点命名：`namespace::tool`——插件只写短名（`upload`），kernel 拼全名（`grading::upload`），撞名从机制上不可能；模型可见名经 wire name 映射（`::`→`__`，如 `grading::upload` → `grading__upload`），内部名、审计名与 `trigger_command` 不变（ADR-0020）。
- 三类入口点：**Tool**（LLM 调度）、**Command**（GUI/用户调度）、**Event**（kernel 生命周期调度）。
- 内核服务：`ServiceId::{Storage, Memory, Compute, Model}`；内核插件经 `KernelPlugin` 两段式契约注册（info 声明 namespace/provides/入口点，register 绑定 handler，ADR-0035）。
- 会话调度是独立内核级模块（kernel-session），**不占 ServiceId**；切换决策由主模型完成（LlmTurnDecider，失败降级 continue）。
- 工具列表示例：`vision::read / grading::upload / grading::list / practice::generate / practice::gaps / practice::check / report::weekly / exam::compose / tracking::checkin / compute::verify / memory::save / memory::show / memory::remove`；会话历史经 RPC `list_sessions / read_session` 提供（不注册为模型工具）。

## 13. 术语表（浓缩）

- **Kernel（内核）**：核心调度层——agent loop、会话、工具注册与调度、事件/RPC、指令加载。
- **Kernel plugin（内核插件）**：信任边界内的特权子系统（storage/memory/compute/model + session 调度模块），经 `KernelPlugin` 两段式契约注册。
- **User plugin（用户插件）**：注册工具/命令/事件回调的业务插件，回调由 kernel 主动调用。
- **Service / Service handle**：内核插件提供的受控能力 / 注入用户插件的受限接口（等价 OS 的 fd）。
- **CallerPolicy**：UserAndModel 或 UserOnly，决定入口点谁能调用。
- **EntryPoint**：Tool / Command / Event 三类调用入口。
- **ToolDef / ToolError**：工具元数据（短名、描述、schema）/ 结构化错误（code、message、retryable）。
- **Turn（回合）**：一次完整的 agent 执行单元。
- **SessionKey**：内部会话路由键，对用户隐藏。
- **Session scheduler**：独立内核级模块，负责会话调度；任务层由主模型决策（ADR-0030/0032），持久化委托 storage。
- **Guard model（守卫模型）**：已退役（ADR-0030）——现切换决策全部归主模型：新消息到达先判断（ADR-0032）、回合内 `session::switch` 工具、回合末 LlmTurnDecider 判断三动作；失败一律 continue（存疑即继续）。
- **Goal（会话目标）**：当前会话要完成的学习目标，主模型在 start_new 时生成并写入会话元数据，作为 continue / update_goal / start_new 的决策依据。
- **History route（历史路由）**：session::history / session::read，模型按需翻阅完整消息树；新会话上下文只含本会话子树（从摘要节点起算），旧会话内容不进模型上下文。
- **Message tree / Active path**：id/parentId 消息树 / 上下文只包含的当前路径。
- **Memory route**：按层级路径组织的跨会话记忆。
- **Compaction**：活跃路径旧消息的上下文摘要（原文保留）。
- **Audit / Diagnostic log**：操作事实记录（默认全覆盖）/ 分级诊断日志。
- **Data root**：`~/Documents/.mistake-agent`，一切数据所在。
- **Main model / Vision model**：deepseek-v4-flash / SiliconFlow qwen3-VL。
- **ModelHandle**：注入插件的受限模型服务句柄（带超时/abort/审计）。
- **Command channel**：trigger_command，GUI 触发命令的唯一通道。
- **Compute backend**：验算执行端（v2 为 WebView 内 Pyodide）。

## 14. 风险与后续优化

- **风险**：Windows 打包已实测通过（NSIS setup.exe 安装运行正常）；settings 明文存 key 是已知取舍（DPAPI 列后续）；主模型每回合新消息预决策 + 回合末决策共两次小调用，有少量成本（可接受）。
- **后续优化**：工具并行（拓扑排序）、子 agent、wasmtime 内嵌 Python（compute 收进 kernel）、第三方插件/技能系统、数据目录可配置、家长端报表、Windows 凭据管理器。
