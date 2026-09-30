# 0047 — 服务端：账号体系、兑换码售卖与 DeepSeek 中转

日期：2026-09-29
状态：已采纳
修订：`docs/TODO.md` 第 3 条「加入服务端」的架构待定项；**豁免** AGENTS.md「mistake-agent 本体单 crate，不新增 crate 拆分」红线的适用范围（见「影响」）

## 背景

此前 mistake-agent 是纯本地单二进制应用：无服务端、无账号，模型走用户在设置里自备的 DeepSeek Key（ADR-0045 单模型配置）。`docs/TODO.md` 早已列出「加入服务端：教师端班级管理 + 出题下发」，但技术栈、数据模型、鉴权方式、客户端接入路径四项均未决。

现在要落地的第一个服务端功能是**模型服务中转 + 售卖服务包**：平台统一持有 DeepSeek Key，学生零配置使用，额度与时长由平台管控。这解决三个现实问题：

- 中学生与家长自备 API Key 的门槛过高（注册、充值、填配置），不符合「双击安装即用」的产品前提；
- 家长需要**可控上限**（防超支），自备 Key 无法提供；
- 未来学校/机构统一采购需要一个可发码、可对账的账号载体。

账号体系的目标形态含三种角色（用户 / 老师 / 管理员），但首期 Teacher 无功能（班级管理、出题下发属二期），仅占位。

## 决策

### 1. 交付物与仓库位置

服务端作为**同仓库顶层 `server/` 目录**下的独立 Cargo 项目（自带 `[workspace]`，不进客户端二进制、不被根 crate 依赖）。理由：中转协议与客户端接入必须同步演进，同仓库可在一个 PR 内改完两侧；独立 Cargo 项目保证根目录 `cargo test` / `clippy` 不受影响。

### 2. 技术栈

| 层 | 选型 |
|---|---|
| 语言 | Rust 2024 edition |
| HTTP | axum |
| 数据访问 | sqlx（编译期 SQL 校验 + migrations；表数量少，事务与聚合可控性优先于 CRUD 代码量） |
| 数据库 | PostgreSQL |
| 口令 | Argon2id |
| 反代 | Caddy（自动 TLS） |
| 部署 | 单二进制 + systemd |

### 3. 账号与角色

- 角色 `role ∈ {user, teacher, admin}`，**首期 `teacher` 仅占位**：建字段与权限位，不提供任何教师端功能。
- 首个 `admin` 由 bootstrap 种子（环境变量）或 CLI 创建，不开放管理员自助注册。
- 用户注册采用自助注册；口令 Argon2id；账号状态 `active` / `disabled`。

### 4. 鉴权：不透明令牌，不用 JWT

令牌为 `mka_` 前缀 + 32 字节随机串，数据库只存 SHA-256，可设过期、可撤销、记 `last_used_at`。

理由：计费场景下**每次请求都必须查额度**（DB 查询无法避免），JWT 的无状态优势归零；而撤销、踢下线、设备数限制、泄漏排查都需要服务端状态。

### 5. 中转：DeepSeek Responses API 的 drop-in 兼容子集

服务端实现 `POST /responses`，客户端**模型适配器零改动**即可切换（依据见下）：

```
POST /responses
  1. 校验 Bearer 令牌 → 定位用户 / 令牌 / 生效权益
  2. 三窗口限额校验，不足则直接 402 + {"error":{"message":"..."}}（不发起上游请求）
  3. 请求体仅做三件事：覆盖 model（不信任客户端）、限制 body 上限、生成本次 request_id
  4. 转发至 api.deepseek.com/responses，携带平台密钥
  5. SSE 边转发边旁路解析，只提取 response.completed 的 usage —— 不缓冲、不加工
  6. 写 usage_events + 递增权益已用次数
```

关键性质：服务端**不解析工具调用、不拼上下文、不做任何 Agent 逻辑**，是纯流式管道 + 会计。因此无状态、可水平扩展，客户端 Agent 行为完全不受服务端影响。

**请求与响应正文不落库、不落临时文件**（内存透传用完即弃）；日志与审计只记元数据（用户、令牌、模型、token 计数、耗时、request_id）。上游错误码原样透传。

自有业务 API 走 `/api/v1/*`，与中转路径分离；不把业务语义塞进 `/responses`。

### 6. 商品、计量与限额

