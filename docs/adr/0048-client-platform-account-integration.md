# 0048 — 客户端接入平台服务（账号、兑换码、额度展示）

日期：2026-09-29
状态：已采纳
依赖：ADR-0047（服务端架构）、ADR-0049（多设备同步）
修订：ADR-0027（settings 补丁与热更新新增 `account` 段）、ADR-0045（单模型配置增加「平台服务 / 自备 Key」两种取值来源）、ADR-0031（余额卡片在平台模式下语义变化）

## 背景

ADR-0047 落地了服务端（账号、兑换码、DeepSeek 中转）。客户端需要一条接入路径，且**不能破坏既有的纯本地使用方式**——「本地优先、无服务端也能完整使用」是本项目的产品身份（PROJECT.md §2），不是可选特性。

现状相关事实：

- 模型配置为单份 `main_model { api_url, api_key, model, transport }`（ADR-0045），用户独占写、kernel 独占读（ADR-0015/0027）。
- 模型链路已支持任意 `api_url` + Bearer Key，因此**指向平台服务不需要改协议**（ADR-0047 决策 5）。
- GUI 左下角用户菜单的三项（下载手机端 / 帮助与反馈 / 退出登录）目前是纯占位，点击只如实说明"尚未支持"（`web/src/App.vue`）。
- 设置页已有余额卡片（`check_balance`，ADR-0031，仅 DeepSeek）。

## 决策

### 1. 配置：新增 `account` 段，不塞进 `main_model.api_key`

```json
{
  "account": {
    "server_url": "https://api.example.com",
    "token": "mka_...",
    "email": "a@b.com",
    "role": "user",
    "sync_enabled": false
  }
}
```

**语义分离的理由**：`api_key` 与用户令牌是两种凭据（一个花自己的钱，一个花平台的额度），混用会让"切换自备 Key / 平台服务"、登出清理、泄漏排查全部变得含糊。令牌明文落盘与既有 `api_key` 同等取舍（DPAPI / Windows 凭据管理器列后续），但风险等级低一档——令牌只能消耗额度，不是平台主密钥。

### 2. 模型链路选择：令牌非空走平台，否则走自备配置

```
account.token 非空 → api_url = account.server_url，Bearer = account.token
account.token 为空 → 沿用 main_model（自备 Key / 本地 Ollama）
```

实现上收在模型服务的构造路径里（与 ADR-0027 的热替换同一处）：登录、登出、兑换码生效后立即热替换，下一轮模型调用即生效，不需要重启。

**登录纯可选**：不登录时客户端行为与今天完全一致（146 项单测 + live_api 链路行为不变），"本地优先"身份不被削弱。

### 3. 新增 RPC 与事件

| 方向 | 名称 | 说明 |
|---|---|---|
| RPC | `register` | 邮箱 + 口令注册 |
| RPC | `login` | 登录并落盘令牌 |
| RPC | `logout` | 清空 `account.token`，回到自备 Key 模式 |
| RPC | `redeem_code` | 提交兑换码，返回生效后的套餐状态 |
| RPC | `get_account_status` | 已登录信息 + 套餐名 / 三窗口剩余 / 到期日 / `sync_enabled` |
| 事件 | `account_changed` | 登录、登出、兑换成功、额度用尽 |

客户端侧承载这些调用的是新增内核级模块 `src/kernel/account/`（与 session scheduler 同级）：持有到服务端的 REST 调用、错误归一化与状态缓存；`settings.json` 仍是唯一持久事实。

### 4. OOBE：追加**可选**登录步骤

现有三步引导之后增加一步「登录平台服务」，明确标注**可跳过**（跳过即纯本地模式，与今天一致）。登录成功后**在同一屏**询问是否开启「同步设备数据」，默认关闭，选择结果写入 `account.sync_enabled`（ADR-0049）。

顺序上必须"先登录、后询问同步"：同步依赖账号，未登录时该选项不出现。

### 5. 设置页「账户与套餐」卡片

未登录：显示登录入口 + 一句"不登录也可完整使用，需自备 API Key"。

