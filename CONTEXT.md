# Mistake Agent Context

面向中学生的本地错题管理与辅助学习 Agent（v2）。采用 OS 式分层：内核负责核心调度，内核插件处理敏感资源与能力，用户插件提供业务功能。

## Language

**Kernel（内核）**:
本地 Agent 引擎的核心调度层，负责 Agent loop、会话生命周期、工具注册与调度、事件/RPC 和指令/技能加载；不实现任何业务能力。
代码布局：`src/kernel/agent/`（核心调度层）+ `src/kernel/plugin/`（内核插件，一插件一文件夹）。
_Avoid_: 核心、引擎（含义过宽）

**Agent core（Agent 核心）**:
可跨应用复用的通用 Agent 运行时：Agent loop、工具注册与调度、会话生命周期、模型运行时抽象与审计护栏；即 Kernel 剔除应用专属内核插件后余下的部分。已按 ADR-0037 从 mistake-agent 提取为独立 crate `so-lite-agent` 仓库（M1-M4 已落地并迁出，M5 crates.io 发布待办），新 Agent 通过 `cargo add so-lite-agent` 复用，内核插件由使用方自行编写。
_Avoid_: 引擎、Agent 内核（与 Kernel 混用）

**Kernel plugin（内核插件）**:
运行在内核信任边界内的特权子系统，负责敏感资源与能力，如会话存储和验算运行时。
经 `KernelPlugin` 两段式契约注册（info + register，ADR-0035），注册上下文为全量服务句柄。
_Avoid_: 内核级插件（口语）、系统服务

**KernelPlugin contract（内核插件两段式契约）**:
内核插件的注册机制，与用户插件 `UserPlugin` 同形：`info()` 声明 namespace、`provides`（提供的 ServiceId）与工具/命令/事件，`register(ctx)` 绑定 handler；与用户插件共用同一张注册表校验（namespace/wire 唯一、CallerPolicy、懒/急加载），注册上下文 `KernelContext` 注入全量服务句柄。
_Avoid_: 直连注册（ADR-0021 旧表述）

**Disabled plugin（禁用插件）**:
插件目录根部存在 `disabled` 标记文件、不参与构建与注册的插件；移除标记即恢复（ADR-0036）。
_Avoid_: 卸载、下架（暗示运行时卸载，编译期内置无此语义）

**User plugin（用户插件）**:
通过内核注册工具、命令与事件回调提供业务能力的插件，如批改、练习、复盘；其回调由 kernel 主动调用，但不直接接触敏感资源。
_Avoid_: 业务插件（过早限定业务范围）、用户态插件（口语）

**Service（服务）**:
内核插件向 kernel 提供的受控能力（v2 为会话存储、验算运行时、记忆、模型），由内核插件在 info 中以 `provides` 声明，用户插件只能通过服务句柄访问。
_Avoid_: API（含义过泛）

**Service handle（服务句柄）**:
kernel 按能力声明注入用户插件的受限接口，只暴露该插件需要的方法，不暴露底层资源；等价于 OS 的文件描述符。用户插件的一切磁盘读写只经 `StorageHandle` 语义方法（ADR-0042 磁盘 IO 铁律）。
_Avoid_: 全局单例、直接依赖

**DomainIo（域内文件能力）**:
storage 引出的数据根目录域内文件 trait（read/write/remove/remove_tree/list，域 = `Domain` 枚举：mistakes/sessions/memory/data/uploads）；实现内部做域根拼接 + canonicalize 兜底（防符号链接逃逸）+ 原子写 + 审计（FileIo）。只注入内核插件（如 memory），用户插件永不持有。_Avoid_: 通用文件系统 API（插件直读 std::fs）

**TmpIo（暂存文件能力）**:
storage 引出的系统 temp 暂存文件 trait（read_staged/remove_staged），硬编码 `std::env::temp_dir()` + `mistake-agent-` 前缀白名单，与 DomainIo 解耦；读删记审计（StagedFileIo）。ADR-0046 后图片改为 uploads/ 路径引用直入模型上下文，附件不再经此通道（代码保留）。_Avoid_: 让插件直读暂存路径

