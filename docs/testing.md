# 测试文档

## 1. 测试策略

- **单元测试**：`cargo test`（142 项），覆盖注册表校验、dispatch、session 调度（守卫/摘要/分支/压缩/中断）、storage（文件/内存/DomainIo/TmpIo/迁移）、memory（文件 CRUD/路径越界/旧布局迁移）、model（SSE/usage 解析）、settings（patch/public_view）、prompt（英语模式规则）、compute 桥接与 handler、插件入口（schema/模板/聚合）。
- **真实 API 集成测试**：`cargo test --test live_api -- --ignored --nocapture`，直接接 DeepSeek/SiliconFlow（无 key 自动跳过）。
- **样例端到端**：`samples/` 三套作业图片逐一走 上传→OCR→判分→归档 全链路。
- **前端自检**：`cd web && npm run check:pyodide`（真实加载 Pyodide WASM 并执行 Python：算术、符号计算（sympy 解方程/求导/积分）、物理（单位换算/运动学）、numpy 数值、异常路径）；`node scripts/katex-check.mjs`（KaTeX 行内/块级/化学式/矩阵/非法公式容错）。

## 2. 用例与结果（2026-08-10 实测）

### 单元测试：142 项全过

| 模块 | 覆盖点 |
|---|---|
| registry | namespace 撞名、wire 撞名、requires 不可满足、懒注册 |
| dispatch | 注册链路、命令回退同名工具 |
| session | 首消息建会话、空闲超时/start_new/session::switch 树内分叉（摘要节点+新子树）、守卫决策（stub+LLM 解析失败保底）、消息级分支派生/切分支、上下文裁剪（scope_session_context）、压缩摘要、InterruptBus |
| storage | 错题 CRUD、会话追加/归档、active_path/derive_branch/splice_compaction |
| memory | 文件 CRUD、目录浏览、子树删除、路径校验（绝对/../空段）、中文路径编码与旧布局迁移 |
| storage IO | `RelPath` 遍历向量、域隔离、TmpIo temp 白名单、运行时真题池文件优先/种子兜底 |
| model | SSE 解析、usage 解析（response.usage 顶层）、ToolCall 展开 |
| settings | patch 校验、public_view 不含 key、english_mode 补丁生效 |
| prompt | english_mode 开启时各提示词追加 English Immersion Mode 规则 |
| compute | BridgeCompute 回执/取消 |
| 插件 | 12 插件入口参数 schema、practice 模板生成、report/exam/tracking 聚合断言 |

### 真实 API 链路

当前 `live_api` 共 9 个 ignored 用例，使用本地 `settings.json` 中的真实主模型/视觉模型配置；复验结果为 9/9 通过。

| 用例 | 结果 |
|---|---|
| hello 回合（send_user_message → Responses API 流式） | ✅ 通过；**断言 assistant 消息已落盘、审计 llm_call 的 tokens 非空** |
| 三套样例批改（grading::upload） | ✅ 通过（subject/reference_answer 已入库） |
| memory 工具往返（save/show/remove + 文件落盘） | ✅ 通过 |
| LaTeX 输出（模型按 prompt 输出 $...$ 公式） | ✅ 通过（勾股定理，$a$/$b$/$c$） |
| compute::verify 全链路（事件→回执→工具成功→模型续答） | ✅ 通过：测试模拟 GUI 执行端回执固定 stdout，kernel→桥→回执→续答闭环 |

| 样例 | 类型 | 题数 | 对 | 错 | 归档 | 备注 |
|---|---|---|---|---|---|---|
| sample1_linalg_real.png | 真实照片（线代填空） | 1 | 0 | 1 | 1 | 未作答判错，知识点"向量组的线性相关性判断" |
| sample2_math_synthetic.png | 合成数学卷 | 3 | 2 | 1 | 1 | \|-3\|=-3 判错正确 |
| sample3_english_synthetic.png | 合成英语卷 | 3 | 1 | 2 | 2 | 见观察项 #1 |

### 门禁

`cargo fmt --check` ✅ ｜ `cargo clippy --all-targets -- -D warnings` ✅ ｜ `cargo test` ✅ ｜ GUI 冒烟（Wayland 下启动 8s 无崩溃）✅

## 3. Bug / 观察列表