- **商品**：体验包 `1 元 / 10 次`（一次性）；月卡 `Lite 28 元` / `Pro 68 元` / `Max 128 元`。
- **对外按次数，内账按 token**：`plans` 定义对外次数与各窗口阈值；`usage_events` 如实记录 `input_tokens` / `cached_tokens` / `output_tokens` / `reasoning_tokens`，用于成本核算、毛利监控与限额校准（价格表配置化，官方调价不改代码）。
- **阶梯扣次**（防止单次超长上下文击穿按次售卖）：`input+output ≤ 32k` 扣 1 次、`≤ 64k` 扣 2 次、`> 64k` 扣 3 次，阈值与档位可配。
- **窗口**：5 小时 / 7 天 / 30 天三个**滑动窗口**（统一实现：一张流水表 + 三个时间偏移聚合；无自然周期边界效应），每次请求前聚合校验。
- **扣减事务**：请求前校验 → 转发 → 收到 `response.completed` 时写流水并递增。语义：上游错误（`upstream_error`）不扣次；用户中断（`aborted`）按已产生 usage 扣次。
- 限额数值**不拍脑袋**：M3 上线后统计真实每回合 token 的 P50/P90/P99，结合单价与目标毛利反推三档阈值；首月从严宽松，`plans` 热改不发布。

### 7. 售卖走兑换码，不接在线支付

`redemption_codes` 批量生成（Crockford Base32 去易混淆字符 + 校验位，便于手抄与电话报码），线下收款后发码；用户端输入码，**事务内**校验状态 → 绑定用户 → 生成 `entitlements` → 码置 `redeemed`（并发重复兑换由唯一约束与行锁兜住）。

- 月卡**叠加顺延**：新权益起始接在现有权益到期之后，而不是从当下算，避免续费亏掉剩余天数。
- `entitlements.source` 预留 `payment`，二期接在线支付时支付成功直接发放权益，兑换码表结构不变。
- 批量生成支持导出 CSV（线下售卖对账）。

### 8. 管理通道

首期 **REST + CLI**，不做网页管理台（避免托管第二个前端）：

```
mistake-agent-server admin create-admin --email ...
mistake-agent-server admin generate-codes --plan lite --count 50 --out codes.csv
mistake-agent-server admin grant --user <email> --plan pro
mistake-agent-server admin revoke-code --code XXXX-XXXX-XXXX
```

### 9. 防共享（轻量）

月卡必然遇到同班共用。首期只用三招，不做自动封禁（误伤成本高于收益）：令牌数上限（体验包 1 台 / 月卡 3 台）、同用户并发上限（默认 2，超出 429）、记录 IP 与令牌 label 供事后排查。

### 10. 数据与隐私边界

| 通道 | 是否落库 |
|---|---|
| 中转请求/响应正文 | **不落**（内存透传） |
| 账号、令牌、权益、用量流水 | 落库（元数据） |
| 设备同步数据（会话/错题/记忆） | **仅当用户显式开启 `sync_enabled`**（ADR-0049） |

`sync_enabled` 默认关闭；关闭时不接收任何内容数据；已上传数据支持一键删除与全量导出。服务端所有业务查询强制带 `user_id` 隔离，不提供跨用户的业务查询接口。

### 11. 中转与同步是两条独立通道

不得混用：中转链路不落正文，同步链路只接收客户端**主动上传**的数据。「同步」是用户显式选择的功能，不是服务端对中转流量的顺手记录——这条区别是隐私表述的基石。

## 依据

- **drop-in 可行性的代码依据**：客户端 `responses_endpoint()`（strip 尾 `/` 与 `/v1` → 拼 `/responses`）、`bearer_auth(&api_key)`、以及 `map_status_error` 对 401/402/429/400 的既有映射（`src/kernel/plugin/model/mod.rs`、`src/kernel/plugin/model/responses.rs`）。因此服务端只要实现兼容子集，客户端连"额度用尽"的错误提示都能复用。
- **兑换码替代在线支付**：在线支付需要营业执照与商户号，周期与不确定性最大；兑换码零合规成本，且天然支持机构批量采购。功能上不损失——`source=payment` 已为二期预留。
- **不透明令牌替代 JWT**：计费场景每次请求必须查额度，无状态收益为零，而可撤销性必需。
- **滑动窗口替代自然周期**：统一实现（同一张流水表 + 三个时间偏移），且避免"月末用光 + 月初满额"的双倍消耗与月卡到期日错位。

## 影响