**RelPath（相对路径）**:
类型安全的域内相对路径：`parse` 构造即校验（段白名单 `[a-zA-Z0-9._-]`、首尾必须字母数字、拒绝 `.`/`..`/`\`/`:`/非 ASCII），不做任何路径规范化（规范化即攻击面），fail-closed——类型上不可能表示目录遍历。_Avoid_: 裸字符串路径拼接

**Capability declaration（能力声明）**:
用户插件注册时声明的服务依赖清单；kernel 据此校验并注入句柄。
_Avoid_: 依赖注入（框架术语）

**Trust boundary（信任边界）**:
内核与内核插件所在的受信区域；用户插件只通过服务句柄与之交互。
_Avoid_: 沙箱（沙箱专指验算运行时）

**LoadPolicy（加载策略）**:
插件在 info 中声明的初始化时机：eager（读取即加载）或 lazy（首次使用才加载），默认 lazy。
_Avoid_: 积极性（口语）

**ToolDef（工具定义）**:
工具在 info 中声明的元数据条目：短名、描述、参数 schema；实际执行由 register 阶段绑定的 handler 完成。
_Avoid_: 工具（指调用实例）

**Tool catalog（工具目录）**:
前端可见工具元数据（标题/分组/图标/描述/参数 schema）的唯一事实源，经 `list_tools` RPC 由 kernel 下发；前端不得自行维护工具名 → 展示信息映射。
_Avoid_: 前端工具表、硬编码图标/标题

**EntryPoint（入口点）**:
用户插件向 kernel 登记的调用入口，共三类：Tool（LLM 调度）、Command（GUI/用户调度）、Event（kernel 生命周期调度）。
_Avoid_: 回调（只指其中一类）、接口

**ToolError（工具错误）**:
工具调用失败时返回给 LLM 的结构化错误，含 code、message、retryable；retryable 表示是否值得模型换参数重试。
_Avoid_: 异常（Rust panic 语义不同）

**Turn（回合）**:
kernel 一次完整的 agent 执行单元：从输入触发开始，到模型自然停止或护栏中止结束，期间可多次调用工具。
_Avoid_: 会话（Session 是整个使用生命周期）

**ModelRuntime（模型运行时）**:
kernel 对 LLM provider 的统一抽象，提供流式消息与工具调用；v2 实现 OpenAI 兼容端点与 Ollama。
_Avoid_: Provider（指具体厂商适配器）

**Session（会话）**:
一次对话流的过程记录，JSONL 追加式持久化；一个话题 = 一条独立会话，**新建、切换、重命名、删除都只由用户发起**（ADR-0044，`create_session` / `open_session` / `rename_session` / `delete_session` RPC），旧会话归档保留。
_Avoid_: 对话（用户视角的聊天）、聊天记录

**SessionKey（会话键）**:
标识一个会话的内部路由键；平台层由消息来源派生（v2 即本地 App 本身，未来聊天渠道按渠道与对端隔离），用户新建会话时生成新键。键本身不暴露给用户，但会话边界对用户可见（GUI 可新建/查看）。
_Avoid_: 会话 ID（暗示用户可见）

**Session handoff（会话交接）**:
用户新建会话时（`create_session` 且 `carry_summary` 为真），把「上一会话梗概」作为新会话首条 system 消息带入的机制，让新会话延续旧会话的结论；旧会话本身不被写入。
_Avoid_: 迁移、续传

**Interrupt（内部中断）**:
内核组件（settings、memory、compaction）向 agent loop 发出的环境变更信号，通知其下回合上下文需按新环境重组；回合边界消费，不抢占当前回合，真正需要立即打断的场景走取消链。（会话调度已不再是生产者——ADR-0044 删除模型自动切换后，`SessionSwitched` / `GoalUpdated` 一并消失。）
_Avoid_: 事件（Event 指面向 GUI 的播报）

**Wire name（模型可见名）**:
内部规范名 namespace::tool 经 :: → __（双下划线）映射后发给模型的工具名（如 grading::upload → grading__upload），函数名受 OpenAI 系 API 的 ^[a-zA-Z0-9_-]+$ 约束；内部名、审计名与 trigger_command 不变。
_Avoid_: 全名（指内部 namespace::tool）

**Session scheduler（会话调度）**:
独立的内核级模块（非服务插件），负责会话生命周期、会话标题生成、空闲超时检测与交接摘要；**会话新建与切换只由用户发起**（ADR-0044，`create_session` / `open_session` RPC）——归档全部活动会话后新建独立 SessionKey 或激活指定会话，树内分叉机制已整体删除；持久化委托 storage 服务。
_Avoid_: 会话管理（易与用户可见的管理界面混淆）

**Guard model（守卫模型）**:
（已退役，ADR-0044）原设计由 Session scheduler 调用的独立调度模型，负责在"新消息到达"与"回合结束"时决策 continue / update_goal / start_new。最后一个调用方（会话切换决策）已删除，`GuardModel` trait / `guard_prompt` / `StubGuard` / `LlmTurnDecider` 全部移除。
_Avoid_: 调度模型（易与主模型混淆）

**Goal（会话目标）**:
会话元数据中的可选学习目标（摘要器的输入）；`create_session` 可显式传入，不再是模型决策的产物。与 Session title 语义分离：Goal 面向模型，title 面向用户。
_Avoid_: 任务名（过窄，Goal 可含更丰富描述）

**Session title（会话标题）**:
会话元数据中的用户可见名称（`SessionMeta.title`），显示在聊天页侧栏列表；首回合结束后由模型异步生成（`LlmTitler` + `session_title_prompt`，≤12 字），失败降级为首条用户消息前 40 字；用户可经 `rename_session` 改名，已有标题不再自动覆盖。与 Goal 的差别在于**受众**：title 给人看，Goal 给摘要器看。
_Avoid_: 会话名（口语）、Goal（给模型看的学习目标）

**History route（历史路由）**:
浏览历史会话的通道，经 RPC `list_sessions` / `read_session` 提供（**未注册为模型工具**——模型看不到历史路由）；聊天页侧栏会话列表是它唯一的 GUI 呈现（原独立「会话」页已删除）；新建会话后旧会话完整归档，可按需翻阅。
_Avoid_: 聊天记录查询（口语）

**Message tree（消息树）**:
会话内消息的组织结构：每条消息有 id 与 parentId，JSONL 追加式存储；编辑消息或"重新生成"回答时在该点派生新分支，历史永不截断。
_Avoid_: 版本历史、对话树（口语）

**Active path（活跃路径）**:
消息树中从根到当前节点的唯一路径；LLM 上下文只包含活跃路径上的消息，旁支不进入上下文。
_Avoid_: 当前分支（口语）

**Memory route（记忆路由）**:
记忆按层级路径组织（学科/知识点/条目），模型通过 memory::save/show/remove 自行浏览与读写；数据根目录 memory/ 文件持久化（重启不丢）；上下文不注入记忆内容，只保留一行入口提示。
_Avoid_: 记忆检索（暗示向量/全文检索）、长期记忆（过于宽泛）

**Memory entry（记忆条目）**:
记忆目录中的一个具体条目，由路径定位、文本内容承载；写路径由 LLM 决定，路径由 memory 内核插件校验。
_Avoid_: 记忆文件（暗示实现细节）

**Command channel（命令通道）**:
GUI 触发已注册 EntryPoint 的唯一通道（trigger_command）；协议层不存在"执行任意命令"的接口，前端门禁由此结构性成立。
_Avoid_: 命令执行（暗示任意执行）

**ModelHandle（模型句柄）**:
kernel 注入用户插件的受限模型服务句柄，仅暴露带超时、abort 与审计的 complete 调用；凭据与 provider 适配永远不离开 kernel。
_Avoid_: 模型客户端、直接调 provider

**Settings（配置）**:
数据根目录 settings.json 的内容，由用户通过 App 设置界面独占写入，kernel 启动时读取；模型与插件没有任何配置访问通道。
_Avoid_: 配置文件（实现细节）、系统设置

**English immersion mode（英语练习模式）**:
settings.json 的 `english_mode` 布尔开关（默认 false）；开启后主对话、判分、出题、即时批改、图片理解、会话标题与摘要等模型提示全部追加英文输出规则，GUI 界面文字保持中文。
_Avoid_: 界面语言切换（只切模型输出语言）

**Compute backend（验算执行端）**:
compute 服务的实际执行位置（v2 为 GUI WebView 内的 Pyodide，经 Event::ComputeRequest / Method::ComputeResult 桥接，kernel 侧 BridgeCompute 等待回执并做超时/取消/审计）；Pyodide 即 WASM 沙箱。
_Avoid_: 沙箱（专指隔离形态）

**Audit（审计）**:
所有操作的强制记录（默认全覆盖），由 kernel 经 storage 服务写入数据根目录 audit/ 的追加式 JSONL；流式中间态不记，大内容以引用关联。
_Avoid_: 日志（过泛，包含调试日志）

**Diagnostic log（诊断日志）**:
分级诊断记录（DEBUG/INFO/WARN/ERROR/CRITICAL/PANIC），与审计分离，写入数据根目录 logs/；敏感值脱敏。
_Avoid_: 日志（与 Audit 混用）、审计日志

**Model（模型）**:
承担 agent loop 调度与对话、判分、摘要与图片理解的模型（v2 为 DeepSeek `deepseek-flash`，经 Responses API 接入，图片走 `input_image`），在 settings 中以单份配置提供 API_URL 与 API_KEY。
_Avoid_: 聊天模型（口语）、主模型（旧双模型叫法）

**Vision model（视觉模型）**:
（已退役，ADR-0045）旧方案中负责图片理解与 OCR 的独立模型（硅基流动 SiliconFlow 的 qwen3-VL，经 Chat Completions 接入）。`deepseek-flash` 的 Responses API 已原生支持图片输入，该端点与 `ModelKind` 选路一并删除；图片理解内化为模型能力，不再是独立术语。
_Avoid_: 用「视觉模型」指代现在的图片理解（已无独立模型）

**Attachment ref（附件引用）**:
用户消息上对 uploads/ 持久附件的路径式引用（`AttachmentRef`：相对文件名 + mime + 原名，ADR-0046）；消息树只存引用、不存图片字节，模型请求构建时由 `AttachmentResolvingModelService` 读盘还原为运行时附件并展开成 `input_image`（进程内缓存）。_Avoid_: 把图片 base64 落进会话 JSONL、让插件直接读 uploads 路径

**CallerPolicy（调用方策略）**:
EntryPoint 的调用方边界：UserAndModel（模型可调，用户必可调）或 UserOnly（仅用户可调，模型工具列表不可见且调度拒绝）。
_Avoid_: 权限（含义过泛）

**Data root（数据根目录）**:
本 Agent 所有数据与配置的统一存放目录（~/Documents/.mistake-agent）；不存在"项目"概念。
_Avoid_: 项目目录、工作区


**Learning task（学习任务）**:
学生在一次会话中要完成的学习单元（批改一次作业、一轮复习等），与会话一一对应。
_Avoid_: 任务（Task，易与工具任务混淆）

**Chemistry rendering（化学渲染）**:
前端 Markdown 中化学内容的渲染方式：KaTeX + 官方 mhchem 扩展（`\ce{}` / `\pu{}`）支持化学式、方程式、同位素与单位；结构式（键线式）由模型以 SMILES 代码块（```smiles）输出，前端 smiles-drawer 绘制 SVG；不支持 chemfig/TikZ 类结构式宏包（KaTeX 无 TikZ 引擎，需完整 LaTeX 才可编译）。
_Avoid_: 直接把 chemfig 当 KaTeX 宏包引入（会静默渲染失败）、让模型输出结构式图片或 Unicode 伪图形