已登录：套餐名、生效期、**5 小时 / 周 / 月三个窗口的剩余用量**、到期日、兑换码输入框、退出登录、同步开关与状态、「删除云端数据」入口。

**余额卡片的语义调整**：平台模式下 `check_balance`（DeepSeek `/user/balance`）不再有意义——学生花的不是自己的余额，而是平台额度。平台模式下余额卡片改为展示套餐与窗口余量；自备 Key 模式下保持现状。两种模式下都不显示平台密钥相关信息。

### 6. 左下角用户菜单接真实数据

`web/src/App.vue` 的三项占位改为真实行为：已登录时「退出登录」执行 `logout`；未登录时显示「登录」。菜单内展示当前身份（邮箱 / 角色）。同步状态以轻量指示呈现在同一区域（同步中 / 已同步 / 失败），不打断操作。

### 7. 错误引导（复用既有映射）

客户端已有 `map_status_error` 把 401 → `AuthFailed`、402 → `QuotaExceeded`、429 → `RateLimited`：

| 上游 | GUI 行为 |
|---|---|
| 401 | 提示登录失效，引导重新登录（不清空本地数据） |
| 402 | 提示额度用尽 / 套餐到期，引导输入兑换码或续费 |
| 429 | 提示"操作过于频繁"，保留重试入口 |

不自建第二套错误语义，避免与服务端返回的分叉。

### 8. 服务端地址

内置生产地址作为默认值；设置页可改（便于自建服务端与联调）。地址校验沿用既有 `http(s)://` 前缀规则。

## 依据

- 客户端模型链路已经是"任意 `api_url` + Bearer"，因此平台接入是**配置问题而非协议问题**，这是本方案低风险的根本原因（ADR-0047 决策 5）。
- 令牌与 `api_key` 分离：避免"登出后残留平台凭据"与"自备 Key 被平台令牌覆盖"两类混淆。
- 登录可选：维持 PROJECT.md §2「本地优先、家长在意隐私」的产品前提；强制登录会把现有用户变成流失用户。

## 影响

- **既有行为不变**：未登录路径零改动，146 项单测与 `cargo test --test live_api -- --ignored` 应全绿；新增单测覆盖"令牌非空走平台 / 为空走自备"的分支。
- **数据兼容**：旧 `settings.json` 无 `account` 段，解析时缺省（`token` 为空即本地模式），启动不报错。
- **OOBE 流程变化**：三步 → 四步（末步可跳过）；`docs/usage.md` 需同步。
- **文档同步**：`PROJECT.md`、`CONTEXT.md`（Account / Platform service 术语）、`docs/usage.md`（登录、兑换码、同步开关、左下角菜单）、`docs/api.md`（新增 RPC 与 `account_changed` 事件）、`docs/testing.md`（平台模式验收项）。
- **未验证项**：登录态下的会话内工具链路（含图片 `input_image`）经中转的端到端复验；令牌过期时的批量失败体验（需避免每个回合都弹错）。

## 修订（2026-09-30，S5 登录半程落地）

S5 原定「登录 + OOBE 可选登录 + 兑换码 + 账户与套餐卡 + 401/402 引导」一次性交付，但 **S4（兑换码 / 套餐）在 `server/` 里一个接口都没开工**，`docs/server-api.md` §1 也明确写着「客户端不要提前接」。故拆成两段：本轮只交付**登录半程**（注册 / 登录 / 登出 / 账号状态 / 模型链路切平台 / 左下角菜单真实化），兑换码与套餐相关的全部内容顺延为 **S5-B**（依赖 S4）。以下是落地时对上面决策的偏离与新事实。

### R1. 登录入口是**首屏门禁（可跳过）**，不是 OOBE 第 4 步

决策 4 原定在 OOBE 末尾追加可跳过的登录步。落地改为独立整页 `web/src/components/LoginGate.vue`：启动且未登录时盖在最上层，页脚留「先跳过，用本地模式」。理由是顺序——OOBE 是「没配 Key」时的向导，而登录后根本不需要自备 Key；已登录时 OOBE 直接不出现（`onMounted` 的触发条件收紧为 `!key_set && !logged_in`）。

