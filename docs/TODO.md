# TODO

## 新增待办（2026-09-19）：会话改用户手动切换 + 模型收敛 DeepSeek + 教师服务端 + 错题本优化

### 1. 会话系统改成只有用户能切换（参考 Chatbox）

目标：**一个话题 = 一条独立会话**，左侧会话列表 +「新对话」（Chatbox / DeepSeek 网页版形态）；**会话切换只由用户发起**，主模型不再自动判断换话题、不再自动分叉新会话。放弃「摘要节点 + 新会话子树」的树内分叉承载会话边界（ADR-0026/0030/0032 需修订或另立 ADR）。→ **已另立 [ADR-0044](adr/0044-user-driven-session-creation.md)**（2026-09-21），0030/0032/0034 已标注被取代。

- [x] 下线模型自动切换：`SessionScheduler::on_new_message` 主模型预决策（ADR-0032）、回合末 `LlmTurnDecider` 决策（ADR-0030）、`session::switch` 工具（模型不可见）三处一并删除；`GuardModel`/`turn_decider_prompt` 相应退役。✅ **已完成（2026-09-21，ADR-0044）**：三处全部删除，`src/kernel/agent/session/guard.rs` 与 `src/kernel/plugin/session/` 整个目录移除；`complete_with_retry` 迁至 `session/summarize.rs` 与 `LlmSummarizer` 共用。
- [x] 保留的自动行为：仅系统级空闲超时是否保留待定（倾向保留但改为"提示用户"而非自动切）；失败降级逻辑随决策一起删除。✅ **已完成（2026-09-21，ADR-0044）**：按"保留检测、改为提示用户"落地——12h 空闲超时仍检测，但只发 `session_idle` 事件，不再自动分叉；失败降级逻辑随决策一并删除（不再有决策，也就没有决策失败）。
- [x] 后端 `start_new` 语义 = 用户手动新建会话（不再由模型触发）；交接摘要仅在用户新建会话时按需携带。✅ **后端已完成（2026-09-21，ADR-0044）**：新增 `create_session` RPC（`carry_summary` / `goal` 参数），归档旧会话 + 新建独立 `SessionKey`；单 Active 不变量由"归档全部 Active"保证；回合在飞时拒绝（`turn_in_progress`）。**前端入口仍待做**（见下一条）。
- [x] 前端：会话列表（新建 / 重命名 / 删除 / 按最近活动排序）+「新对话」入口，Chatbox 式交互；会话标题仍由模型按首条消息生成。✅ **已完成（2026-09-23）**：聊天页左栏新增 [SessionListPanel.vue](../web/src/components/SessionListPanel.vue)（「新对话」/ 内联重命名 / 二次确认删除 / 按 `last_activity_at` 倒序），App 的「会话」导航项与 `SessionsPage.vue` 一并删除；聊天页只渲染当前会话（`renderSessionBubbles`），切换/新建/删除走 `open_session` / `create_session` / `delete_session`。标题在首回合落盘 + `TurnEnd` 之后**异步**由模型生成（`LlmTitler` + `session_title_prompt`），失败降级为首条用户消息前 40 字；生成/改名后发 `session_title_updated` 让列表刷新。
- [x] 存量数据迁移：树结构会话（含摘要节点、兄弟分支）拆分为独立会话条目，幂等 + `.bak`。✅ **已完成（2026-09-23）**：[file/migrate.rs](../src/kernel/plugin/storage/file/migrate.rs)，在 `FileStorage::open` 加载会话前执行；按 `上一会话梗概：` 边界节点（`上下文压缩摘要：` / 老一代 `交接摘要：` 不算）切分并沿 parent 链归属后代（含兄弟分支）→ 每段新 `SessionKey`、段首 `parent_id = None`、消息 id 保留；仅含老 `active_path` 的段继承老状态（**不新增 Active**）。原文件改名为 `<key>.jsonl.bak` 完整保留，扩展名非 `.jsonl` 故二次启动自然跳过（幂等）。非致命：失败只 `log::warn`。
- [x] **已定（2026-09-23）**：会话内消息版本切换（编辑重发 + `< / >` 浏览旧版本，DeepSeek 式）**保留**，明确限定为"会话内版本浏览"，不再承担会话边界语义；`switch_branch` 仍只作用于当前活动会话。

