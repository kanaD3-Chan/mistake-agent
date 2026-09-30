# 服务端接口文档（客户端接入用）

面向**修改客户端**的同学。服务端代码在 [`server/`](../server/README.md)，决策留痕见 [ADR-0047](adr/0047-server-account-package-relay.md)（服务端架构）与 [ADR-0048](adr/0048-client-platform-account-integration.md)（客户端接入）。

> **当前版本：S1–S3**（账号 + 三协议模型中转 + 配额计费 + 安全护栏）。
> 尚未实现的部分在文末 [「尚未实现」](#尚未实现s4s6-预告) 列出——**兑换码、套餐用量查询、设备同步都还没有**，客户端不要提前接。

---

## 1. 通用约定

| 项 | 约定 |
|---|---|
| Base URL | 例如 `https://api.example.com`（HTTPS；TLS 由反向代理终结） |
| 请求体 | JSON（`Content-Type: application/json`） |
| 响应体 | JSON，字段为 `snake_case` |
| 时间 | RFC 3339 / ISO 8601，**UTC**（如 `2026-09-29T13:17:18.795203Z`） |
| 错误体 | 统一 `{"error":{"code":"...","message":"..."}}`，`message` 已是面向用户的中文文案 |
| 限流响应 | 状态码 `429`，带 `Retry-After: <秒>` 头（客户端应据此退避，不要立即重试） |
| ID | UUID 字符串 |

### 鉴权

两种头都接受，值是**同一个平台令牌**（登录时签发）：

```
Authorization: Bearer mka_<64 位十六进制>      # OpenAI 系客户端 / 本项目客户端
x-api-key: mka_<64 位十六进制>                 # Anthropic 系客户端（Claude Code 等）
```

- 令牌形如 `mka_` + 32 字节小写十六进制，有效期默认 **90 天**（服务端 `TOKEN_TTL_DAYS` 可配）。
- 令牌明文**只在登录响应里出现一次**，服务端只存 SHA-256，**无法找回**——丢了就重新登录。
- 未登录不影响客户端的本地功能：可以继续用用户自备的 API Key（见 §6）。

---

## 2. 账号面

### 2.1 注册

```
POST /api/v1/auth/register
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `email` | string | ✅ | 邮箱；大小写不敏感（`A@B.com` 与 `a@b.com` 是同一账号） |
| `password` | string | ✅ | 8–128 字符 |
| `display_name` | string | | 昵称，≤24 字符；可省略 |

**201 Created**

```json
{
  "user": {
    "id": "fb9a30d3-ed20-402e-a7ea-457c1b540836",
    "email": "demo@example.test",
    "role": "user",
    "display_name": "小测",
    "disabled": false,
    "sync_enabled": false,
    "created_at": "2026-09-29T13:17:18.795203Z"
  }
}
```

自助注册**固定为 `user` 角色**；`teacher` / `admin` 只能由管理端授予。

### 2.2 登录

```
POST /api/v1/auth/login
```

请求：`{"email": "...", "password": "..."}`

**200 OK**

```json
{
  "token": "mka_bb06feef...(共 68 字符)",
  "expires_at": "2026-12-28T13:17:19.123456Z",
  "user": { "...同注册里的 user..." }
}
```

失败一律 **401 `invalid_credentials`**：
「口令错」与「账号不存在」返回**完全相同的响应**（含耗时），这是刻意的——登录接口不做邮箱枚举器。
客户端**不要**尝试区分这两种情况。

### 2.3 登出

```
POST /api/v1/auth/logout      Authorization: Bearer <token>
```

**204 No Content**。只撤销**当前这一个**令牌，同一账号在其它设备上的令牌不受影响。

### 2.4 账号状态

```
GET /api/v1/me                Authorization: Bearer <token>
```

**200 OK**：`{"user": { ... }}`（字段同上）。

### 2.5 局部更新账号

```
PATCH /api/v1/me              Authorization: Bearer <token>
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `display_name` | string | `""`（纯空白）= **清空**；不传该字段 = 不改 |
| `sync_enabled` | bool | 设备数据同步开关（同步功能本身在 S6，先落开关） |

**200 OK**：`{"user": { ... }}`（返回更新后的完整账号）。

> 语义提醒：`display_name` 传空串是"清空"，与客户端 `settings.json` 里 `nickname` 的语义一致；
> 而不传字段才是"不动"。这与 `api_key` 那种"空串=保留"的约定不同。

---

## 3. 管理面

```
GET /api/v1/admin/users?limit=50&offset=0      Authorization: Bearer <admin 令牌>
```

`limit` 默认 50、上限 200；`offset` 默认 0。

**200 OK**

```json
{ "users": [ { "...user..." } ], "limit": 50, "offset": 0 }
```

非 `admin` 角色访问 → **403 `forbidden`**；未登录 → **401 `missing_token`**。

---

## 4. 中转面（模型调用）

这是客户端接平台服务后**唯一需要改的调用目标**：把模型请求指向服务端，其余不变。

| 协议面 | 路径（两种写法都行） | 上游对应端点 |
|---|---|---|
| OpenAI Responses | `POST /responses`、`POST /v1/responses` | `POST /responses` |
| OpenAI Chat Completions | `POST /chat/completions`、`POST /v1/chat/completions` | `POST /chat/completions` |
| Anthropic Messages | `POST /messages`、`POST /v1/messages` | `POST /anthropic/v1/messages` |

> **为什么两种路径都认**：OpenAI SDK 与第三方客户端硬编码 `/v1/...`，而本项目客户端的
> `responses_endpoint()` 会把 `api_url` 尾部的 `/v1` 剥掉再拼 `/responses`。两种都支持，
> 所以客户端把 `api_url` 填成 `https://host` 或 `https://host/v1` **都能跑**。

### 4.1 服务端会做什么（客户端必须知道的五条）

1. **请求体原样透传**，只改两处：把 `model` 覆盖成平台配置的模型（**客户端传的 model 被忽略**）、
   注入平台用户 id（Responses 用 `user`、Chat 用 `user_id`、Anthropic 用 `metadata.user_id`），
   以获取上游的 KVCache 与调度隔离。
2. **必须流式**：`stream: true`。非流式返回 **400 `stream_required`**（非流式无法可靠取用量）。
3. **响应原样透传**：SSE 事件名、字段、结束标记都保持上游原样（Responses 无 `data: [DONE]`；
   Chat Completions 有 `[DONE]`；Anthropic 有 `ping`/`message_*` 事件）。
4. **上游报错原样透传**：状态码与响应体直接返回给客户端（例如上游 429、400）。
   **这类响应不扣次**。
5. **不落正文**：请求与响应正文只在内存里过一遍，服务端不存储对话内容（隐私边界，
   见 ADR-0047 决策 10/11）。服务端只记元数据：模型、token 计数、耗时、状态。

### 4.2 响应头

成功时：`Content-Type: text/event-stream`、`Cache-Control: no-cache`。
被限流时（429）：额外带 `Retry-After: <秒>`。

### 4.3 配额与限额（对外只有"次数"）

平台**对外按次数**计量（月卡另有三个滑动窗口），**不暴露 token**。额度不足返回 **402**，
`code` 用于区分该引导用户做什么：

| `code` | 含义 | 建议客户端文案/引导 |
|---|---|---|
| `no_entitlement` | 没有任何生效服务包 | 引导去兑换 |
| `uses_exhausted` | 体验包次数用尽 | 提示可兑换月卡继续 |
| `window_5h_exceeded` | 近 5 小时用量达上限 | 提示稍后再试 |
| `window_week_exceeded` | 本周用量达上限 | 提示下周额度恢复 |
| `window_month_exceeded` | 本月用量达上限 | 提示可升级套餐 |

`message` 字段里已经是可直接展示的中文文案，客户端也可以按 `code` 自己组织文案。

### 4.4 限流与并发（429）

| `code` | 含义 |
|---|---|
| `too_many_concurrent` | 同一用户同时在飞的请求超上限（默认 2） |
| `rate_limited` | 令牌桶限流（中转面默认 60 次/分钟，突发 60；按用户与按 IP 各一道） |
| `global_busy` | 服务端全局并发已满 |

均带 `Retry-After`。**客户端应做成"退避后重试"，不要把它当致命错误弹给用户**——
学生一轮对话里连发数个请求是正常形态，服务端的突发容量已按这个形态设置。

### 4.5 请求示例

```bash
# 登录拿令牌
TOKEN=$(curl -s -X POST https://api.example.com/api/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"demo@example.test","password":"<密码>"}' | sed 's/.*"token":"\([^"]*\)".*/\1/')

# Responses 面（本项目客户端默认传输）
curl -N -X POST https://api.example.com/responses \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"model":"deepseek-flash","input":"1+1=? 只回答数字","stream":true}'

# Chat Completions 面（客户端 transport=chat_completions 时）
curl -N -X POST https://api.example.com/v1/chat/completions \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"model":"deepseek-flash","messages":[{"role":"user","content":"2+2=?"}],"stream":true}'

# Anthropic 面（第三方客户端，用 x-api-key）
curl -N -X POST https://api.example.com/v1/messages \
  -H "x-api-key: $TOKEN" -H 'anthropic-version: 2023-06-01' \
  -H 'Content-Type: application/json' \
  -d '{"model":"deepseek-flash","max_tokens":64,"messages":[{"role":"user","content":"3+3=?"}],"stream":true}'
```

---

## 5. 基础设施端点

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/healthz` | 存活探针，返回 `{"status":"ok","service":"mistake-agent-server","version":"..."}`；**不碰数据库** |
| GET | `/readyz` | 就绪探针，探数据库；不可达返回 **503** |

---

## 6. 客户端接入要点（改客户端时看这一节）

> **落地情况（2026-09-30）：本节描述的「登录半程」客户端已实现**——`src/kernel/account/`（REST 客户端 + 错误归一化 + 账号服务）、`settings.json` 的 `account` 段、模型链路按令牌切换、RPC 四方法（`register` / `login` / `logout` / `get_account_status`）与事件 `account_changed` 都在了；前端有登录/注册页与左下角退出登录。
> **仍未接入**：兑换码（§7）、套餐用量查询（§7）——所以登录成功后若账号没有生效权益，模型调用会回 `402 no_entitlement`（见 §4.3），**这是正常的**，要真正用上平台中转得等 S4 把兑换码做完。决策留痕见 [ADR-0048](adr/0048-client-platform-account-integration.md) 修订节 R4–R6。

### 6.1 配置

`settings.json` 新增 `account` 段（与 `main_model` 分开，**不要把令牌塞进 `api_key`**）：

```json
{
  "account": {
    "server_url": "https://api.example.com",
    "token": "mka_...",
    "email": "demo@example.test",
    "role": "user",
    "sync_enabled": false
  }
}
```

模型链路的选择规则：

- `account.token` 非空 → `api_url = account.server_url`、`api_key = account.token`（走平台）
- `account.token` 为空 → 沿用 `main_model`（自备 Key / 本地 Ollama）

**登录纯可选**：不登录时客户端行为与今天完全一致，只是不能同步设备数据。

默认服务端地址为 `http://8.131.146.250:8080`（`settings.rs` 的 `DEFAULT_SERVER_URL`），用户可在登录页改。写入前经归一化：必须是 `http(s)://` 且带主机名，尾斜杠去掉（`normalize_server_url`）。

**客户端不允许经 `set_settings` 写令牌**：`SettingsPatch.account` 里只有 `server_url` 一个字段，`token` / `email` / `role` / `sync_enabled` 只能由登录 / 登出 / 状态刷新三条路径写。服务端的响应是这四个字段的**唯一来源**——不要在前端做"猜到身份"的推断。

### 6.2 错误分流

| 状态码 | 客户端行为 |
|---|---|
| 401 `missing_token` / `invalid_token` | 令牌失效或过期 → 清空本地令牌并引导重新登录（**不要清本地数据**） |
| 401 `invalid_credentials` | 登录页：提示邮箱或口令不正确（不要提示"账号不存在"） |
| 402 | 额度不足 → 按 §4.3 的 `code` 引导兑换或升级 |
| 403 `account_disabled` | 账号被停用 → 提示联系管理员 |
| 429 | 按 `Retry-After` 退避重试；不要弹致命错误 |
| 409 `email_taken` | 注册页：该邮箱已注册，引导去登录 |
| 400 `validation_failed` | 表单校验失败，`message` 可直接展示 |

### 6.3 与既有客户端代码的衔接

- 现有的 `map_status_error`（`src/kernel/plugin/model/mod.rs`）已把 401→`AuthFailed`、
  402→`QuotaExceeded`、429→`RateLimited`，**接入平台后这套映射可以直接复用**，
  不需要新造错误语义。
- `responses_endpoint()` 会剥掉 `api_url` 尾部的 `/v1`，所以 `server_url` 带不带 `/v1` 都行。
- 平台模式下 `check_balance`（DeepSeek 余额）语义不再适用——学生花的不是自己的余额，
  而是平台额度；应改为展示套餐与窗口余量（数据源待 S4 提供）。客户端目前已加一句提示，**尚未**隐藏这张卡片。
- 凭据落盘后要热替换模型服务：`LiveSettingsModelService::refresh()`（`set_settings` 已用的同一条路径），
  **不需要重建 Kernel**，也不需要重启应用。
- 账号方法走 `CustomMethod` 兜底 + `RpcExtension`（`AppRpc`），不进通用 `Method` 枚举（账号是业务）；
  服务端的 `{"error":{"code"}}` 的 `code` 原样成为 RPC 错误码，前端直接按 §6.2 分流即可。
- **实测提醒**：客户端的账号单测全部离线（不打网络、不落盘），「注册 → 登录 → 模型走平台 → 退出登录」
  这条链截至 2026-09-30 **一次都没跑过**（本机无 PostgreSQL/docker）。真机走查步骤见
  [docs/testing.md](testing.md) §5 最后一条，**动手前先备份 `settings.json`**。

---

## 7. 尚未实现（S4/S6 预告）

**客户端暂时不要按下面这些写代码**，但可以据此规划：

| 计划接口 | 里程碑 | 说明 |
|---|---|---|
| `POST /api/v1/redeem` | S4 | 提交兑换码；返回生效后的套餐状态。兑换码采用**注册制**（服务端对称加密存储明文，可重发） |
| `GET /api/v1/me` 扩展字段 | S4 | 增加当前套餐、三窗口剩余次数、到期日（客户端「账户与套餐」卡片的数据源） |
| `POST /api/v1/sync/push`、`GET /api/v1/sync/pull` | S6 | 会话 / 错题 / 记忆的多设备同步（默认关闭，用户显式开启） |
| `POST /api/v1/me/tokens`、`DELETE /api/v1/me/tokens/{id}` | S4 | 令牌自助管理（多设备上限与套餐绑定） |

---

## 8. 错误码总表

| `code` | 状态码 | 出现位置 |
|---|---|---|
| `validation_failed` | 400 | 账号面（邮箱/口令/昵称校验） |
| `invalid_json` / `stream_required` | 400 | 中转面 |
| `missing_token` / `invalid_token` | 401 | 所有需鉴权的接口 |
| `invalid_credentials` | 401 | 登录 |
| `no_entitlement` / `uses_exhausted` / `window_5h_exceeded` / `window_week_exceeded` / `window_month_exceeded` | 402 | 中转面 |
| `account_disabled` | 403 | 登录与所有需鉴权的接口 |
| `forbidden` | 403 | 管理面（非 admin） |
| `unknown_path` | 404 | 未知路径 |
| `email_taken` | 409 | 注册 |
| `too_many_concurrent` / `rate_limited` / `global_busy` | 429 | 中转面 |
| `login_blocked` | 429 | 登录（失败次数过多被临时封禁） |
| `upstream_unavailable` | 502 | 中转面（连不上上游） |
| `internal_error` | 500 | 任意接口（详细信息只在服务端日志里） |

> 上游自己返回的错误**不在本表内**：那时服务端会把上游的状态码与响应体原样透传给客户端。
