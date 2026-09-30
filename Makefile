# Mistake Agent 构建入口（GNU Make 4.x，POSIX 兼容，跨平台）
#
# 产物（全部位于项目根 target/ 下）：
#   target/release/mistake-agent(.exe)      —— release 可执行文件
#   target/release/bundle/nsis/*.exe         —— NSIS 安装包（Windows；bundle.active=true 时产出）
#   target/release/bundle/deb/*.deb / *.AppImage —— Linux 安装包（target 决定）
#   target/release/bundle/macos/*.dmg       —— macOS 安装包（target 决定）
#
# 用法：
#   make                   # 等价于 make all
#   make all               # 完整构建（前端 + Rust release + 安装包）
#   make build-frontend    # 只重建 web/dist
#   make build-rust        # 只重新编译 Rust release 二进制
#   make bundle            # 只重打安装包
#   make fetch-pyodide     # 单独下载 Pyodide 离线 wheel
#   make clean             # 清理 target/、web/dist/、web/node_modules/
#   make help              # 列出所有 target
#
# 跨平台安装 GNU Make：
#   Linux   : 包管理器自带（apt/dnf/pacman）
#   macOS   : brew install make（Apple 自带 BSD make 不够用）
#   Windows : winget install ezwinports.make  —— 永久加进系统 PATH
#             或临时：export PATH="$LOCALAPPDATA/Microsoft/WinGet/Packages/ezwinports.make_*/bin:$PATH"
#
# 依赖：Rust 2024 toolchain + Node.js 18+ + Git（仅 Windows 用 Git Bash 时需要）
#
# ─────────────────────────────────────────────────────────────────────────
# Windows shell 选择（PowerShell / cmd / Git Bash 都可）：
#   - 默认 Make 4.4 (ezwinports) 在 Windows 用 cmd.exe，cmd 没有 printf / awk，
#     会导致 recipe 失败。
#   - 强制走 Git Bash 的 sh.exe 即可：recipe 用 POSIX 工具 (printf / awk / rm)，
#     与 Linux/macOS 完全一致。
#   - 路径按本机常见安装位置探测；若你的 Git 装在别处，临时覆盖：
#       make SHELL='C:/path/to/sh.exe'
# ─────────────────────────────────────────────────────────────────────────
ifeq ($(OS),Windows_NT)
    # Git Bash 自带 sh.exe；按常见安装路径探测。优先级：PATH → 默认安装位置。
    ifdef GIT_BASH
        SHELL := $(GIT_BASH)
    else ifneq ($(wildcard C:/Program Files/Git/usr/bin/sh.exe),)
        SHELL := C:/Program Files/Git/usr/bin/sh.exe
    else ifneq ($(wildcard C:/Program Files/Git/bin/sh.exe),)
        SHELL := C:/Program Files/Git/bin/sh.exe
    endif
    # 显式声明要用 bash 执行 recipe（POSIX 模式，避免 dash-only 行为差异）
    .SHELLFLAGS := -c
endif

# ---------------------------------------------------------------------------
# 平台探测（决定 .exe 后缀 / install 命令 / bundle 格式）
# ---------------------------------------------------------------------------

ifeq ($(OS),Windows_NT)
    EXE_EXT      := .exe
    PLATFORM_TAG := windows
    # Windows 上的 rm 由 Git Bash 或 MSYS sh 提供；若用纯 cmd.exe 会失败
    RM           := rm -rf
    # tauri 默认的 Windows installer 是 NSIS；其他 bundle 格式可在此追加
    BUNDLE_TARGETS := nsis
else
    EXE_EXT      :=
    PLATFORM_TAG := $(shell uname -s | tr '[:upper:]' '[:lower:]')
    RM           := rm -rf
    # Linux 优先 deb（Debian/Ubuntu 系），macOS 用 dmg
    ifeq ($(PLATFORM_TAG),linux)
        BUNDLE_TARGETS := deb appimage
    else ifeq ($(PLATFORM_TAG),darwin)
        BUNDLE_TARGETS := dmg
    else
        BUNDLE_TARGETS :=
    endif
endif

# ---------------------------------------------------------------------------
# 工具与产物路径
# ---------------------------------------------------------------------------

ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))

BIN          := target/release/mistake-agent$(EXE_EXT)
BUNDLE_DIR   := target/release/bundle
FRONTEND_DST := web/dist
# 以**入口文件**而不是目录当 make 目标：目录的 mtime 会被"往目录里写任何文件"刷新
# （例如把 pyodide 拷进 dist/），于是 make 误判前端已是最新而**跳过 vite build**，
# 最终编出一个没有 index.html 的 exe（2026-09-30 实测踩到）。
FRONTEND_ENTRY := $(FRONTEND_DST)/index.html
# 前端源码清单（git ls-files 跨平台；含未跟踪的新文件，避免新增文件不触发重建）
FRONTEND_SRC := $(shell git ls-files web/src web/index.html web/vite.config.js) $(shell git ls-files --others --exclude-standard web/src)