### 2. 设置删掉硅基流动等视觉模型，统一只用 DeepSeek

目标：单份 DeepSeek 配置同时承担**主模型 + 调度模型 + 视觉理解模型**。✅ **已落地（2026-09-23，ADR-0045）**

- [x] settings.json 收敛为一份模型配置（`api_url` / `api_key` / `model` / `transport`），默认模型改 `deepseek-flash`。**决策调整**：`vision_model` 字段未删除，按"保留字段但不再使用"处理（兼容旧配置、解析不报错），运行时不再读取；前端不再展示。
- [x] 设置页删「视觉模型（OCR / 图片理解）」卡片与 SiliconFlow 余额项；OOBE 由四步收敛为三步。（[SettingsPage.vue](../web/src/components/SettingsPage.vue)、[OobePage.vue](../web/src/components/OobePage.vue)）
- [x] 调度/摘要与图片理解全部改走同一配置；**删除 `ModelKind` 与 `ModelRequest.model`**、`RoutingModelService`、`build_vision_service`，按用途选模型的入口彻底收敛（[routing.rs](../src/kernel/plugin/model/routing.rs)、[services/model.rs](../src/kernel/plugin/services/model.rs)）。
- [x] 余额查询去掉 SiliconFlow 专用分支（[balance.rs](../src/kernel/agent/balance.rs)；ADR-0019/0031 已修订）。
- [x] 存量配置兼容：旧 settings.json 带 `vision_model` 被忽略、启动不报错。
- [x] **已验证**：DeepSeek `deepseek-flash` 的 Responses API 原生支持图片输入（`input_image` content part，base64 data URL / http(s) URL）。
- [x] **后续（ADR-0046）**：进一步删除 `vision::read` 工具，图片以 `uploads/` 路径引用直入消息上下文（`AttachmentRef` + `AttachmentResolvingModelService`）；`grading::upload` 改为 `{items}` 只归档模型判分结果；PDF 在 GUI 边界抽文。

### 3. 加入服务端：教师端班级管理 + 出题下发（学生端登录接入）

目标形态：新增**服务端 + 学生端登录接入**；服务端带账号体系，学生端登录后从服务端拉取下发题目并同步错题。

- [ ] 账号体系：教师/学生登录，学生端登录接入，本地数据与账号绑定。
- [ ] 教师端：创建班级、管理学生（加入/移除/重置）、查看学生错题内容与掌握度。
- [ ] 出题下发：教师出题后下发给**全班或指定部分学生**；学生端接收获派作业（练习/试卷）并作答，结果回传。
- [ ] 同步：错题本/事件流 ↔ 服务端（增量上传 + 下发拉取；冲突与离线策略需定）。
- [ ] 架构决策待立 ADR：服务端技术栈、数据模型、鉴权方式、学生端（Tauri）接入路径。

### 4. 错题本优化（前端错题卡）

- [ ] **错题卡加入标题（标题由模型生成）**：卡片顶部加一行标题（现状：[MistakesPage.vue](../web/src/components/MistakesPage.vue) 卡片只有 学科/知识点 badge + 题干截断，无标题）。做法：判分归档时由模型一并生成短标题 → [src/mistake.rs](../src/mistake.rs) 新增 `title` 字段 + 判分提示词（[prompt.rs](../src/kernel/prompt.rs)）补标题字段与「一句话概括、不超过 N 字」要求 + `grading::update` 支持编辑；卡片/抽屉顶部展示。存量错题无 `title`：前端回退显示 学科 + 知识点（或按需补一次回填）。
- [ ] **小字加入 LaTeX 渲染支持**：卡片「你的作答 / 参考答案」两处小字（`.answer-strip-text`）目前是纯插值，`$x^2$` / `$\frac{1}{2}$` 原样显示；改为走 `v-html-smiles` / `renderMarkdown`（KaTeX + mhchem + DOMPurify，[markdown.js](../web/src/lib/markdown.js)），同时保留单行省略与字号样式。
- [ ] **错题正文完全拷贝到错题卡题目内容中**：归档时把原题正文**逐字完整**写入 `question`，不概括、不重写、不漏小问（现状：判分提示词只写「question（题目）」，模型可能缩写重写——[prompt.rs](../src/kernel/prompt.rs) 判分系统提示需补「题干必须逐字保留原文」约束，并核对 `grading__upload` 落库路径与卡片 2 行截断展示）。