**Mistake management state（错题管理状态）**:
错题本记录的轻量管理字段：`is_correct` 表示已掌握（复用原有字段），`pinned` 表示置顶，`deleted_at` 非空表示软删除；`grading::list` 默认隐藏已删除记录，`grading::remove` / `grading::remove_many` 只写 `deleted_at`，不物理删除。_Avoid_: 硬删除错题、为已掌握另建 `mastered` 字段

**Mistake edit boundary（错题编辑边界）**:
错题修改的权限语义：模型可经 `grading::update` 改**内容字段**（subject/knowledge_point/title/question/student_answer/reference_answer/analysis），不可改**管理字段**（is_correct/pinned/deleted_at）；删除（remove/remove_many）与已掌握标记仅用户可做（UserOnly）。模型是错题本主要写入者（判分归档、练习回写），编辑能力保证幻觉内容可自愈；管理字段只由用户维护，避免模型污染掌握度统计。_Avoid_: 模型可删题、模型标已掌握

**Mistake card title（错题卡标题）**:
错题卡与详情抽屉顶部那句话，存在 `Mistake.title`：判分归档时由模型一并生成（≤16 字，概括考点或错因），用户可在编辑弹窗里改写，传纯空白即清空。**可缺省**——存量错题没有该字段，练习模块回写的错题也不生成，此时前端回退显示「学科 · 知识点」，因此标题永远不构成数据完整性要求，无需回填。_Avoid_: 把标题当必填、用标题代替题干（题干始终是逐字原文）

