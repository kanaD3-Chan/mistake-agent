# 前端对接说明（场景二：薄弱知识点巩固提升 + 错题本页管理）

> 面向前端：后端三个工具已全量完成并测试通过，本文是接口契约与待做工作清单。
> 调用入口：`triggerCommand(entry, params)`（唯一命令通道，勿直接拼工具名）。

## 一、后端已完成能力

1. `practice::gaps` — 薄弱点定位：聚合错题本近 N 天错题，按错误次数排序，给出建议起点难度。
2. `practice::generate` — 分层出题：15 个高频知识点确定性模板（优先）→ `difficulty=exam` 真题池抽取 → 模板未命中走 LLM 生成；自动避开近 30 天已掌握题目；几何题返回可解性校验结果。
3. `practice::check` — 即时批改：参考答案对拍优先，否则 LLM 判分；答错自动回写错题本；记录练习历史（item/知识点/对错/时间）。

## 二、接口契约

### practice::gaps

- 参数：`{subject?: string, days?: number, limit?: number}`
- 返回：按错误次数排序的薄弱点列表，每项含 `knowledge_point`、`error_count`、`suggested_difficulty`（basic/variant/advanced）

### practice::generate

- 参数：`{knowledge_point: string, difficulty: "basic"|"variant"|"advanced"|"exam"}`
- 返回：
  - 成功：`{matched: true, source?: "llm"|"exam_pool", geometry_checked?: bool, item: {...}}`
  - 未命中：`{matched: false, message?: string}`（如"该知识点近期已练习且掌握""真题池暂未收录"），需展示给用户
- `item` 字段：
  - `template_id`：唯一题目标识（**必须保存，批改时回传**）
  - `question_text`、`answer_spec`（参考答案，作答前隐藏）、`difficulty`
  - `diagram_spec?`：几何图纸（见下）
  - `source?`：真题来源，如"2021 新高考Ⅰ卷（数学）"

### practice::check

- 参数：`{question, student_answer, reference_answer?, subject?, knowledge_point?, kind?, item_id?, difficulty?}`
- 返回：`{correct, method: "exact_match"|"model", score?, total?, analysis, archived_mistake}`
- **`item_id` 必须回传 generate 返回的 `template_id`**（连同 `knowledge_point`、`difficulty`），练习历史/防重复记录才准确。

### diagram_spec（前端已有渲染器）

- 结构：`{points: {A: [x,y], ...}, objects: [{type: "segment"|"polygon"|"circle"|"right_mark"|"equal_ticks"|"angle_arc"|"label", ...}], labels: [...]}`
- 渲染：直接用 `web/src/components/GeometryFigure.vue`（props 传 `spec`），已带 DOMPurify 白名单，不要自己画。

## 三、前端待做工作

### P0（聊天内闭环，最低可用门槛）

1. `MessageBubble.vue` 接入 `GeometryFigure`：消息/工具结果里的 `diagram_spec` 渲染为几何图（当前完全没有接入，几何题不出图）。
2. 题目卡片渲染：题目文本 + 难度/来源标识 + 图形；**`answer_spec` 作答前隐藏，批改完成后才展示**。
3. 练习流程接线：出题卡片带作答输入 → 提交时回传 `item_id/knowledge_point/difficulty` 调 `check` → 展示对错/解析/"已归档错题本"。
4. `gaps` 结果渲染成薄弱点卡片列表，点击卡片直接发起 `generate`。

### P1（体验升级，建议新建练习面板）

1. 难度四层选择按钮：基础补漏 → 同类变式 → 综合拔高 → 高考真题。
2. 答题区 + "再练一题"（同点换难度 / 换知识点）。
3. 已掌握/未收录提示展示（后端 `message` 字段）。

### P2（打磨）

1. 真题来源角标、`geometry_checked` 徽标。
2. 练习历史/掌握度可视化（需后端补 `practice::history` 读取工具，前端先不做）。

## 四、关键提醒

- 几何题渲染务必用现成的 `GeometryFigure.vue`。
- `item_id` 链路（generate 返回 → check 回传）是防重复的数据基础，前端别丢。
- `answer_spec` 是参考答案，学生作答前不应看到。

---

# 错题本页管理功能前端对接说明

> 本文补充错题本页新功能：页面编辑/批量软删除、单题编辑、长按/右键自定义菜单、置顶、已掌握、追问。

## 一、后端已提供能力

- `grading::list`：继续读错题列表，默认已过滤软删除记录。
- `grading::get`：按 `id` 读取单条错题详情。
- `grading::update`：单题编辑，同时支持置顶和已掌握。
- `grading::remove`：单题软删除。
- `grading::remove_many`：批量/全选软删除。
- 删除都是软删除：只写 `deleted_at`，数据仍保留在 `mistakes.json`，列表和详情不再返回。