## 任务书（2026 项目实战·任务 3）落地任务（2026-08-09 设计方案已定，决策见 ADR-0039/0040/0041）

### 基础架构改造（三个场景的地基，先做）

- [ ] **错题本目录化 + 事件流**（ADR-0039）：`mistakes/<id>/mistake.json + events.jsonl + schedule.json`；`graded`/`mastery_changed` 事件纯追加（带 subject/knowledge_point 冗余 + duration_seconds 可选）；bootstrap 迁移旧 `mistakes.json`（逐题原子拆 + 幂等 + `.bak` + backfill 事件）。
- [ ] **掌握度调度与裁决**（ADR-0040）：Anki 式 schedule.json（interval/ease/due_at，错 1 次重置 7 天 / 答对 ×2）；连错 2 次打回 is_correct；`grading::update` 升级 UserAndModel Tool 只限内容字段（前端 trigger_command 零改动）；删除/管理字段保持 UserOnly。
- [ ] **scheduler 内核插件**（ADR-0042）：`ServiceId::Scheduler` + `SchedulerHandle`（注册定时配置：interval/载荷文本/fire_on_start）；scheduler 只存配置、到点请求 kernel 核心由内核特权发起 Interrupt::Timer；kernel 核心加 pending proactive 回合队列（空闲消费）+ 白名单缺省为空 + 全局频率硬护栏。
- [x] **数据运行时化**（ADR-0042）：数据根目录 `data/` + bootstrap 种子写入（AGENTS.md 同款幂等）；`gaokao_pool.json` 真题池从 include_str! 改运行时读取（practice 连坐）；`point_deps.json` 先验依赖表启动时模型生成一次落盘固化。✅ 已落地（2026-08-10）：真题池文件优先/种子兜底 + 真实链路测试；依赖表待场景 5 落地。
- [x] **磁盘 IO 铁律**（ADR-0042）：`DomainIo`（域内文件，域枚举 + canonicalize 兜底 + 原子写 + 审计）+ `TmpIo`（temp 暂存白名单）+ `RelPath`（类型层无遍历）；memory 收编（中文路径 base64url 段编码）+ 旧布局启动迁移（`read_legacy/remove_legacy` 通道，幂等）；vision/grading 附件读写、practice 真题池全经 StorageHandle 语义方法；verify_geometry.py 维持 include_str!（代码非数据）。✅ 已落地（2026-08-10，live_api 9/9 真实链路复验）。

### 场景 3：多周期学习复盘

- [ ] **`report::weekly` 重写为 `report::overview`**（改名决策：`weekly` 名不副实——支持 daily/weekly/monthly/semester 四档后名字误导；新名绑定周期语义，将来加档位不用再改名）：加 `period` 参数（daily/weekly/monthly/semester；不传=旧行为）；semester 支持 `start_at/end_at` 可选参数（模型会话式问用户学期起止）；同步改 PROJECT.md / docs/api.md / prompts.md 工具名引用 + 前端 trigger_command 调用处，MVP 阶段直接断旧名不留兼容别名。
- [ ] 持续薄弱考点：近 N 天错 ≥3 次 且连续两期上榜（硬编码），`weakest_points[]` 加 `persistent` 标记。
- [ ] 答题时长采集：exam 计时器自动记 + 上传批改学生自述（模型填 duration_seconds）；practice::check 不采集；提示词让模型告知用户"作答计时"。
- [ ] 复习清单：report 输出 Markdown，前端「导出」= Blob 下载 .md + window.print() 打印 PDF。
- [ ] ECharts 按需打包 + `ReportChart.vue`（后端出结构化 JSON，前端只渲染）。

