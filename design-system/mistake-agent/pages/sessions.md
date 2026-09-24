# 页面覆盖：会话列表（session-list，应用侧栏内）

基于 MASTER.md。原「会话历史」独立页已删除（ADR-0044 收尾）——会话列表不再是导航项，
也不再是聊天页里的独立一列，而是**应用侧栏 `.sidebar` 的常驻区块**（「新对话」按钮与底部导航
之间，挂点 `.sidebar-sessions`，[SessionListPanel.vue](../../../web/src/components/SessionListPanel.vue)），
数据经 `list_sessions` / `read_session` / `create_session` / `open_session` / `rename_session` / `delete_session` RPC。
本文件对该区域覆盖 [chat.md](chat.md)。

- 侧栏固定 260px（`.sidebar`），不随 hover 变化，也没有「会话列表」开关图标——列表常驻。
  品牌行右端有一个手动收起开关（`.sidebar-toggle`，mdi:chevron-left / chevron-right）：收起后
  侧栏变 72px 图标条，文字与会话列表一起隐藏（要切会话就先展开），选择记在 `localStorage`
  的 `ma:sidebar-collapsed`。收起态下「新对话」退成和导航项同款的纯图标按钮，提示沿用浏览器
  原生 `title`（和「错题本 / 设置」一致），不另画 tooltip。
- 侧栏从上到下：品牌行 → 「新对话」按钮（`.sidebar-top`，占原「聊天」导航位，主色占满整行；
  回合在飞（busy）时禁用，避免与半截回答抢会话）→ 会话列表 → 底部次级导航（错题本 / 设置）
  → 用户区（头像 + 昵称 + 状态，点开占位菜单）。
- 列表行：标题（单行省略）+ 相对时间副行；当前会话高亮（左侧主色条 + 底色）。按 `last_activity_at` 倒序。
- 标题回退链：`title` → `goal.text` → 「新会话」；缺字段不得渲染空白行。
- 行内操作：重命名（内联输入，形态同消息气泡的 `.edit-inline`）、删除（二次确认弹窗，形态同错题本
  `.confirm-overlay` / `.confirm-dialog.card`，主按钮 `.btn ghost`、危险按钮 `.btn danger`）。
- 会话边界气泡：迁移后每条会话可能以「上一会话梗概」system 消息开头，渲染为可折叠分隔气泡
  （`<details>`，图标 mdi:content-cut），默认折叠——它是背景，不是对话内容。
- 空状态：mdi:history + 提示文案；接口未接通时展示明确错误而非空白。
- 状态归属：`App.vue` 持有会话列表与 `activeSessionKey`（列表是应用级 chrome，哪个页面都在）；
  点会话若不在聊天页会切回聊天页，因为会话内容在聊天区。
- 触屏（hover:none）：行内操作按钮常显；桌面端 hover 显示、focus-within 常显。
- 手机（< 768px）：侧栏变为底部标签栏，只留「错题本 / 设置」；会话列表、「新对话」与用户区
  整体隐藏（`.sidebar-sessions` / `.sidebar-top` / `.user-box { display: none }`）。
