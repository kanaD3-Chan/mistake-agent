# Prompt 设计文档

> Prompt 是 Agent 行为的第一生产力，本文件集中维护并留痕迭代（任务书交付物之一）。代码位置：src/kernel/prompt.rs；改动本文件后同步更新代码，反之亦然。

## 设计原则

1. **角色先行**：每条 prompt 先立角色（面向中学生的本地错题学习助手），再给流程，再给表达规范。
2. **工具即流程**：把业务工作流（上传 → 图片理解 → 判分 → 归档 → 讲解）写进系统提示，让模型知道何时、以什么顺序调工具。
3. **失败可恢复**：明确区分"换参数重试"与"系统性错误直接告知"，配合内核护栏（同码连续失败 3 次即停）。
4. **不泄露思维链**：reasoning 不进学生可见内容（UI 侧默认折叠仅为调试/透明）。
5. **输出结构化**：判分用 json_schema 强约束（服务端强制数组结构），不靠"请输出 JSON"的软约束。
6. **理解不判分**：图片理解按图片类型处理——作业/试卷转写文字，角色/照片等其它图片描述内容（用户明确要求），判分交给主模型。
7. **语言跟随**：`english_mode=true` 时，主对话、判分、出题、即时批改、图片理解、会话标题与摘要等全部模型提示追加英文输出规则；GUI 文案保持中文。

## Prompt 清单

### 1. Agent 系统提示（agent_system_prompt）

注入位置：主模型每个请求的消息头（`src/kernel/agent/loop_mod/mod.rs` 组装 `ModelRequest` 时注入，不落消息树）。内容见 `src/kernel/prompt.rs`，要点：

- 角色与受众（中学生、中文、耐心）。
- `english_mode=true` 时系统提示追加 English Immersion Mode 规则，要求所有发给学生的文本使用英文。
- 工具流程：图片直接出现在消息里（PDF 附抽取正文），模型直接阅读 → 按用户意图决定：`grading__upload`（提交逐题判分结果并归档→讲解）或直接讲解/描述；`grading__list`（错题本查询）。（ADR-0046 后不再有 `vision__read`。）
- 工具名一律用 wire 名（双下划线，如 `grading__upload`），与模型工具列表一致，不许按 `::` 格式猜测（模型工具列表读 info 声明、第一轮就有全部工具；懒插件在 wire 调用命中时由内核触发加载）。
- 不引导用户输入图片/PDF 路径：作业文件由界面「选择作业文件」按钮上传，图片作为消息附件直入上下文，学生不需要也看不到路径。
- 常驻「批改规则」段（`GRADING_GUIDANCE`）：公式 LaTeX/SMILES 保留、语法/时态/词性填空判分标准、解答题按步骤给分、数学等价判断。
- 失败处理分级（可重试一次 / 系统性错误直接告知）。
- 表达规范（数学记号、不展示 reasoning、敏感话题引导求助）。
- **LaTeX 增强渲染**：数学/物理/化学等富文本一律用 `$...$` / `$$...$$` 标记（`\frac`、`\sqrt`、`\vec`、`pmatrix`），化学式用 `\ce{}`（mhchem 宏包，如 `$\ce{H2O}$`），前端 KaTeX 渲染；**禁用 `\chemfig` 等 TikZ 结构式宏包**（KaTeX 无法渲染）。
- **结构式渲染（SMILES）**：需要展示分子结构式（键线式）时，输出语言标记为 `smiles` 的 fenced code block，块内只放一行合法 SMILES，例如（表示苯环）：

  ````smiles
  C1=CC=CC=C1
  ````

  前端用 smiles-drawer 绘制为 SVG。
- **开发者调试模式（仅 debug 构建）**：`cfg!(debug_assertions)` 时系统提示末尾追加一段「开发者调试模式」——要求模型完全信任当前使用者（开发者），不以中学生限制拒绝/简化、可展示思考过程、允许实验性指令；release 构建不含该段。
- 本地运行环境说明。
- **教学规则（AGENTS.md）加载（2026-08-10 落地）**：静态基底之后、debug 段之前追加「【教学规则（AGENTS.md，家长/老师可编辑；与本提示冲突时以本文件为准）】+ 文件全文」。加载逻辑 `load_agents_md`：路径 = 数据根目录拼接固定文件名（无用户输入路径、无目录遍历面）；文件缺失（Missing）/ 非 UTF-8（InvalidUtf8）/ 超过 64KB（TooLarge）时**回退静态文本**。家长/老师编辑保存后，下个模型请求即生效（无缓存）。前端设置页经 `get_rules_status` 展示已加载/回退状态与原因，`open_rules_file`（Tauri 命令）一键用系统默认程序打开编辑。