### 场景 4：阶段性考核验证

- [ ] `exam::compose` 加 `paper_type`（quiz/unit/midterm/final/gaokao）映射难度配置，复用 practice 出题核心（模板三档 + 真题池 + LLM 兜底）。
- [ ] 限时作答：前端计时器，到点提醒 + 自愿提交 + 真实用时记录（超时如实统计）。
- [ ] 判分：模型逐题调 `practice::check`（不新建批量入口）。
- [ ] 达标判定：卷内该知识点题数 ≥2 且得分率 ≥80% → 自动置 true + `mastery_changed(source=exam_pass)`；前端达标/待巩固可视化（ECharts 上色）。

### 场景 5：长效查漏补缺追踪

- [ ] 每学科 `mistakes/graph/<学科>.json`（sanitize + 路径校验；storage 持有文件、tracking 持有语义，全经 StorageHandle）；纯拓扑（节点 + 边 + 权重）可重建；共现层 = graded 事件带 batch_id（一次判分调用一批），同批知识点两两成边、权重 = 共现批次数（批内去重）、双层剪枝（写入宽松/查询严格）、无时间衰减；按学科隔离（无跨学科边）。
- [ ] `tracking::graph_query` 工具（UserAndModel）：输入 学科 + 知识点 → 输出 mastery/neighbors（含前置方向）/related_mistakes/recent_events（属性实时聚合自 schedule/事件流，不给结论；Agentic RAG 落地，不做向量）。
- [ ] 主动重测回合（ADR-0041/0042）：tracking 注册定时配置（30 分钟 + 载荷 + fire_on_start）→ scheduler 请求 kernel 核心 → 内核特权发起中断（回合边界排队）→ 空闲时独立 proactive 回合（不并入用户回合）；白名单缺省为空 + 模型经 `tracking::due_list` 自查（防骚扰内存态：每运行期每知识点 ≤1 次）；`tracking::dismiss` 记 24h 内存冷却；合成 user 消息（proactive 标记 + display_text 通知气泡）落当前聊天树，无活跃会话建专属提醒会话。
- [ ] 反复丢分考点聚合视图：跨快照/跨事件统计「连续两期以上均丢分」的知识点清单（数据源：事件流时间线 + schedule），供 report/tracking 输出与图谱高亮。
- [ ] 知识图谱力导向图：`tracking::graph`（UserOnly Command）→ trigger_command 拉全图拓扑 → ECharts graph 渲染。

### 加分项

- [ ] **知识图谱力导向图**：方案已定（`tracking::graph` Command → trigger_command → ECharts graph，实现见场景 5 对应项）。
- [ ] **错题本导出 Anki 卡组**：前端导出 tab 分隔文本（问题\t答案\t知识点标签\t错因），Anki「文件→导入」直接成卡组；PDF 复用复习清单打印。
- [ ] **语音提问**：MediaRecorder 录音 → SiliconFlow `audio/transcriptions`（SenseVoice）→ 文本回填输入框（用户确认后发送）；**拍照讲解**：getUserMedia 进附件管线（图片直入模型上下文，ADR-0046）。
- [ ] **手写 OCR 评测**：🔬 待测——图片理解已内化为模型能力（ADR-0046 直入上下文）；答辩兜底：用现有 3 套样例（含 1 真实手写）端到端跑通结果整理进 docs/testing.md 作鲁棒性证据，暂不建评测集。
- [ ] **家长端报表订阅**：⏸ 挂起——候选形态为设置页 PIN 家长模式 + 学情总览视图（复用 ReportChart），未排期。

### 交付物缺口（任务书必交）

- [ ] **演示视频**：5 个场景各 1-2 分钟，未产出。
- [ ] **Prompt 人工评测报告**：docs/prompts.md 有 prompt 记录但无正式人工评测报告（任务书要求"人工评测若干题"）。
- [ ] **答辩要点：LangChain/LangGraph 取舍说明**：任务书"强烈建议"LangGraph，本项目为自研 Rust kernel（PROJECT.md §2 有理由），需在技术文档/答辩中明确对比说明。
- [ ] **项目复盘报告**：任务书通用规则 D14 必交 1-2 页（做对了什么/踩了什么坑/学到什么），未产出。
- [ ] **Agent 流程图**：任务书交付物要求"源代码仓库（含 Agent 流程图、Prompt 库）"——Prompt 库已有（docs/prompts.md），缺 agent 工作流图（工具调度/会话切换/重测循环的流程图，答辩文档用）。