## 二、接口契约

### grading::list

- 参数：`{subject?: string, knowledge_point?: string}`
- 返回：`{count: number, mistakes: Mistake[]}`

### grading::get

- 参数：`{id: string}`
- 返回：`{mistake: Mistake}`；软删除后返回不存在。

### grading::update

- 参数：`{id, subject?, knowledge_point?, question?, student_answer?, reference_answer?, analysis?, is_correct?, pinned?}`
- 返回：`{mistake: Mistake}`（更新后的完整对象）
- 语义：
  - `is_correct: true` = 标记已掌握
  - `pinned: true` / `false` = 置顶 / 取消置顶
  - `reference_answer` 传 `null` 可清空，传字符串可覆盖

### grading::remove

- 参数：`{id: string}`
- 返回：`{deleted: true, id: string}`

### grading::remove_many

- 参数：`{ids: string[]}`
- 返回：`{deleted: number}`，表示成功软删除条数。

## 三、Mistake 数据字段

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | 错题唯一标识 |
| `subject` | string | 学科 |
| `knowledge_point` | string | 知识点 |
| `question` | string | 题干，Markdown/KaTeX 渲染 |
| `student_answer` | string | 学生作答 |
| `reference_answer` | string/null | 参考答案 |
| `is_correct` | boolean | `true` = 已掌握，`false` = 未掌握 |
| `analysis` | string | 错因分析 |
| `created_at` | string | ISO 8601 创建时间 |
| `pinned` | boolean | `true` = 已置顶 |
| `deleted_at` | string/null | 非空 = 已软删除，前端不应展示 |

## 四、前端需要做的事

### 1. 页面编辑模式 + 全选软删除

- 页面提供“编辑模式”入口，进入后每张卡片显示选择框。
- 提供“全选”，选中当前筛选/搜索结果的全部 `id`。
- “批量删除”调用 `grading::remove_many`，删除前二次确认。
- 成功后按返回的 `deleted` 提示“已删除 n 条”，并更新本地列表或重新调用 `grading::list`。

### 2. 单题编辑

- 编辑表单覆盖：`subject`、`knowledge_point`、`question`、`student_answer`、`reference_answer`、`analysis`。
- 提交调用 `grading::update`，用返回的 `mistake` 替换本地对应记录。

### 3. 长按/右键自定义菜单

- 在卡片上监听右键 `contextmenu` 和长按（`pointerdown` + 计时器，建议 500ms）。
- 必须 `preventDefault()`，不能弹出浏览器/WebView 默认菜单。
- 菜单位于光标或触点位置；点击外部或按 Esc 关闭。
- 菜单项：
  - **追问**：切换到聊天页，用 `send_user_message` 发送包含该错题上下文的用户消息，例如“追问这道错题：{question}”；不走 `grading` 命令。
  - **置顶**：`grading::update {id, pinned: true}`
  - **取消置顶**：仅当 `pinned === true` 时显示；`grading::update {id, pinned: false}`
  - **删除**：`grading::remove {id}`
  - **已掌握**：`grading::update {id, is_correct: true}`
- 置顶/已掌握成功后更新本地对象；删除成功后从列表移除。

### 4. 详情页/抽屉标记已掌握

- 读取 `is_correct`；为 `true` 时按钮显示为已掌握状态，颜色使用 `#16a34a`。
- 点击后调用 `grading::update {id, is_correct: true}`，成功后同步按钮状态。

### 5. 排序与展示

- 建议置顶卡片排在前面（`pinned === true`）。
- 卡片和详情可显示“置顶”徽标和“已掌握”状态色。

## 五、调用示例

```js
// 置顶
await kernel.triggerCommand("grading::update", { id, pinned: true });

// 标记已掌握
await kernel.triggerCommand("grading::update", { id, is_correct: true });

// 单题软删除
await kernel.triggerCommand("grading::remove", { id });

// 全选/批量软删除
await kernel.triggerCommand("grading::remove_many", { ids });
```

## 六、关键提醒

- `grading::get/update/remove/remove_many` 都是 UserOnly 命令，只能经 `triggerCommand` 调用；它们不会出现在模型工具列表或聊天功能中心。
- 后端错误通过 `error.code` / `error.message` 返回，前端需要展示给用户。
- 删除是软删除，前端不要尝试物理删除本地文件。
- 不要在前端维护“工具名 → 标题/图标”的映射；如需展示信息，使用 `list_tools` 返回的元数据。