跳过的选择记在 `localStorage: ma:gate-skipped`，**不是单向门**：侧栏菜单里的「登录平台服务」随时能把门禁叫回来。改写的是决策 4 的**位置与形态**，"登录纯可选"这一内核不变。

### R2. 「账户与套餐」卡片未做，余额卡片只加一句语义提示

决策 5 的套餐名 / 三窗口余量 / 兑换码输入框全部依赖 S4 的 `GET /me` 扩展字段，本轮只落地决策 5 里"余额卡片语义调整"的一半：平台模式下在余额卡片顶部加一行提示「已登录平台服务：模型调用走平台额度，下面这个 DeepSeek 余额不再被使用」（`SettingsPage.vue` 的 `.balance-note`）。**没有**按平台模式隐藏余额卡片——自备 Key 与平台模式共用同一份 `main_model`，隐藏反而会在切换瞬间闪断。

### R3. 错误引导只做「令牌失效即清 + 一句提示」，没有回合内气泡

决策 7 的 401 引导落地为：内核 `get_account_status { revalidate: true }` 打一次 `GET /me`，**只有服务端明确回 `invalid_token` / `missing_token` / `account_disabled` 才清本地令牌**；`Unreachable` / `BadResponse` 一律只回 `reason`，本地一个字节不动。这个判定被收在唯一一处 `AccountError::invalidates_token()`，前端 `revalidateAccount()` 的行为是它的镜像。GUI 启动时静默校一次，失效时写侧栏状态行。

**已知缺口（留给 S5-B）**：令牌在会话中途过期时，模型层仍是 `AuthFailed` → `StopReason::InternalAbort { ModelUnavailable }` → 只发 `TurnEnd`，**回合内没有任何文案**。决策 7 设想的「60s 节流 + 系统气泡 + 去登录按钮」需要 `ChatPage.vue` 的回合结束分支配合，本轮未动，学生此时看到的现象仍是"回答突然没了"。

### R4. `redeem_code` RPC 未做（依赖 S4）

决策 3 的方法表里 `redeem_code` 与 `get_account_status` 的套餐字段均无服务端支撑，一律不做。实际落地的方法只有 4 个：`register` / `login` / `logout` / `get_account_status`。

### R5. `get_account_status` 只返回账号视图 + 可选 `reason`，不返回套餐字段

返回 `{logged_in, server_url, email, role, sync_enabled, reason?}`，其中 `reason ∈ {token_invalid, account_disabled, unreachable, server_error}`。这是**追加式**契约：S4 补齐套餐字段后前端无需改动即可读取。

### R6. `set_account_sync` RPC 未做；`AccountPatch` 只有 `server_url`

决策 1 原定 `AccountPatch { server_url, sync_enabled }`，落地砍到只剩 `server_url`——`sync_enabled` 没有写入方（同步引擎属 S6），留一个无人调用的字段只是待腐化的接口。`token` / `email` / `role` / `sync_enabled` 四个字段**在类型层面**就无法经 `set_settings` 注入，只由登录 / 登出 / 状态刷新三条路径写（`settings.rs` 的 `AccountPatch` 定义处有注释说明）。

### R7. 服务端地址的编辑入口在**门禁上**，不在设置页

决策 8 说"设置页可改"。落地改为：地址输入框在 `AccountAuthForm` 里（登录/注册之前），已登录时**没有**地址输入框——改地址必须先退出登录。理由是避免"令牌属于 A 服务器、地址却指向 B"的半截状态，这比"改地址方便"重要。默认值 `DEFAULT_SERVER_URL = http://8.131.146.250:8080`，写入时经 `normalize_server_url()` 归一化（强制 http(s)、必须有主机名、去尾斜杠）。

### R8. 计数与门禁现状（2026-09-30 实测）

`cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` / `cargo test --lib`（**179 项**，其中账号相关 14 项）/ `cd web && npm run build` 全绿。原文与 §影响里的「146 项单测」是本文档写作时的旧数，现以 179 为准。服务端代码本轮**未改动**。

### R9. 端到端**尚未验证**（已由 R11 推翻，2026-09-30 跑通）