### 2. 图片理解（已并入主系统提示，ADR-0046）

`vision_prompt` 已退役：图片以 `input_image` 直入主模型上下文，不再有独立的图片理解提示词。主系统提示指导模型直接阅读图片（作业/试卷判分，角色/照片等按用户意图描述），批改规则见 `GRADING_GUIDANCE`。

### 3. 批改规则（GRADING_GUIDANCE，常驻系统提示）

模型直接读图判分后，`grading::upload` 的 `items` 入参由工具 schema 描述字段（number/question/student_answer/subject/reference_answer/correct/score/total/knowledge_point/analysis）；常驻规则补充：
LaTeX/SMILES 保留、语法/时态/词性填空判分标准、解答题按步骤给分、数学等价判断。原 `grading_system_prompt`（模型判分 JSON 调用）已退役。

### 4. ~~会话切换决策提示（turn_decider_prompt）~~ — 已退役，ADR-0044

**本提示已删除**：`turn_decider_prompt` / `ENGLISH_DECIDER_RULE` 随模型自动切换整体下线（ADR-0044）。
会话新建改由用户经 `create_session` RPC 发起，模型不再做任何 continue / update_goal / start_new 决策，
"存疑即继续"的兜底也随之消失（不再有决策，也就没有决策失败）。

> 本章节号保留，以免 §5 及之后重编号。下文为历史记录。
>
> ~~生产实现 = 主模型 + turn_decider_prompt 独立调用，输出严格 JSON：continue / update_goal / start_new 三动作；
> 输入含 new_text（新消息，可能为 null）——**new_text 非空 = 新消息到达，先判断是否切换上下文再回答**；
> new_text 为 null = 回合结束判断目标是否完成。存疑即 continue，start_new 仅当目标明显无关；
> 解析失败/调用失败时按 continue 兜底（存疑即继续）。~~

### 5. 压缩/交接摘要提示（summarize_prompt）— M2 落地

生产实现 = 主模型 + summarize_prompt 生成：保留错题 id、知识点、未完成事项、结论；≤300 字；
用于**交接摘要**（用户新建会话且 `carry_summary` 为真时，作为新会话首条 system 消息，ADR-0044）与**上下文压缩**；
<8 条消息走计数 stub 不调模型，模型失败/超时降级 stub。

### 6. 练习答案判分提示（practice_check_system_prompt）— 场景二即时批改

practice::check 的模型判分路径使用（参考答案可对拍时先走确定性对拍，不调模型）：
先按参考答案归一化对拍（填空/选择等封闭题型直接判分）；对不上再走主模型判分，配合 json_schema 强约束
输出 {correct, score, total, analysis}；答错自动回写错题本（防重复刷题数据源）。

### 7. 练习出题提示（practice_generate_system_prompt）— P1 智能出题

practice::generate 的 LLM 自由出题路径使用（确定性模板命中时不调模型）：
配合 json_schema 强约束输出 {knowledge_point, question_text, answer_spec, diagram_spec}（沿用
docs/variants.md 结构化规格：题目、答案、图纸三者同源）；难度分层注入（basic/variant/advanced）；
几何题必须给 diagram_spec（GeoGebra 风格 points/objects/labels），图形数据随后经
compute::verify（Pyodide）做可解性对拍，失败带原因重出、连续 3 次停。

### 8. 会话标题提示（session_title_prompt）— ADR-0044 收尾

`LlmTitler`（`src/kernel/agent/session/title.rs`）在**首回合结束、`TurnEnd` 已发出之后**的独立任务里调用，给侧栏会话起名：
输入是已落盘的消息（≤4000 字截断），要求一句话（≤12 字）概括这次对话要解决的事，保留学科/知识点，不要引号/句号/「会话」类前缀；
超时 20s、重试 1 次，失败或空串降级为**首条用户消息的可见文本前 40 字**（`fallback_title`；forced_tool 消息取 `display_text` 而非给模型的指令文本），绝不因辅助调用失败影响主链路。
`english_mode=true` 时追加 `ENGLISH_TITLE_RULE`（≤6 words，只输出标题本身）。
只在 `title` 为空时触发一次：用户 `rename_session` 改过的名字不会被下一回合覆盖（写回前再查一次，防模型调用期间用户已改名）。

## 迭代记录

