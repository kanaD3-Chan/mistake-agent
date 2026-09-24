# 0044 — 会话新建由用户发起（模型自动切换退役）

日期：2026-09-21
状态：已采纳
取代：ADR-0030 / ADR-0032 / ADR-0034（及 ADR-0006「不新建 SessionKey、不归档」的结论、ADR-0025 守卫模型的最后一处残留）

**修订（2026-09-23）**：本次决策的**收尾**已落地——原文「不在本次范围」的两项（前端会话列表 UI、存量数据迁移）与「待定」项（会话内版本切换）均已闭环。新增 RPC `open_session` / `rename_session` / `delete_session`；`SessionMeta` 新增 `title`（用户可见标题，首回合末由模型异步生成，`LlmTitler`）；新增事件 `Event::SessionTitleUpdated` 与审计 `SessionOpened` / `SessionRenamed` / `SessionDeleted` / `SessionTitleGenerated`；存量数据在 `FileStorage::open` 时按「上一会话梗概：」边界拆分为独立会话（幂等 + `.bak`，见 [file/migrate.rs](../../src/kernel/plugin/storage/file/migrate.rs)）；前端删除「会话」页，改为应用侧栏内常驻的会话列表。第 4 条决策（空闲超时仅提示）与本文其余结论不变。

## 背景

ADR-0006 当初明确否决了用户显式会话管理，理由是不给用户增加负担：会话切换交给模型判断，且**不新建 SessionKey、不归档**——"切换"实为同一 `SessionKey` 内的树内分叉，挂一个「上一会话梗概」摘要节点，旧分支保留为兄弟版本（ADR-0030）。

这套设计落地后演变成三处自动决策：

1. `SessionScheduler::on_new_message_with_display` 每条新消息前先问主模型要不要换话题（ADR-0032）；
2. `SessionScheduler::on_turn_end` 回合末再问一次（ADR-0030）；
3. `session::switch` 工具让模型在回合内主动切换，且对用户不可见（ADR-0034）。

问题在于**用户全程无感知、也无入口**：会话边界不存在于 UI，模型可能自作主张把话题切走，用户想主动"开个新对话"却做不到。分叉机制本身也把会话做成了树，读侧要靠 `scope_session_context` 把摘要节点之前的祖先裁掉，才不至于把旧目标当成当前目标。

## 决策

**一个话题 = 一条独立会话，新建会话只由用户发起。**

1. 三处自动决策一并删除：`on_new_message` 的预决策（ADR-0032）、回合末 `LlmTurnDecider`（ADR-0030）、`session::switch` 工具（ADR-0034）。`GuardModel` / `turn_decider_prompt` 随之退役——守卫模型至此已无任何调用方。
2. **彻底删除树内分叉机制**，含读侧：不再有"同一 SessionKey 内的分支切换"，`scope_session_context` / `is_session_summary` / `fork_branch` 全部移除。
3. 新建会话经 RPC `create_session` 触发，语义为**归档当前活动会话 + 新建独立 `SessionKey`**，交接摘要由参数 `carry_summary` 显式控制。
4. 系统级空闲超时（12h）**保留检测**，但改为发 `Event::SessionIdle` 提示用户，不再自动分叉。
5. 失败降级逻辑随决策一起删除：不再有"决策失败则 continue"这类兜底，因为不再有决策。

## 实现方式

- **新建会话**（`SessionScheduler::create_new_session`）：列出所有 `Active` 会话 → 需要时在归档前生成交接摘要 → 归档所有 `Active` 会话 → 新建 `SessionKey` → 摘要作为新会话首条 system 消息并 `set_active_path`。返回 `CreatedSession { key, archived, summary_attached }`。
  - 归档全部 `Active` 而非仅第一个：单 Active 是既有不变量（`active_session_key()`、缓存统计、调度器都用 `find(status == Active)`，而 `MemoryStorage::list_sessions` 遍历 HashMap 顺序不定），两个 Active 会让"当前会话"变成随机。
  - 摘要仅在 `carry_summary && 旧会话有非空内容` 时挂载；空会话没有可交接的内容。
- **RPC**：`Method::CreateSession { carry_summary: bool, goal: Option<String> }`，wire 形如 `{"method":"create_session","carry_summary":true}`。回合在飞时拒绝（`turn_in_progress`），否则该回合会继续往刚归档的会话里写。摘要**同步**生成：`LlmSummarizer` 对 <8 条消息直接走计数 stub 不调模型，失败降级 stub，最坏是一次超时带重试。
- **空闲超时**：检测仍在消息到达时（用户沉寂超过阈值后再次发言），发 `Event::SessionIdle { session, idle_seconds }` 后**继续当前会话**。
  - `EventSink` 而非 `InterruptBus`：后者（ADR-0023）的消费者只做日志/审计，到不了前端。
- `Interrupt` 收敛为 `ConfigChanged` / `MemoryChanged` / `CompactionDone`——`SessionSwitched` / `GoalUpdated` 的生产者已全部消失。

## 影响

- **存量数据（2026-09-23 已迁移）**：删除 `scope_session_context` 后，既有树结构会话（含摘要节点）会把整条路径原样送给模型——摘要节点与其祖先消息内容重复，token 上升，模型可能把旧目标当成当前目标。原文结论为「数据本身不受影响、无需迁移，用户新建会话即可绕开」；收尾时改为**主动迁移**：`FileStorage::open` 加载会话前按「上一会话梗概：」边界把多话题文件拆成独立会话（本地 `0ad77bb4….jsonl` 的 152 条消息拆成 22 条会话），原文件改名 `<key>.jsonl.bak` 完整保留可回退，`.bak` 非 `.jsonl` 故二次启动幂等；含老 `active_path` 的段继承老状态，不新增 Active。迁移非致命，失败只 `log::warn` 并留原文件待下次重试。
- 审计记录 `SessionSwitched` → `SessionCreated { session, archived, summary_attached }`；事件 `SessionSwitched` → `SessionIdle`。收尾新增审计 `SessionOpened` / `SessionRenamed` / `SessionDeleted` / `SessionTitleGenerated`，新增事件 `Event::SessionTitleUpdated`。
- 前端会话列表 UI 与存量数据迁移**已在收尾中补齐**（2026-09-23），见 [docs/TODO.md](../TODO.md) 第 1 项与文首修订注。会话内消息版本切换（`edit_message` + `switch_branch`）明确**保留**，限定为「会话内版本浏览」，不再承担会话边界语义。
- 文档同步：`docs/api.md`、`docs/prompts.md`、`docs/kernel-dev.md`、`docs/testing.md`、`docs/plugin-dev/kernel.md`、`PROJECT.md`、`CONTEXT.md`、`README.md`、`AGENTS.md`、`CHANGELOG.md`，以及被取代/受影响的 ADR 0006 / 0013 / 0017 / 0023 / 0025 / 0030 / 0032 / 0034 / 0035（均在文首加修订注或更新状态行）。