本机无 PostgreSQL、无 docker，`8.131.146.250:8080` 不通，`server/` 又是 postgres-only ——「注册 → 登录 → 模型走平台 → 退出登录」这条链**一次都没跑过**。验证只到离线单测 + 静态门禁 + 前端构建。真机走查步骤、以及「动手前先备份 `settings.json`」的提醒见 `docs/testing.md` §5。

单测不覆盖登录/登出落盘链路是**刻意的**：那条路径会经 `Settings::save()` 写真实数据根目录（`~/Documents/.mistake-agent/settings.json`），跑一遍就把用户配置覆盖了（`src/kernel/account/tests.rs` 模块头有说明）。

### R10. 登录态一律忽略自备 Key；余额卡片改为三窗口用量（2026-09-30）

**变更**（协作开发者决定），其中第 2 条**取代 R2 的「余额卡片只加一句语义提示」**：

1. **登录后自备 API Key 自动忽略**，要做成**代码层硬保证**，不只是文案：

   - **UI**：设置页自备 Key 输入区在登录态下**置灰** + 一行说明「已登录平台服务，自备 Key 暂不生效
     （退出登录后恢复）」。**不删 `settings.json` 里的 key**——走查步骤 6「退出登录 → 模型链路切回自备 Key」
     要求它可逆，删了会毁掉这个语义。
   - **代码层**：`routing::effective_config` 是**唯一**允许取用出站凭据的入口（令牌非空 → `api_url` 与
     `api_key` 都被平台覆盖）。
   - **已发现一处真实旁路（必修）**：`src/kernel/agent/balance.rs` 直接读 `settings.main_model` 去查
     DeepSeek 余额——登录态下**仍会用自备 key 发请求**，卡片显示的还是用户自己的余额（与实际由平台结算相矛盾）。
     修法：登录态**在读 key 之前**直接返回「平台模式」占位（结构性保证，而不是加个 `if` 之后仍可能读到 key），
     并补单测钉住「登录态不构造任何带自备 key 的请求」。

2. **登录态的余额卡片改为展示平台额度**：三个滑动窗口的**使用百分比**（含已用 / 上限 / 重置时间）。

   - **数据源尚不存在**：服务端没有窗口用量端点 → 需先落地 `GET /api/v1/me/quota`
     （见 [ADR-0047](0047-server-account-package-relay.md) 修订 R13）。**顺序：服务端端点先行，客户端卡片随后。**
   - 未登录时卡片行为**完全不变**（沿用 DeepSeek 余额）——这是回归红线。
   - **刷新时机**：登录后、以及**每回合结束后**主动查一次。中转的 usage 在服务端就被消费掉了
     （流式 tee 旁路解析），客户端拿不到流内用量，故卡片不能靠流式事件实时更新。
   - 无生效权益时显示**兑换引导**，不是错误态。

### R11. 端到端已跑通（2026-09-30，推翻 R9）

`tests/live_platform.rs` 三条 `#[ignore]` 用例把"注册 → 登录 → 模型走平台 → 退出登录"这条链跑通了
（本地 3/3；远端 `8.131.146.250` 的登录链路 2/2，注册因公网丢包未复跑，开发者确认暂不需要）。

做法上有两个值得保留的技巧：

1. **隔离数据根**：`Settings::data_root()` 优先读 `USERPROFILE`，把它指向临时目录后，
   登录/登出触发的 `Settings::save()` 只写在那里——真实 `settings.json` 一个字节不动，
   比 R9 建议的"先备份再跑"更干净，也让这条链**可以进 CI**。
2. **假 key + 死地址**：`main_model` 填必然失败的假 key、地址指向 `127.0.0.1:1`。
   于是"模型答出来了"本身就是"走的是平台令牌"的证据；反过来，无权益账号必须拿到
   402 `no_entitlement` 而**不是**静默退回自备 key（已断言）。

**顺带发现（未修）**：账号客户端对超时没有重试，公网抖动会直接暴露成 `Unreachable("请求超时")`；
建议对幂等 GET 加退避重试，并把注册 409 当作"已存在，转登录"处理。