**Mistake event log（错题事件流）**:
追加式 JSONL（错题条目内 `events.jsonl`），逐条记录每道错题的判分与掌握度变更，是「正确率变化 / 反复丢分 / 掌握度」等时间线统计的唯一业务真相；与审计（Audit，操作事实记录、10MB 轮转）不同，事件流不轮转、只追加，`mistake.json` 快照中的 `is_correct` 只是其最新状态。_Avoid_: 审计、日志、Attempt 数组内嵌错题记录（快照与时间线分离，事件不进 mistake.json）

**Mistake entry（错题条目）**:
错题本的一个存储单元：`mistakes/<id>/` 目录，内含 `mistake.json`（当前快照）、`events.jsonl`（该题事件流）、`schedule.json`（该题掌握度调度）——错题以目录为领域对象，与 `sessions/<key>.jsonl` 每会话一文件的哲学一致；旧单文件 `mistakes.json` 由 bootstrap 一次性迁移。_Avoid_: mistakes.json 单文件全量重写、把事件内嵌进错题记录

**Mastery schedule（掌握度调度）**:
每道错题的 Anki 式调度状态（`schedule.json`：interval/ease/due_at/last_result），由判分事件折叠更新——调度层「错 1 次即重置间隔回 7 天」（again 语义，节奏惩罚），状态层「连错 2 次才打回已掌握」（掌握裁决，避免偶然失误误伤）；exam 达标（题数≥2 且得分率≥80%）是可信证据可自动置已掌握。调度与裁决分离，事件流为证据、调度为折叠状态。_Avoid_: 固定 7/14/30 天硬编码、已掌握凭用户自报永不过期

