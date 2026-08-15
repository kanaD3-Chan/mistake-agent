# 使用说明

## 1. 前置配置

数据根目录：`~/Documents/.mistake-agent/`（Windows：`%USERPROFILE%\Documents\.mistake-agent\`）。首次运行前创建 `settings.json`：

```json
{
  "log_level": "info",
  "english_mode": false,
  "main_model": {
    "api_url": "https://api.deepseek.com",
    "api_key": "你的 DeepSeek key",
    "model": "deepseek-v4-flash",
    "transport": "responses"
  },
  "vision_model": {
    "api_url": "https://api.siliconflow.cn/v1",
    "api_key": "你的 SiliconFlow key",
    "model": "Qwen/Qwen3-VL-32B-Instruct"
  }
}
```

## 2. 启动

```bash
# 改过前端后先构建（否则用旧的嵌入资源）
cd web && npm install && npm run build && cd ..

# 开发运行
cargo run --bin mistake-agent
```

## 3. 界面操作

- **聊天**：底部输入框发消息，Enter 或「发送」。
- **显式调用工具**：在输入框输入工具名（如 `practice::generate`），前端弹出工具候选框，**按 Tab 确认**后输入框显示工具徽章与淡色 `<可选参数>` 占位；输入参数（可省略）后 Enter 发送，Agent 会**强制调用该工具**并基于结果在聊天中讲解（不绕过 LLM）。工具清单、名称、图标、用法示例全部来自后端。
- **批改作业**：点「作业」选择图片/PDF → 自动生成"请批改这份作业：<路径>"并发送 → Agent 调 `grading::upload`。
- **附件展示**：上传的图片会直接渲染在聊天气泡里，PDF 显示图标与文件名；点击可查看大图或完整 PDF。附件持久保存在数据根目录 `uploads/`，不随系统临时文件清理而丢失。
- **连续历史**：聊天记录是一条从第一次使用到现在的连续消息树（切换会话不会清空），会话历史页同样展示全部记录。
- **错题本**：左侧导航「错题本」直接查看错题列表（学科/知识点过滤、题目/作答/参考答案/错因）；也可在聊天里问"错题本里有什么"。
- **会话历史**：左侧导航「会话历史」查看/回放历史会话；聊天中可对 assistant 消息编辑/重新生成，用 < > 切换消息树分支。
- **设置**：左侧导航「设置」配置主模型/视觉模型的 URL、Key、模型 ID、接入方式与日志级别；「英语练习模式」开启后，模型对话、判分、出题与复盘统一使用英文，界面文字保持中文；Key 只显示"已设置"状态，留空表示保留原 Key。
- **停止**：回答/批改过程中点「停止」立即中止（回合中发送按钮禁用，先停止再发新消息）。
- **思考过程**：模型推理增量默认折叠在"思考过程"卡片里，点击展开/折叠；不展示给学生也随时可查。
- **工具进度**：批改中底部显示"grading::upload：正在识别…"等进度。
- **验算**：让 Agent 验算数学题（如"用 Python 验证 3x+5=11 的解"），Agent 调 `compute::verify`，代码在应用内 Pyodide（WASM 沙箱）执行。
- **练习/复盘/组卷/追踪**：聊天里说"生成一道全等三角形变式题 / 我的薄弱点在哪 / 批改一下这道题 / 给我周复盘 / 出 5 道薄弱点试卷 / 检查我的掌握度"，Agent 分别调 `practice::generate`、`practice::gaps`、`practice::check`、`report::weekly`、`exam::compose`、`tracking::checkin`。

## 4. 数据与产物

| 路径 | 内容 |
|---|---|
| settings.json | 模型配置与 key（用户独占写） |
| sessions/<key>.jsonl | 会话消息树（首行元数据） |
| mistakes/mistakes.json | 错题本 |
| memory/ | 记忆条目（文件持久化；中文路径编码落盘，应用层路径不变） |
| uploads/ | 作业附件持久副本 |
| data/ | 运行时教学数据（真题池等） |
| audit/audit.jsonl | 审计（10MB 轮转） |
| logs/ | 分级诊断日志（10MB 轮转） |

## 5. 常见问题

- 发送后没反应：确认右上角状态为"就绪"（内核自检通过）；查看 `~/Documents/.mistake-agent/logs/` 诊断日志。
- 扫描版 PDF：提示不支持，请拍照成图片再上传。
- 模型报"余额不足/模型不可用"：检查对应 key 与官方模型状态。