## Agent core 剥离为 so-lite-agent（✅ 已迁出，M5 在新仓库推进）

把通用 Agent 运行时（loop/工具注册/会话/模型 Provider 抽象/通用 RPC）剥离为独立 crate `so-lite-agent`，开箱即用（`cargo add` 即可开发新 Agent），内核/用户插件由使用方编写。完整计划见 [docs/plan/so-lite-agent.md](plan/so-lite-agent.md)（历史归档），决策见 [ADR-0037](adr/0037-so-lite-agent-crate-extraction.md)。

- ✅ M1 本仓库解耦（行为不变）：`system_prompt` 注入、`Interrupt::ConfigChanged`、错题领域类型移到 `src/mistake.rs`、RPC 通用子集 + `custom` 兜底 + `RpcExtension` + `KernelBuilder`。
- ✅ M2 本地独立 crate 骨架 `so-lite-agent/`：通用 registry/dispatch/loop/会话存储/RPC + `InMemorySessionStore` + `MockModelService`，`cargo run --example hello` 跑通 mock 回合。
- ✅ M3 Provider 层：`register_provider()` + `openai/responses/anthropic` 适配器，本地 SSE 测试通过，真实 API 测试 ignored。
- ✅ M4 插件手册/参考模板随 crate（`so-lite-agent/docs/plugin-dev/`），内核 + 用户插件双注册跑通测试。
- ✅ **迁出完成（v0.1.0，2026-08-19）**：`so-lite-agent/` 已迁出至独立仓库；本仓库不再包含该子目录（见 [CHANGELOG.md](../CHANGELOG.md) "Removed" 节）。
- ⏳ M5 发布 crates.io（0.x）：在新仓库推进；mistake-agent 后续按需切换消费（`cargo add so-lite-agent`）。

## 近期：英语练习模式（已落地）

沉浸式英语环境：开启后整个对话环境切全英文，含模型输出。

- [x] settings.json 加 `english_mode: bool`（用户独占写，默认 false；设置页开关）。
- [x] 启动/热更新时生效：`agent_system_prompt()`（[src/kernel/prompt.rs](../src/kernel/prompt.rs)）在 english_mode 下替换为英文版系统提示（或追加强指令"All replies must be in English"），全链路模型输出（含判分/出题/复盘）随主系统提示走英文。
- [x] 范围决策：判定模型指令（判分、摘要等）是否也切英文——倾向跟随（同一沉浸语境）；GUI 界面文字暂不切（只切模型对话侧，UI 留中文更安全）。
- [x] 提示词让模型在 english_mode 下判分/讲解也用英文（练习 + 答题一体）。
- [x] `agent_system_prompt` 注入英文人设（B+C 演法：全听懂中文、假装只抓英文关键词、永远只回英文并用英文引导组句）；数据根 `AGENTS.md` 中文教学规则照常注入、不翻译（保持单文件，与 AGENTS.md 加载特性共存）。

### 技术债：RPC 通用子集与扩展兜底两套架构并存

合并远程 PR #12 时 `get_rules_status`（教学规则状态查询）选择保留在通用 `Method` 枚举（`WireMethod::Generic` 直分派），而远程已将 `test_connection` / `check_balance` / `get_cache_stats` 迁入 `CustomMethod`/`WireMethod::Custom` 兜底 + `RpcExtension`（`AppRpc`，src/kernel/agent/rpc/mod.rs）。当前两套机制并存：

- 走扩展兜底（新架构）：`get_settings` / `set_settings` / `compute_result` / `test_connection` / `check_balance` / `get_cache_stats`；
- 走通用枚举（旧架构残留）：`get_rules_status`。