**Timer service（定时服务）**:
内核插件，提供定时触发与主动回合申请通道（`ServiceId::Scheduler` + `SchedulerHandle`）；只存插件注册的定时配置（interval / 载荷文本 / fire_on_start），到点请求 kernel 核心、由内核特权发起中断，对业务零感知（不知道到期/重测/清单）。_Avoid_: 调度器（与 Session scheduler 混淆）、定时器（只指底层 tokio 机制）

**Proactive turn（主动回合）**:
定时中断唤醒后发起的无用户消息回合——scheduler 内核插件到点请求 kernel 核心，内核特权在回合边界发起 Interrupt（"环境有变动"信号），回合空闲时从 pending 队列消费发起独立回合（不并入用户回合）；模型经白名单工具 `tracking::due_list` 自查到期清单，产出带 `proactive` 标记的合成 user 消息（display_text 前端渲染为系统通知气泡）落树；proactive 回合工具白名单缺省为空（结构性杜绝自动出题）。防骚扰状态（last_reminded_at / dismissed_until）纯内存、重启作废。_Avoid_: 推送通知（无服务器）、后台任务抢占当前回合、自动重测（模型只提醒，判分链只在学生回应后走）

**Knowledge graph（知识图谱）**:
知识点及其关联的结构化拓扑：每学科一个 `mistakes/graph/<学科>.json`（文件名 sanitize），节点 ID = `学科::知识点`，纯拓扑（节点 + 边 + 权重），属性实时聚合自 schedule.json / events.jsonl（可重建）；边分先验层（启动时模型生成一次前置依赖表落盘 `data/point_deps.json`，生成即数据）与共现层（一次判分批次 batch_id 事件驱动增量，权重 = 共现批次数，双层剪枝）；图谱按学科隔离（无跨学科边）。图谱同时是可视化数据源（`tracking::graph` Command → ECharts 力导向图）与 Agentic 检索索引（`tracking::graph_query`）。_Avoid_: 图数据库（违反本地单二进制红线）、向量检索（结构化精确过滤优先）、图谱快照存属性（真相在事件流，属性现算）