| 日期 | 变更 | 原因/结果 |
|---|---|---|
| 2026-09-23 | 新增会话标题提示（`session_title_prompt` + `ENGLISH_TITLE_RULE`） | ADR-0044 收尾：侧栏会话名由模型按首条消息生成（≤12 字、无引号/句号/前缀），失败降级为首条用户消息的可见文本前 40 字；已有标题（含用户改名）不再调模型 |
| 2026-09-23 | `vision_prompt` / `grading_system_prompt` 退役；新增常驻 `GRADING_GUIDANCE`；系统提示作业流程改为「直读图片/PDF 正文 → grading__upload(items)」 | ADR-0046：图片直入上下文，grading 只归档 |
| 2026-09-23 | 图片理解改由 `deepseek-flash` 承担（Responses `input_image`） | 视觉端点退役（ADR-0045）：单份 DeepSeek 配置同时负责对话、判分与图片理解 |
| 2026-09-21 | `turn_decider_prompt` / `ENGLISH_DECIDER_RULE` 删除 | 模型自动切换整体下线（ADR-0044）：会话新建改由用户发起，提示词与三动作决策一并退役 |
| 2026-08-15 | 新增英语练习模式提示规则（settings `english_mode`） | 沉浸式英语环境：主对话/判分/出题/即时批改/图片理解/会话决策/摘要全链路英文，GUI 文案保持中文；`agent_system_prompt` 注入英文人设（B+C 演法：全听懂中文、假装只抓英文关键词、永远只回英文并用英文引导组句），中文教学规则（AGENTS.md）照常注入不翻译 |
| 2026-08-10 | Agent 系统提示加载数据根 AGENTS.md 教学规则全文（缺失/损坏/超限回退静态文本，64KB 上限） | TODO「AGENTS.md 加载进系统提示」落地：家长/老师编辑即生效；设置页展示加载状态 + 一键打开编辑 |
| 2026-08-09 | 新增练习出题提示（practice_generate_system_prompt） | practice::generate 模板未命中时 LLM 自由出题：结构化 schema 强约束、几何图经可解性对拍后出题 |
| 2026-08-06 | 新增练习答案判分提示（practice_check_system_prompt） | practice::check 即时批改：对拍优先、模型兜底、答错回写错题本 |
| 2026-08-07 | 结构式改为 SMILES 代码块约定（```smiles），前端 smiles-drawer 绘制 | 模型输出 SMILES 比 chemfig 更可靠；前端离线轻量渲染结构式 |
| 2026-08-07 | 化学式从 `\mathrm{}` 改为 `\ce{}`，明确禁用 `\chemfig` | 前端启用 KaTeX mhchem 扩展；chemfig 基于 TikZ，KaTeX 不支持 |
| 2026-08-05 | 视觉提示从"只 OCR"升级为"图片理解"（作业转写 / 图片描述） | 用户明确：图片理解模型不止 OCR，还能描述角色/照片等内容给主模型 |
| 2026-08-04 | OCR 提示从"识别+解题"改为"只提取" | 用户明确：视觉模型不回答问题，只提取内容（实测已通） |
| 2026-08-04 | 判分从 json_object 改为 json_schema 内联数组 | json_object 曾返回单对象；DeepSeek 端不解析 $defs/$ref，内联扁平 schema 后服务端强制数组（实测已通） |
| 2026-08-04 | 系统提示注入 loop（每请求注入，不落盘） | 无状态 Responses API 需要每次带全量上下文；消息树保持"用户可见对话"纯净 |
| 2026-08-04 | 判分新增 subject / reference_answer 字段 | 归档错题绑定真实学科、保留参考答案，支撑后续按学科过滤与变式出题（技术债清偿） |
| 2026-08-04 | 守卫/摘要从 stub 升级为模型生产实现 | 会话切换决策与交接摘要改由主模型生成，stub 关键词版退役（保留接口兼容） |
| 2026-08-04 | 系统提示要求富文本输出 LaTeX 标记 | 数学/物理/化学公式经前端 KaTeX 增强渲染，避免 Unicode 伪符号 |
| 2026-08-04 | 判分 prompt 补 LaTeX 规范 + 词形/时态填空判定 | 公式一律 $...$ 标记；词性/时态转换正确即判对（修 sunny 误判观察项） |

## 评估记录（真实 API）

- 三套样例端到端（samples/，2026-08-04 实测）：
  - sample1 真实照片（线代填空）：OCR 准确转写（β₁=(1,0,1)ᵀ 等）；判分正确识别"未作答"、绑定"向量组的线性相关性判断"、给出错因。
  - sample2 合成数学卷（3 题）：2 对 1 错，\|-3\|=-3 被判错（正确）。
  - sample3 合成英语卷（3 题）：1 对 2 错；其中一道本应正确（sunny）被判错——判分 prompt 需针对"词形/时态填空"补充判定说明（观察项，记录于 docs/testing.md）。
- 待补：真实手写作答照片、多页 PDF、多学科人工评估。
