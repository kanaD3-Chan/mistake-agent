# 错题 Agent（Mistake Agent v2）

面向中学生的**本地智能错题管理 + 辅助学习 Agent**：上传图片/PDF，先读图再按你的意图行动——讲解题目或判分归档错题，还能生成变式练习、周复盘报告、组卷与掌握度追踪。桌面应用（Tauri + Vue），数据与模型密钥全部保存在本机。

## 功能

- **上传图片/PDF**：可一次选多张、混合图片与 PDF，附件挂在输入框上方（不进入聊天气泡），发送后模型逐个理解（`vision::read`：作业转写、角色/照片等描述内容），再根据图片内容与你的话决定——要批改就逐题判分、错题自动归档（LaTeX 公式增强渲染），只想讲解/描述就讲解。
- **五个学习场景**：批改、变式练习（`practice::generate` 模板/LLM 智能出题，几何题带图形规格并经可解性对拍；`practice::gaps` 薄弱点定位；`practice::check` 即时批改，答错回写错题本；高考真题池）、周复盘（`report::weekly`）、组卷（`exam::compose`）、掌握度追踪（`tracking::checkin`，7/14/30 天重测计划）。
- **显式工具调用**：输入功能名（如"生成练习题"）按 Tab 确认，或点输入框上方的工具按钮；模型被强制调用该工具并基于结果在聊天中讲解——不绕过 LLM。
- **连续对话历史**：聊天记录是从第一次使用到现在的完整消息树（会话切换无感知、旧消息自动携带），支持编辑/重新生成与分支切换。
- **跨会话记忆**：`memory::save/show/remove` 文件持久化，重启不丢。
- **Python 验算**：`compute::verify` 在应用内 Pyodide（WASM 沙箱）执行。
- **英语练习模式**：设置页开启后，对话、判分、出题与复盘全部以英文输出，界面文字保持中文。
- **安全与鲁棒**：文件只经系统临时目录暂存、kernel 不读任意本地路径；守卫/摘要/回合对瞬时错误自动重试；审计默认全覆盖、日志脱敏。

## 插件开发

- Kernel 开发手册：[docs/kernel-dev.md](docs/kernel-dev.md)
- 用户插件手册：[docs/plugin-dev/user.md](docs/plugin-dev/user.md)
- 内核插件手册：[docs/plugin-dev/kernel.md](docs/plugin-dev/kernel.md)
- 参考模板（复制即开工，构建期自动发现）：[docs/plugin-dev/reference/](docs/plugin-dev/reference/)

## 架构

```
Tauri GUI（Vue 3，进程内 Kernel，standalone 单二进制）
        │  RPC（Tauri Channel/命令桥接，JSON Lines 协议）
        ▼
Kernel（agent loop · 工具注册与调度 · 会话调度（主模型决策）· 审计）
        ├─ 内核插件：storage · memory · compute · model · session（KernelPlugin 两段式契约，ADR-0035）
        └─ 用户插件：vision · grading · practice · report · exam · tracking
```

- 主模型：DeepSeek `deepseek-v4-flash`（Responses API，thinking + 工具调用）
- 视觉模型：SiliconFlow `Qwen/Qwen3-VL-32B-Instruct`（图片理解：作业转写、其它图片描述内容，不判分）
- 会话调度：主模型在回合边界做 continue / update_goal / start_new 决策（ADR-0030/0032），失败一律"存疑即继续"

## 快速开始

### 前置配置

首次运行前在数据根目录 `~/Documents/.mistake-agent/settings.json` 配置模型密钥：

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

也可以在应用「设置」页里填写（Key 只显示"已配置"状态，不回显）。

### 构建与运行

推荐用仓库根的 **Makefile** 一键完成（跨平台，自动增量；产出落在 `target/`）：

```bash
make               # 完整构建：前端 + Rust release + 安装包
make help          # 列出所有可用 target
make build-rust    # 只重编 Rust（前端未变则跳过 vite build）
make clean         # 清掉 target/、web/dist/、web/node_modules/
```

#### 前置依赖