**Runtime data（运行时数据）**:
数据根目录 `data/` 下的教学数据文件（`data/gaokao_pool.json`、`data/point_deps.json` 等），bootstrap 启动时缺失即写默认种子（与 AGENTS.md 同款幂等），运行时可编辑、可被模型经 storage 句柄更新（生成即数据）；与编译期 include_str! 嵌入相对。_Avoid_: 静态资源、内置数据（暗示不可变）

**Instruction loading（指令加载）**:
数据根目录单文件 `AGENTS.md`（教学规则，家长/老师可编辑）全文注入主模型系统提示（静态基底之后、debug 段之前；`load_agents_md`）。文件缺失（Missing）/ 非 UTF-8（InvalidUtf8）/ 超 64KB（TooLarge）时回退静态基底；路径仅由数据根拼接固定文件名，无用户输入路径与遍历面；保存即生效（无缓存，每请求读取）。设置页经 `get_rules_status` 展示加载状态、`open_rules_file` 打开编辑。英文练习模式开启时，同一 AGENTS.md 中文教学规则照常注入，由静态层英文人设（B+C 演法：全听懂中文、假装只抓英文关键词、永远只回英文并用英文引导组句）保证输出全英文；不翻译、不生成独立英文规则文件。_Avoid_: 技能系统（v2 无技能，ADR-0012）、分层合并指令文件（ADR-0011 单文件）

**Account（账号）**:
平台服务中的用户身份（`users` 表）：`role`（user / teacher / admin）、`sync_enabled`、状态；首个 admin 由 bootstrap 种子（`ADMIN_EMAIL`/`ADMIN_PASSWORD` 环境变量）创建，不开放管理员自助注册；管理操作经 **REST + WebUI**（ADR-0047 修订 R12，不做 CLI），客户端经 `settings.json` 的 `account` 段持有登录态（ADR-0047/0048）。
_Avoid_: 用户（易与本地昵称 `nickname` 混淆——昵称只是显示称呼，不构成身份）

**Platform service（平台服务）**:
可选接入的服务端能力合集（账号、DeepSeek 中转、多设备同步）。**不构成使用前提**：不登录时客户端与纯本地形态完全一致（ADR-0047/0048）。
_Avoid_: 云端版（暗示数据必须在云上）、后端（指实现而非能力）