**待办**：后续把 `get_rules_status` 从通用 `Method` 枚举迁入 `AppRpc` 扩展，统一走 `CustomMethod` 兜底，彻底移除通用枚举对业务方法的依赖。迁移时同步删 `Method::GetRulesStatus` 枚举变体与 `handlers.rs` 对应分支，前端 wire 不变（`{method:"get_rules_status"}` 仍兼容）。

## 近期：桌面输入方式增强（剪贴板已落地，摄像头未落地）

- [x] **剪贴板粘贴截图**：WebView 监听 `paste`（Ctrl+V / 右键粘贴），图片直接进入附件暂存，与「选择作业文件」共用 附件 → 判分归档管线。✅ 已落地（2026-08-17）：新增 `stage_clipboard_image` Tauri 命令（`stage_bytes` 与选文件共用落盘），ChatPage 根节点 `@paste` 监听，粘贴截图入 `pendingAttachments` 走同一暂存/判分管线。
- [ ] **摄像头拍题**：调用 WebView `getUserMedia` 拍题入队，拍完即走同一条 OCR 管线；需处理 WebView2 相机权限与设备选择。

## 中期：Android 手机 / 平板适配（规划，未落地）

- Tauri v2 增加 Android target：移动端壳、触控/窄屏响应式适配、相册/摄像头/剪贴板输入、Pyodide 在移动 WebView 的可用性与性能验证、移动端存储路径与权限模型、离线包体积控制。构建不依赖 macOS（Windows 装 Android SDK 即可）。

## 长期：iOS / iPadOS 适配（规划，未落地）

- 在 Android 落地后追加 iOS/iPadOS target：Apple 相机/相册/剪贴板权限、平台差异收敛到统一能力层。
- **本机无 macOS 的解法**：构建/签名/发布走云 macOS——优先 GitHub Actions macOS runner（本仓库公开，macOS 构建免费额度），签名证书与描述文件以仓库 secrets 托管，CI 出 ipa 后上传 App Store Connect；备选 Codemagic / MacStadium 云 Mac。

## OOBE 初始化数据根目录（已完成）

[src/kernel/bootstrap.rs](../src/kernel/bootstrap.rs) 的 `init_data_root` 在 `Kernel::new` 引导阶段与 `set_settings` 保存路径中执行（幂等）：创建数据根目录及 `sessions/ mistakes/ memory/ audit/ logs/ uploads/` 六个子目录；`AGENTS.md` 缺失时写入默认教学规则模板（存在不覆盖）。storage/logger/memory 各自的懒创建已收敛到 bootstrap。


## AGENTS.md 加载进系统提示（已完成）

- [x] `agent_system_prompt()` 加载数据根目录 AGENTS.md 全文进系统提示（`load_agents_md`，PROJECT.md §6 指令加载 / ADR-0011 / ADR-0012）
- [x] 缺失（Missing）/ 损坏（InvalidUtf8）/ 超限（TooLarge，64KB 上限）时回退静态文本；路径仅由数据根目录拼接固定文件名（无用户输入路径、无遍历面）
- [x] 设置页「教学规则」卡片：`get_rules_status` RPC 展示规则已加载/回退状态与原因 + 「打开规则文件」按钮（`open_rules_file` Tauri 命令，系统默认程序打开，复用 `open_with_system`）
- [x] 单测 6 项（正常/缺失/超限/编码 + 拼接回退 + reason 标签）

现状：AGENTS.md（教学规则，家长/老师可编辑）全文注入主模型系统提示（在静态基底之后、debug 段之前），文件保存后下个请求即时生效；前端设置页可查看加载状态并一键打开编辑。


## 前端工具元数据去硬编码（已完成）

[web/src/lib/tools.js](../web/src/lib/tools.js) 建立工具目录模块：启动时经 `list_tools` 拉取一次并缓存 `entry → {title, icon, group}`，`toolIcon` / `toolTitle` / `toolList` 都从它取；`messages.js` 的 `TOOL_ICONS` / `TOOL_TITLES` 已删除，渲染与 `FORCED_RE` 还原统一走目录（缺失回退 entry 名）。ChatPage 与 SessionsPage 共用该模块，前端不再维护工具名 → 展示信息映射。