- **红线豁免范围**：AGENTS.md 的「本体单 crate」约束继续适用于客户端 `src/`；`server/` 是**独立部署单元**，不参与客户端 crate 图、不进客户端二进制。本 ADR 即为该豁免的留痕。后续服务端新增内部模块不受单 crate 约束。
- **协议归属**：`/responses` 是 DeepSeek 兼容面（客户端模型链路），`/api/v1/*` 是自有面（账号/套餐/同步），二者不得互相侵入。
- **未验证项**：DeepSeek 上游在中转下的并发/超时/断流行为；含 base64 图片的大 body（数 MB）上限与内存占用；限流参数取值。均需 M3 真实链路验证。
- **限额数值待校准**：`plans` 的窗口阈值是初始占位，M3 后按真实数据收紧（见决策 6）。
- **本机环境**：开发机当前无 `psql`，M1 需要以容器方式起 PostgreSQL。
- **文档同步**：`PROJECT.md`（新增服务端章节、工程结构、里程碑、ADR 计数）、`CONTEXT.md`（账号 / 平台服务 / 服务包 / 兑换码 / 权益 / 中转 / 限额窗口 / 扣次 术语）、`docs/TODO.md`（第 3 条改为已立 ADR + 里程碑清单）；服务端落地后另需 `docs/server.md`（部署运维手册，M8 交付）与 `docs/usage.md` 中「纯本地、无账号体系」表述的修订。

## 修订（2026-09-29，S3 开工前的研究结论）

S3 动工前做了一轮一手来源研究（DeepSeek 官方文档 + 本机实测抓包 + new-api/sub2api 源码机制），成果见 [docs/research/deepseek-api-compat.md](../../docs/research/deepseek-api-compat.md) 与 [docs/research/llm-gateway-mechanisms.md](../../docs/research/llm-gateway-mechanisms.md)。据此修订以下决策：

### R1. 协议面从「只做 Responses」扩为「三面全做，且全部透传」（**推翻**原先「Anthropic 需双向翻译、后置」的判断）

DeepSeek 官方提供 **Anthropic 兼容端点**（`base_url = https://api.deepseek.com/anthropic`，`x-api-key` 完全支持，`anthropic-version` 被忽略，`tools`/`tool_use`/`tool_result`/图片块完整支持）。因此三个协议面对应的上游端点**都存在同名形态**：

| 下游路径 | 上游路径 | 终态标记 | usage 位置 |
|---|---|---|---|
| `/responses`（含 `/v1/responses`） | `/responses` | `response.completed`/`incomplete`/`failed` 集合 | `response.usage` |
| `/chat/completions`（含 `/v1/chat/completions`） | `/chat/completions` | `data: [DONE]` | `data.usage` |
| `/v1/messages` | `/anthropic/v1/messages` | `message_delta` → `message_stop` | `message_start.message.usage`（输入）+ `message_delta.usage`（终值） |

**结论**：协议适配器是薄层（路径映射 + usage 提取），**无需双向翻译**。Anthropic 纳入首期。
代价与风险从「一周以上的翻译层」降为「一个小提取器」，但要处理三个真实差异：三面 usage 字段名不同、`input_tokens` 语义不同（Responses 已含缓存命中；Anthropic 不含，总输入 = `input_tokens + cache_read_input_tokens + cache_creation_input_tokens`）、终态标记不同。

同时采纳：中转路由**同时接受** `Authorization: Bearer` 与 `x-api-key`；路径**同时认** `/v1/...` 与无前缀形态（OpenAI SDK 与第三方客户端硬编码 `/v1`，而本项目客户端会把 `/v1` 剥掉）。

### R2. 计费从「事后记账」改为「预扣 → 结算」

借 new-api 的做法（`PreConsumeBilling` → 转发 → `PostConsumeQuota` 差额结算，且预扣额度可按分段结算上调）：请求开始时先写一条 `status='reserved'`、`billed_uses=1` 的用量记录（使其立即计入三窗口聚合），拿到上游 usage 后**结算**该行（更新为 `status='ok'` 与最终 `billed_uses`，阶梯可能把 1 上调为 2/3）；上游明确失败则记为 `upstream_error` 且 `billed_uses=0`（等价退款）。这样并发下不会有两个请求同时挤过窗口边界。

计费安全不变量同步采纳（来自 new-api 的计费约定，逐条写进我们的测试）：乘数量必须有上界并在入口 400 拒绝；扣次与窗口换算集中在 `billing` 模块且用饱和运算；异常扣次（clamp）落审计；新增计费路径必须走「校验 → 扣次 → 预留 → 结算」全链路核对。

### R3. 中断与终态判定按面分流，且终态是集合

Responses 面的终态**不止** `response.completed`：工程实践还要认 `response.done`/`cancelled`/`canceled` 等变体（new-api 的累加器即按集合判定），解析器对未知事件名必须容忍（Anthropic 面实测会下发 `ping`）。中断计费规则：**流已开始且上游未明确报失败 → 至少扣 1 次**；明确失败 → 不扣。理由与 new-api 注释一致：上游一旦开始生成就已经为 prompt 计费。