**Relay（中转）**:
服务端以平台密钥代理 DeepSeek Responses API 的通道（`POST /responses`）；对客户端 drop-in 兼容（模型适配器零改动），服务端不解析工具调用、不拼上下文，请求与响应正文**不落库**，只记用量元数据（ADR-0047）。
_Avoid_: 代理（易与网络代理混淆）、网关（暗示更多职责）

**Service package（服务包）**:
可售卖的商品形态（`plans` 表）：体验包（一次性 N 次）或月卡（有效期 + 三窗口限额）；经兑换码兑换后生成 Entitlement 才生效（ADR-0047）。
_Avoid_: 会员（暗示订阅自动续费）、点数（暗示虚拟货币）

**Entitlement（权益）**:
一次兑换/发放产生的可用额度实例（`entitlements` 表）：额度、生效期、状态；月卡叠加时新权益**顺延**接在旧权益到期之后，不从当下起算（ADR-0047）。
_Avoid_: 订单（Order 是售卖凭据，Entitlement 是可用额度）、套餐（Plan 是商品定义）

**Redemption code（兑换码）**:
线下收款后发给用户的凭据（Crockford Base32 + 校验位，可批量生成与 CSV 导出）；兑换在**事务内**完成绑定与权益生成，`source` 预留 `payment` 供二期接在线支付（ADR-0047）。
_Avoid_: 激活码（暗示激活产品而非额度）、卡密（口语）

**Usage window（限额窗口）**:
月卡的三个**滑动**限额窗口（5 小时 / 7 天 / 30 天）：任意窗口内已扣次数不得超过阈值。滑动语义避免自然周期的边界双倍消耗，也便于与月卡到期日对齐（ADR-0047）。
_Avoid_: 配额重置（暗示固定周期清零）

**Billed uses（扣次）**:
一次模型请求计入的「次数」（对外的计量单位）：按 `input+output` 总量阶梯扣 1/2/3 次，防单次超长上下文击穿按次售卖；token 明细另记 `usage_events` 作内账，用于成本核算与限额校准（ADR-0047）。
_Avoid_: 按 token 计费（对外不露出 token）、请求次数（不等同于配额消耗）

**Device sync（设备同步）**:
同一账号多设备间同步会话 / 错题 / 记忆的机制：**默认关闭**、OOBE 登录后询问、可随时关闭并删除云端数据。本地始终是真相源——断网零退化、同步失败不阻塞本地写（ADR-0049）。
_Avoid_: 云备份（暗示把数据搬走）、云端为准（本地才是真相源）

**Sync cursor（同步光标）**:
服务端 `changes.id` 的单调序列，客户端增量拉取的唯一依据。相比时间戳：并发写入与时钟漂移下时间戳不可靠（同毫秒、回拨）（ADR-0049）。
_Avoid_: 时间戳水位（不可靠）、版本号（指单实体的 rev）

**Sync outbox（同步发件箱）**:
客户端 storage 在每次成功写入后追加的变更记录文件（`sync_outbox.jsonl`），推送成功后推进 `synced_seq` 水位。会话消息以 JSONL **字节偏移**作天然水位（文件只追加不修改，ADR-0026/0044）；错题是覆盖写，只能靠写时产出记录（ADR-0049）。
_Avoid_: 变更日志（与服务端 `changes` 表混淆）、全量比对（无法捕捉覆盖写）

**Sync conflict resolution（同步冲突裁决）**:
消息与事件取**并集**（UUID + 唯一键，永不冲突）；会话元数据 / 错题快照 / 记忆按 `updated_at` **LWW**，被覆盖一侧落本地 `conflicts/` 保留副本、GUI 只显示计数（ADR-0049）。
_Avoid_: 冲突解决界面（首期不做，概率极低）、静默丢弃（不可接受）

**Blob store（附件内容寻址）**:
附件原图按 `sha256` 内容寻址的服务端存储（`POST / GET /api/v1/blobs`），渲染时懒加载。**首期不启用**（原图同步是独立计费的增值项），协议预留使后置不返工（ADR-0049）。
_Avoid_: 附件表（指元数据）、图床（暗示公网直链）