| 工具 | 版本要求 | 安装方式 |
|---|---|---|
| **Rust** | 2024 edition | [rustup.rs](https://rustup.rs/) 安装后 `rustup default stable` |
| **Node.js** | 18+ | [nodejs.org](https://nodejs.org/) 或包管理器（`apt install nodejs`） |
| **GNU Make** | 4.x | 见下表 |

**GNU Make 安装（按平台）：**

| 平台 | 安装命令 | 说明 |
|---|---|---|
| **Linux** | `sudo apt install make`<br>`sudo dnf install make` | 包管理器自带 |
| **macOS** | `brew install make` | Apple 自带的是 BSD make，不支持本项目语法 |
| **Windows** | `winget install ezwinports.make` | 装完后需把 `%LOCALAPPDATA%\Microsoft\WinGet\Packages\ezwinports.make_*\bin` 加进系统 PATH，或每次运行前：<br>`$env:PATH="$env:LOCALAPPDATA\Microsoft\WinGet\Packages\ezwinports.make_Microsoft.Winget.Source_8wekyb3d8bbwe\bin;$env:PATH"`<br>（PowerShell） |

**Windows 额外依赖（打包时）：**
- Git for Windows（Makefile 内部用 Git Bash 的 `sh.exe` 执行 recipe）
- NSIS（Tauri bundler 自带，无需手动安装）
- WebView2（Win10 1809+ / Win11 已预装）

#### 构建产物

| 平台 | 可执行文件 | 安装包 |
|---|---|---|
| **Windows** | `target/release/mistake-agent.exe` | `target/release/bundle/nsis/*.exe` |
| **Linux** | `target/release/mistake-agent` | `target/release/bundle/deb/*.deb`<br>`target/release/bundle/appimage/*.AppImage` |
| **macOS** | `target/release/mistake-agent` | `target/release/bundle/macos/*.dmg` |

`make bundle` 会按当前平台自动选安装包格式。

#### 等效手动步骤（调试 Makefile 失败时用）

```bash
cd web && npm ci && npm run fetch:pyodide && npm run build && cd ..
cargo build --release --bins                    # 只编译二进制
cargo tauri build --bundles nsis                # Windows 打包
cargo tauri build --bundles deb appimage        # Linux 打包
cargo tauri build --bundles dmg                 # macOS 打包
```

> **`npm run fetch:pyodide` 为什么必须：**  
> 预热 numpy/sympy/mpmath 离线 wheel，构建期必需。产物随 `dist/pyodide/` 打包进应用，运行期不依赖 CDN。清过 `node_modules` 后必须重跑一次。
>
> 完整构建流程（含 Pyodide 离线包说明、Makefile 各 target 含义、常见问题）见 [docs/build.md](docs/build.md)。

### 开发命令

```bash
cargo test                                  # 单元测试
cargo test --test live_api -- --ignored     # 真实 API 集成测试（需已配置 key）
cargo clippy --all-targets -- -D warnings
cargo fmt --check
make build-frontend                         # 只重建前端
make build-rust                             # 只重编 Rust release
cd web && npm run check:pyodide             # Pyodide 真实执行自检（算术/符号计算/物理/numpy）
cd web && node scripts/katex-check.mjs      # LaTeX 渲染链路自检
```

## 数据目录（`~/Documents/.mistake-agent/`）

| 路径 | 内容 |
|---|---|
| `settings.json` | 模型配置与密钥（用户独占写） |
| `sessions/` | 会话消息树（JSONL，含思维链与工具调用记录） |
| `mistakes/` | 错题本 |
| `memory/` | 跨会话记忆 |
| `uploads/` | 作业附件持久副本（图片/PDF 展示用） |
| `data/` | 运行时教学数据（真题池、依赖表等） |
| `audit/` | 审计（10MB 轮转） |
| `logs/` | 分级诊断日志 |

## 文档

- [PROJECT.md](PROJECT.md) — 项目总览（唯一入门文档）
- [CONTEXT.md](CONTEXT.md) — 术语表
- [docs/kernel-dev.md](docs/kernel-dev.md) — Kernel 生命周期、模块职责与扩展边界
- [docs/api.md](docs/api.md) — GUI ↔ kernel RPC 协议与真实模型对接
- [docs/build.md](docs/build.md) — 构建流程与 Pyodide 离线包步骤
- [docs/adr/](docs/adr/) — 架构决策记录（43 条）
- [docs/plan/so-lite-agent.md](docs/plan/so-lite-agent.md) — Agent core 剥离方案归档（ADR-0037；crate 已迁出至独立仓库）
- [docs/prompts.md](docs/prompts.md) / [docs/testing.md](docs/testing.md) / [docs/usage.md](docs/usage.md) — Prompt、测试与使用记录
- [docs/variants.md](docs/variants.md) — 变式出题设计（场景二，已按设计落地）
- [CHANGELOG.md](CHANGELOG.md) — 版本变更记录（Keep a Changelog）
- [`.github/workflows/ci.yml`](.github/workflows/ci.yml) / [`.github/workflows/release.yml`](.github/workflows/release.yml) — CI（PR + push）与 tag 触发的 release workflow

## 许可证

[AGPL-3.0](LICENSE)。本项目允许参考开源工程机制（协议见 PROJECT.md §2 开源策略），业务逻辑自研。