| # | 现象 | 根因 | 状态 |
|---|---|---|---|
| 1 | 英语合成卷 3 题中 1 道"本应正确"被判错 | 模型判分歧义（"sunny" vs "sun" 时态/词性判断），非链路故障；属模型行为，后续用判分 prompt 与样例校准 | 观察中 |
| 2 | 回合任务独立于 JoinSet，main 返回触发运行时关闭 | 关闭时序导致回合被取消（DNS task cancelled） | 已修：关闭前轮询 `is_idle` 再退出 |
| 3 | Tauri Channel<String> 交付字符串，UI 当对象用导致事件全丢 | 桥接类型不匹配 | 已修：JS 侧 JSON.parse |
| 4 | 无 IPv6 环境 reqwest 连接失败 | 解析到 v6 后无回退 | 已修：客户端强制 IPv4 本地地址 |
| 5 | flexi_logger 重复初始化报错 | 全局 logger 只能 init 一次 | 已修：OnceLock 幂等 |
| 6 | DeepSeek json_schema 返回 schema 原文 | schema 含 $defs/$ref 不被解析 | 已修：内联扁平数组 schema |
| 7 | tokio stdout 写管道丢失 | 环境差异 | 已修：帧写入改同步 stdout（协议通道，绝不含日志） |
| 8 | 会话 JSONL 只有 user 消息、审计 tokens 全 None | SSE 事件映射未命中 usage（usage 在 `response.usage` 顶层） | 已修：completed/incomplete 事件解析 response.usage；live_api 加落盘+usage 断言 |
| 9 | Method::ComputeResult 的 id 与 RPC 顶层 id 撞名 | serde flatten 字段冲突 | 已修：rename `compute_id`，前端按 compute_id 回执 |
| 10 | 工具调用回合报"reasoning_text must be passed back" | 三层原因：①只回传 id 丢文本；②**并行调用时一个 reasoning 只覆盖第一个 function_call**（实测 DeepSeek 要求每个调用前都有 reasoning）；③流式 delta 先于 item start 时文本丢失 | 已修：loop 累积 id+text 并防御 delta 乱序；`messages_to_responses_input` 回传文本并**按调用复制 reasoning**；再被拒时兜底剥离 reasoning + `effort=none` 重试；真实 API 复验通过 |
| 11 | 会话切换频率超限时丢消息/归档错乱 | 归档后才检查切换频率 | 已修：频率检查前置，超限降级 continue（消息不丢） |
| 12 | 回合失败后界面/状态未恢复 | 失败时未发 turn_end | 已修：失败发 `turn_end(failed)` + error，前端恢复可聊天 |
| 13 | DeepSeek 503 导致守卫/摘要/回合失败 | 无重试 | 已修：守卫/摘要对瞬时错误重试 2 次（线性退避），主回合流重试 1 次；系统性错误（无余额/模型下架）不重试直接降级；单测模拟 503→成功通过 |
| 14 | 工具调用回合报"reasoning_text must be passed back"（批改失败） | **真实根因是 call_id 不匹配**：loop 丢弃首轮 function_call 的真实 call_id，第二轮回填用随机 uuid；DeepSeek 对错误 call_id 的报错信息误导为 reasoning | 已修：ToolCall 消息保存真实 call_id（tool_call_with_id），回传时优先使用；保留 reasoning 回传（无害）；真实批改多轮验证通过 |
| 15 | 会话切换后上下文/历史断裂 | 切换只注入摘要，模型记不住之前对话 | 已修：切换 = 树内分叉——当前节点下挂「摘要节点 + 新会话子树」，摘要作为上下文边界，旧分支保留为兄弟版本（GUI < / > 可切回） |

## 4. 成本观察（真实调用）

- 视觉 OCR（Qwen3-VL-32B）：线代题图 prompt 267 / completion 815 tokens。
- 主模型判分（deepseek-v4-flash，thinking=none）：每题约 1~4 秒，json_schema 强制结构化。

## 5. 安全测试

- XSS：Markdown 输入含 `<script>`、事件属性、`javascript:` 链接时，DOMPurify 净化后不执行（渲染层唯一 v-html 入口，其余文本均走 Vue 插值自动转义）。CSP：`script-src 'self'`，无内联脚本。
- XXE：当前全栈无 XML 解析（无 SAX/DOM/外部实体处理）；PDF 用 lopdf 二进制解析；Markdown/HTML 渲染不解析 XML。若未来引入 SVG/XML 上传，须先过 DOMPurify 或专用解析器白名单。

## 5. 待补（记录在案）

- 至少 3 套**真实手写作业照片**端到端（当前 1 真实 + 2 合成）。
- 多页 PDF（含图片页）渲染 OCR。
- 判分质量评估：多学科样例人工核对（任务书要求 Prompt 评估报告）。