### R4. 计费口径归一化为四元组

内部统一 `{input_total, cached, output, reasoning}`，由各协议适配器填充分解字段。对外仍只暴露「次数」。`usage_events` 的字段设计保持不变（已能容纳两个协议的形状）。

### R5. 把平台用户 id 透传到上游的隔离字段

三面字段名不同（Responses 顶层 `user`、Chat `user_id`、Anthropic `metadata.user_id`），填进去即可获得上游的 **KVCache 隔离、内容安全隔离与调度隔离**——免费的多租户隔离能力，默认开启。

### R6. 里程碑边界调整

`plans` / `entitlements` / `usage_events` 三张表**从 S4 提前到 S3**：中转没有权益就无从限流（否则只能一律 402）。S4 保留兑换码、admin CLI、CSV 导出与套餐数值校准，并承接令牌模型白名单与软删（借 new-api 的令牌设计）。

### R7. 未登录的产品规则（协作开发者确认，写入实现与文档）

**未登录仍可完整使用 App**；但要接入模型须**自行配置 API Key**，且**不参与消息同步**。即登录态只决定「平台模型服务」与「设备同步」两项能力，不构成 App 使用前提。这与 ADR-0048 决策 2 的「登录纯可选」一致，此处明确为可对外表述的产品规则。

### R8. 对既有文档的事实纠正（已同步至 PROJECT.md）

1. **`top_p` 的生效条件原先写反了**：它在 thinking 模式下**生效**（有效区间 0.95–1.0），非 thinking 下恒为 1.0 且传入值被忽略；`temperature` 才是 thinking 下不生效。
2. **`deepseek-v4-pro` 不支持图片输入**（`input_modalities` 只有 text），配成 Pro 后带图消息会失败。
3. **旧模型名 `deepseek-v4-flash` 并非报错退役**，仍可调用并被路由到 V4.1-Flash。

### R9. 未采纳项（明确不抄）

new-api / sub2api 均为多渠道平台（前者 AGPL-3.0、后者 LGPL-3.0，**只借鉴机制不复制代码**）。不采纳：渠道池与负载均衡、失败换渠道重试、分组倍率、WebSocket 传输、任务型计费、ClickHouse 日志库、多数据库兼容矩阵、Casbin 授权。Redis 亦不引入——单实例单上游用进程内状态即可；**若将来水平扩展，并发闸门与窗口预留必须挪到共享存储**（此项预先记录，避免届时遗漏）。

### R10. 兑换码的最终形态：注册制 + 服务端对称加密存储

讨论中评估过"自包含密码学码"（用户只持密文、公私钥均在服务端）。**结论：不采用，改走注册制**——码的有效性由库里的登记状态决定，因为**登记这一步无论如何都要碰数据库**（防重放、作废、批次计数都需要服务端状态），自包含码省不掉它，却要额外付出：码长（RSA-2048 密文 256 字节 → Base32 约 410 字符）、多一对密钥的保管与轮换、以及**无法按码作废**（退款/发错人只能整批处理）。

配套确认的产品流程（协作开发者确认）：

1. 代理商/学校**离线生成**随机码（我们不给他们数据库权限）
2. **登记入库**——码在登记之前不可用
3. 用户兑换：按码查库 → 有效则发权益、置为已用
4. 已用的码不再是有效记录 → 重放被挡

**存储形态：服务端对称加密（AES-256-GCM），不是 RSA，也不是哈希。**

- 用对称而非非对称，理由与 R1 一致：**加解密双方都是服务端**，公钥没有第二个使用者；RSA 只会把"一把密钥"膨胀成"一对密钥 + 256 字节密文"。
- 加密而非哈希：库单独泄露时（密钥在环境变量/0600 密钥文件里，**不落库**）未兑换的码仍安全，同时保留**客服重发同一张码**的能力。哈希方案同样抗泄露，但丢掉重发；若日后确定不需要重发，可退化为哈希。
- 密文携带 **key id**，支持密钥轮换而不作废存量码；另存 `code_prefix`（前 8 字符）供客服肉眼核对。

表结构（S4 落地）：

```sql
redemption_codes(id, batch_id, plan_id,
                 code_ciphertext bytea,   -- AES-256-GCM(nonce||ct||tag)
                 code_key_id text, code_prefix text,
                 status text,             -- unused / redeemed / void
                 imported_by, imported_at, expires_at,
                 redeemed_by, redeemed_at, entitlement_id)
redeem_batches(id, agent, plan_id, allocated_count, redeemed_count,
               status, note, created_at)
```

三条配套纪律：

