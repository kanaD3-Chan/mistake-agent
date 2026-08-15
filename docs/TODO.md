# TODO

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
- [ ] **语音提问**：MediaRecorder 录音 → SiliconFlow `audio/transcriptions`（SenseVoice）→ 文本回填输入框（用户确认后发送）；**拍照讲解**：getUserMedia 进附件管线（vision::read）。
- [ ] **手写 OCR 评测**：🔬 待测——vision::read 功能已覆盖；答辩兜底：用现有 3 套样例（含 1 真实手写）端到端跑通结果整理进 docs/testing.md 作鲁棒性证据，暂不建评测集。
- [ ] **家长端报表订阅**：⏸ 挂起——候选形态为设置页 PIN 家长模式 + 学情总览视图（复用 ReportChart），未排期。

### 交付物缺口（任务书必交）

- [ ] **演示视频**：5 个场景各 1-2 分钟，未产出。
- [ ] **Prompt 人工评测报告**：docs/prompts.md 有 prompt 记录但无正式人工评测报告（任务书要求"人工评测若干题"）。
- [ ] **答辩要点：LangChain/LangGraph 取舍说明**：任务书"强烈建议"LangGraph，本项目为自研 Rust kernel（PROJECT.md §2 有理由），需在技术文档/答辩中明确对比说明。
- [ ] **项目复盘报告**：任务书通用规则 D14 必交 1-2 页（做对了什么/踩了什么坑/学到什么），未产出。
- [ ] **Agent 流程图**：任务书交付物要求"源代码仓库（含 Agent 流程图、Prompt 库）"——Prompt 库已有（docs/prompts.md），缺 agent 工作流图（工具调度/会话切换/重测循环的流程图，答辩文档用）。


## Agent core 剥离为 so-lite-agent（M1-M4 已落地，M5 未落地）

把通用 Agent 运行时（loop/工具注册/会话/模型 Provider 抽象/通用 RPC）剥离为独立 crate `so-lite-agent`，开箱即用（`cargo add` 即可开发新 Agent），内核/用户插件由使用方编写。完整计划见 [docs/plan/so-lite-agent.md](plan/so-lite-agent.md)，决策见 [ADR-0037](adr/0037-so-lite-agent-crate-extraction.md)。当前 M1-M4 已落地，M5 待办。

已落地：
- M1 本仓库解耦（行为不变）：`system_prompt` 注入、`Interrupt::ConfigChanged`、错题领域类型移到 `src/mistake.rs`、RPC 通用子集 + `custom` 兜底 + `RpcExtension` + `KernelBuilder`。
- M2 本地独立 crate 骨架 `so-lite-agent/`：通用 registry/dispatch/loop/会话存储/RPC + `InMemorySessionStore` + `MockModelService`，`cargo run --example hello` 跑通 mock 回合。
- M3 Provider 层：`register_provider()` + `openai/responses/anthropic` 适配器，本地 SSE 测试通过，真实 API 测试 ignored。
- M4 插件手册/参考模板随 crate（`so-lite-agent/docs/plugin-dev/`），内核 + 用户插件双注册跑通测试。

未落地：
- M5 发布 crates.io（0.x），mistake-agent 切换到新 crate 消费并删除重复代码。

## 近期：英语练习模式（已落地）

沉浸式英语环境：开启后整个对话环境切全英文，含模型输出。

- [x] settings.json 加 `english_mode: bool`（用户独占写，默认 false；设置页开关）。
- [x] 启动/热更新时生效：`agent_system_prompt()`（[src/kernel/prompt.rs](../src/kernel/prompt.rs)）在 english_mode 下替换为英文版系统提示（或追加强指令"All replies must be in English"），全链路模型输出（含判分/出题/复盘）随主系统提示走英文。
- [x] 范围决策：判定模型指令（判分、摘要等）是否也切英文——倾向跟随（同一沉浸语境）；GUI 界面文字暂不切（只切模型对话侧，UI 留中文更安全）。
- [x] 提示词让模型在 english_mode 下判分/讲解也用英文（练习 + 答题一体）。

## 近期：桌面输入方式增强（规划，未落地）

- **剪贴板粘贴截图**：WebView 监听 `paste`（Ctrl+V / 右键粘贴），图片直接进入附件暂存，与「选择作业文件」共用 vision__read → 判分归档管线。
- **摄像头拍题**：调用 WebView `getUserMedia` 拍题入队，拍完即走同一条 OCR 管线；需处理 WebView2 相机权限与设备选择。

## 中期：Android 手机 / 平板适配（规划，未落地）

- Tauri v2 增加 Android target：移动端壳、触控/窄屏响应式适配、相册/摄像头/剪贴板输入、Pyodide 在移动 WebView 的可用性与性能验证、移动端存储路径与权限模型、离线包体积控制。构建不依赖 macOS（Windows 装 Android SDK 即可）。

## 长期：iOS / iPadOS 适配（规划，未落地）

- 在 Android 落地后追加 iOS/iPadOS target：Apple 相机/相册/剪贴板权限、平台差异收敛到统一能力层。
- **本机无 macOS 的解法**：构建/签名/发布走云 macOS——优先 GitHub Actions macOS runner（本仓库公开，macOS 构建免费额度），签名证书与描述文件以仓库 secrets 托管，CI 出 ipa 后上传 App Store Connect；备选 Codemagic / MacStadium 云 Mac。

## OOBE 初始化数据根目录（已完成）

[src/kernel/bootstrap.rs](../src/kernel/bootstrap.rs) 的 `init_data_root` 在 `Kernel::new` 引导阶段与 `set_settings` 保存路径中执行（幂等）：创建数据根目录及 `sessions/ mistakes/ memory/ audit/ logs/ uploads/` 六个子目录；`AGENTS.md` 缺失时写入默认教学规则模板（存在不覆盖）。storage/logger/memory 各自的懒创建已收敛到 bootstrap。


## AGENTS.md 加载进系统提示（未完成）

现状：AGENTS.md（教学规则，家长/老师可编辑）已完成初始化写入（见上一条），但内核系统提示仍是静态文本（[src/kernel/prompt.rs](../src/kernel/prompt.rs) agent_system_prompt()），文件内容对模型行为暂无影响。

目标：
- agent_system_prompt() 改为加载数据根目录 AGENTS.md 全文进系统提示（PROJECT.md §6 指令加载 / ADR-0011 / ADR-0012）
- 缺失、损坏或超限时回退当前静态文本；路径校验仅限数据根目录内（参照 bootstrap::init_data_root）
- 建议与设置页「教学规则」编辑入口（或「打开规则文件」按钮）配套落地，前端展示规则已加载状态
- 优先级：中（MVP 不阻塞，静态提示词已覆盖核心教学流程）

参考：PROJECT.md §6 指令加载；[docs/adr/0011-single-data-root.md](adr/0011-single-data-root.md)、[docs/adr/0012-no-skill-system-v2.md](adr/0012-no-skill-system-v2.md)。


## 前端工具元数据去硬编码（已完成）

[web/src/lib/tools.js](../web/src/lib/tools.js) 建立工具目录模块：启动时经 `list_tools` 拉取一次并缓存 `entry → {title, icon, group}`，`toolIcon` / `toolTitle` / `toolList` 都从它取；`messages.js` 的 `TOOL_ICONS` / `TOOL_TITLES` 已删除，渲染与 `FORCED_RE` 还原统一走目录（缺失回退 entry 名）。ChatPage 与 SessionsPage 共用该模块，前端不再维护工具名 → 展示信息映射。