# Pyodide 离线 wheel 标记（任一文件存在即视为已下载）
PYODIDE_WHEEL := web/node_modules/pyodide/numpy-2.4.3-cp314-cp314-pyemscripten_2026_0_wasm32.whl

# ---------------------------------------------------------------------------
# 默认与伪目标
# ---------------------------------------------------------------------------

.PHONY: all help build-frontend fetch-pyodide build-rust bundle clean check-tools

all: bundle ## 完整构建（默认）

help: ## 列出所有可用的 target
	@$(info 可用 target：)
	@$(info   all              —— 完整构建（默认）)
	@$(info   help             —— 列出所有可用的 target)
	@$(info   check-tools      —— 校验 node / npm / cargo 可用)
	@$(info   fetch-pyodide    —— 预热 Pyodide numpy/sympy/mpmath 离线 wheel)
	@$(info   build-frontend   —— 构建前端（vite build）)
	@$(info   build-rust       —— 编译 Rust release 二进制)
	@$(info   bundle           —— 打安装包（NSIS / deb / AppImage / dmg）)
	@$(info   clean            —— 清理 target/、web/dist/、web/node_modules/)
	@echo （以上由 Make 内置 info 函数输出，不依赖外层 shell）

check-tools: ## 校验 node / npm / cargo 可用
	@echo "[check] 校验构建工具...（若下列命令失败则缺少对应工具）"
	@node --version
	@npm --version
	@cargo --version
	@echo "[OK] 所有工具均可用（平台：$(PLATFORM_TAG)）"

# ---------------------------------------------------------------------------
# 阶段 1：前端依赖 + Pyodide 离线包
# ---------------------------------------------------------------------------

# 安装 npm 依赖 —— 增量：package-lock.json 变化时才重跑
web/node_modules/.package-lock.json: web/package.json web/package-lock.json
	@echo "[build] npm ci 装前端依赖"
	@cd web && npm ci

# 下载 Pyodide 离线 wheel —— 一次性，文件存在即跳过
fetch-pyodide: $(PYODIDE_WHEEL) ## 预热 Pyodide numpy/sympy/mpmath 离线 wheel

$(PYODIDE_WHEEL):
	@echo "[build] fetch:pyodide 下载 numpy/sympy/mpmath 离线包"
	@cd web && npm run fetch:pyodide

# ---------------------------------------------------------------------------
# 阶段 2：构建前端 web/dist
# ---------------------------------------------------------------------------

build-frontend: $(FRONTEND_ENTRY) ## 构建前端（vite build）

$(FRONTEND_ENTRY): web/node_modules/.package-lock.json $(PYODIDE_WHEEL) $(FRONTEND_SRC)
	@echo "[build] vite build"
	@cd web && npm run build

# ---------------------------------------------------------------------------
# 阶段 3：Rust release 构建
# ---------------------------------------------------------------------------

build-rust: $(BIN) ## 编译 Rust release 二进制

# 增量：src/ 或 Cargo.toml 变化才重编
$(BIN): $(FRONTEND_ENTRY) $(wildcard src/**/*.rs src/*.rs Cargo.toml build.rs)
	@echo "[build] cargo build --release --bins"
	@cargo build --release --bins
	@echo "[OK] release 二进制就绪: $(BIN)"

# ---------------------------------------------------------------------------
# 阶段 4：Tauri 安装包
# ---------------------------------------------------------------------------

bundle: $(BUNDLE_DIR) ## 打安装包（NSIS / deb / AppImage / dmg）

# 安装包目录存在即视为完成 —— Tauri bundler 一次产出多个 target
$(BUNDLE_DIR): $(BIN)
	@echo "[build] cargo tauri build（bundles: $(BUNDLE_TARGETS)）"
	@cargo tauri build --bundles $(BUNDLE_TARGETS)
	@echo "[OK] 安装包就绪: $(BUNDLE_DIR)"

# ---------------------------------------------------------------------------
# clean
# ---------------------------------------------------------------------------

clean: ## 清理所有构建产物（target/、web/dist/、web/node_modules/）
	@echo "[clean] 删除 target/"
	@$(RM) target
	@echo "[clean] 删除 web/dist/"
	@$(RM) $(FRONTEND_DST)
	@echo "[clean] 删除 web/node_modules/"
	@$(RM) web/node_modules
	@echo "[OK] 清理完成"
