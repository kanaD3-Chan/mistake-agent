# AGENTS.md

## 项目速览

Mistake Agent v2：面向中学生的本地错题管理 + 辅助学习 Agent（Windows 桌面应用，Tauri GUI + 自研 Rust kernel，Rust 2024 edition，mistake-agent 本体单 crate；ADR-0037 的 `so-lite-agent/` 为独立 crate 骨架）。

**动任何代码之前，先读一遍 [PROJECT.md](PROJECT.md)**——它是唯一入门文档，包含架构、信任模型、机制、命名规范、里程碑和分工。

## Agent 启动流程（每次开始工作前执行）

按固定顺序读文档，**读完必须先问协作开发者本次要做什么，确认范围后再动手**：

1. **PROJECT.md** —— 项目全貌：架构、信任模型、机制、命名规范、里程碑、分工
2. **CONTEXT.md** —— 术语表；PROJECT.md 里不懂的词在这里查
3. **docs/TODO.md** —— 当前待办与已定方案（很多任务的方案已在 TODO 里定好，先看再问）
4. **docs/adr/** —— 决策留痕；重点读与任务相关的条目（改设计必须新增 ADR，见开发约定）

读完后：向协作开发者确认本次任务（**做什么 / 范围 / 验收标准**），确认后才开始动手；任务相关细节再按下方「读文档路由」精确定位。

## 读文档路由：做什么 → 读什么

| 你要做什么 | 先去读 | 重点内容 |
|---|---|---|
| 刚加入项目 / 开始新任务 | 按「Agent 启动流程」读 PROJECT.md → CONTEXT.md → docs/TODO.md → docs/adr/ | 全貌、术语、待办、决策；不懂的词去 CONTEXT.md 查 |
| 改设计 / 做架构决策 | docs/adr/ 全部 + CONTEXT.md | 决策留痕；新决策要新增 ADR 并更新术语 |
| 看后续计划 / so-lite-agent 剥离 | docs/plan/so-lite-agent.md + docs/adr/0037 | M1/M2 已落地，M3-M5 待办；mistake-agent 本体在 M5 前仍单 crate |
| 改内核机制（loop/调度/注册表） | PROJECT.md §4-§5 + docs/adr/0003~0010 | 两段式契约、CallerPolicy、护栏、容灾 |
| 改会话 / 消息树 | PROJECT.md §5 会话 + docs/adr/0006、0007 | 双层调度、守卫模型、Goal、历史路由 |
| 改内核插件（services） | PROJECT.md §4-§5 + docs/adr/0001、0014、0015、0016 | 服务句柄、ModelHandle、配置独占、compute 桥接 |
| 改记忆 | PROJECT.md §5 记忆 + docs/adr/0008 | memory::save/show/remove、路径校验、路由式浏览 |
| 改用户插件（plugin/） | PROJECT.md §3、§12 + docs/adr/0002、0003 | 五场景、命名规范、入口点、注册校验 |
| 改 practice 出题（变式/真题/几何校验） | docs/variants.md + PROJECT.md §3、§9 | 出题架构与落地状态、未落地项 |
| 改 GUI / 协议 | PROJECT.md §5 通信 + docs/adr/0013 | trigger_command 唯一命令通道、事件流 |
| 改模型 / 设置 | PROJECT.md §6 + docs/adr/0015、0019 | 双模型配置、用户独占写、明文 key 取舍 |
| 改审计 / 日志 | PROJECT.md §5 审计日志 + docs/adr/0017、0018 | 全覆盖审计、分级日志、脱敏 |
| 抄开源代码 | 该项目 LICENSE + PROJECT.md §2 开源策略 | 保留许可声明、注明来源；机制可抄，业务自写 |
| 写测试 | PROJECT.md §10 里程碑验收标准 + 各模块 tests | 按验收标准补测试 |

## 常用命令

```bash
cargo check        # 快速检查
cargo test         # 单元测试
cargo clippy -- -D warnings
cargo fmt --check
```

## 架构红线（改代码时逐条遵守）

- mistake-agent 本体单 crate：`src/kernel/`（内核）与 `src/plugin/`（用户插件）分区，**M5 切换前不再新增 crate 拆分**；`so-lite-agent/` 是 ADR-0037 允许的独立通用运行时 crate（M1/M2 已落地，M3-M5 待办）
- 能力边界：内核实现用 `pub(crate)` 隐藏；用户插件只经公开 API 面交互；不引入全局可变状态绕过句柄
- CallerPolicy：`UserAndModel` 工具必须配同名用户入口；`UserOnly` 不得进入模型工具列表
- 入口点命名 `namespace::tool`：插件只写短名，kernel 拼全名，撞名由注册表拒绝
- 审计默认全覆盖、日志分级、敏感值（API key 等）脱敏
- 模型：主模型 deepseek-v4-flash；视觉模型 qwen3-VL（SiliconFlow）；settings 配 API_URL/API_KEY
- 抄开源代码必须保留原许可证声明并在文档注明来源

## 开发约定

- 提交信息用简洁中文描述（如 `feat(kernel): 注册表与 CallerPolicy 校验`）
- 改动必须通过 `cargo test` 与 `cargo clippy -- -D warnings`
- 改设计不留痕 = 没改：必须同步 CONTEXT.md 或新增 docs/adr/
- **代码改动必须同步文档**：任何代码修改在测试通过后，凡受影响的文档必须同步修改；发现现有文档没有覆盖该变化时，必须新增对应文档。文档同步完成前，任务不得视为完成。
- **职责先行的模块组织**：开发新功能时先按职责规划模块边界；预计存在两个及以上职责时，直接创建同名文件夹与 `mod.rs`，不要先堆进单文件、超过 ~400 行后再被动拆分。`mod.rs` 只负责公共面、装配与 `pub use` 重导出；职责实现放子模块；子模块间共享的私有项经父模块 `pub(crate) use` 桥接；`use super::*` 只继承父模块的 pub 面，子模块自己的 imports 要显式写。~400 行只是审查预警线，不是拆分触发条件。拆分必须保持外部引用稳定、零行为变化，并通过全量测试 + live_api 复验。

## Agent skills

### Issue tracker

Issues live in this repo's GitHub Issues, accessed via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default labels (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`) map the five canonical triage roles. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context layout: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
