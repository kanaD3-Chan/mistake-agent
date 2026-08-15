# 英语练习模式（English Immersion Mode）

## 背景

面向英语练习场景，需要让 Agent 在开关开启后整体切换到全英文输出，形成沉浸式英语环境。此前系统提示与判分/出题/摘要提示均固定为中文，无法满足该场景。

## 决策

1. `settings.json` 新增 `english_mode: bool`，用户经设置页独占写，默认 `false`；沿用 ADR-0015/0027 的 `set_settings` 持久化与热更新链路。
2. 开启后，主系统提示追加英文沉浸规则，要求所有发给学生的文本必须使用英文。
3. 判分、练习即时批改、练习出题、图片理解、会话切换决策、上下文/交接摘要等提示词均跟随英文模式，确保全链路模型输出一致。
4. GUI 界面文字保持中文：只切模型对话侧，避免把界面本地化纳入本次范围。

## 实现方式

- `Settings` / `SettingsPatch` / `public_view` 增加 `english_mode`，`settings.json` 缺省字段兼容旧配置。
- `prompt.rs` 各提示函数接收 `english_mode`，在英文模式下追加对应输出规则；JSON schema、工具名、action 枚举等协议值保持不变。
- `Dispatch` 在 `ToolCallContext` 注入 `english_mode`，用户插件读取后选择提示词语言。
- `LlmTurnDecider` / `LlmSummarizer` 经共享 `settings` 读取模式，热更新后下一轮调用即生效。
- 设置页新增「英语练习模式」开关，保存时随 `set_settings` 提交。

## 影响

- 文档同步：`docs/TODO.md`、`docs/prompts.md`、`docs/api.md`、`docs/usage.md`、`README.md`、`PROJECT.md`。
- 既有中文模式行为不变；英文模式只影响模型提示词与输出语言，不影响存储结构、工具协议与 GUI 文案。