1. **生成器由我们提供**（离线、无网、无密钥的小工具或脚本），并在导入时严格校验：长度、Crockford Base32 字符集、内嵌校验位、批内去重、以及拒绝明显有序的批次（相邻码仅差末位）。代理商自行生成的弱随机码（时间戳/自增序号/未播种 rand）会让整批可枚举。
2. **批次计数封顶**：一批登记 N 张，第 N+1 次兑换直接拒绝——这是"代理商手里握着全部明文码、理论上可自兑"这一信任边界下平台唯一能做的技术约束（另两条是可整批作废与全链路可追溯；平台**无法阻止**代理商自兑，只能事后追责）。
3. **操作顺序固定为「先生成 → 再登记 → 后分发」**：先分发后登记会让用户当场兑换失败，变成客服问题。

### R11. 部署形态：容器化与 systemd 并存

S8 原定为「单二进制 + systemd」。补上**容器化路径**（`server/Dockerfile`、`docker-compose.prod.yml`、
`deploy/Caddyfile`），两者并存而非替换——镜像里跑的仍然是同一个二进制，所以本 ADR 决策 2
的「单二进制」性质没有改变，改变的是**编排方式**：

- **容器栈**：PostgreSQL + 服务端 + Caddy（自动 TLS）三个容器，一条命令拉起，便于在 VPS 上复现；
- **systemd**：仍然保留为"只用二进制"的最简路径（无 Docker 的机器也能部署）。

三条容器化专属约束（S8 运维手册要写进去）：

1. **服务端容器不发布端口**，只允许同一 compose 网络的 Caddy 访问。8080 是裸 HTTP，
   直接对外等于把"没有 TLS 的 API"暴露在公网。
2. **Caddy 侧必须 `flush_interval -1` 且不开压缩**：中转面是 SSE 长连接，缓冲或压缩都会把
   "边生成边到达"退化成"最后一次性吐出"——这是体验层面的功能性退化，不是性能取舍。
3. **反代之后必须 `SECURITY_TRUST_PROXY=true`**：否则服务端看到的来源 IP 全是网关地址，
   按 IP 的令牌桶限流会把所有学生当成同一个人，fail2ban 也拿不到真实 IP。
   这条与 §R9 的"不信任转发头"默认是同一枚硬币的两面——**默认不信任，确认在代理后面才开**。

运行阶段镜像需要 `ca-certificates`（访问上游 HTTPS）与 `libssl3`（native-tls 在 Linux 上是 OpenSSL）。

**另外记录一条与部署无关但由容器化暴露的环境事实**：本机（Windows + MSVC）上，
`ring` / `aws-lc-sys` 这类**带 C 代码**的 crate 在**含非 ASCII 字符的构建路径**下编译失败
（`...\项目代码\Web与应用\...`；已用 ASCII 路径对照验证）。服务端因此改用 native-tls；
**客户端（根 crate）仍依赖它们，目前只靠旧的构建缓存才能编译**，`cargo clean` 后会暴露——已在
`docs/TODO.md` 单列待修项。

### R13. 平台额度查询端点（客户端用量卡片的数据源）

**背景**：客户端要在登录态把「DeepSeek 余额」卡片换成**平台额度**——5 小时 / 1 周 / 1 月三个滑动窗口的
使用百分比（见 ADR-0048 修订 R10）。但服务端目前只有 `GET /api/v1/me`（用户资料），**没有任何窗口用量端点**，
卡片无从取数。故新增：

```
GET /api/v1/me/quota      # bearer 鉴权，与 /me 同级
```

返回（形状即契约，客户端按此渲染）：

- 套餐：`{ code, name, expires_at }`
- 三个窗口：`[ { key: "five_hour" | "week" | "month", limit, used, remaining, resets_at } ]`
- 权益快照：`{ total_uses, used_uses, expires_at }`

四条必须守住的语义：

1. **`used` 必须与限流判定同源**——复用 `billing/quota.rs` 的窗口计算（`SUM(billed_uses)`，含 reserved 行），
   否则会出现「卡片显示还有额度、网关却回 402」的自相矛盾。**不允许在 handler 里另写一份计数逻辑**。
2. **`resets_at` = 最早一条仍被计入的用量 + 窗口长度**，即「已用次数首次下降」的时刻。滑动窗口没有固定重置点，
   前端不要按自然日/整点渲染。
3. **无生效权益 → `200` + 空窗口 + 标记字段**（客户端据此显示兑换引导），**不要用 402**：
   402 的语义是「这次请求被拒」，而「查自己有没有额度」不是被拒。
4. **不含任何密钥材料**，只有计数与套餐元信息。
