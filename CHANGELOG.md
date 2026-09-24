# Changelog

> [!WARNING]
> **当前发布是 v0.1.0-alpha（首个 alpha 测试版）**：可能随时崩溃、数据可能丢失、API 与功能在 v0.2.0 会大幅调整。**不建议在生产环境或重要数据上使用**。`v0.1.0-alpha` 范围仅做架构里程碑落定（见下） + 五个产品场景 MVP 接入；深度增强、bug 修复、稳定性改进在后续版本。请通过 GitHub Issues 反馈问题。

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **User-driven session creation**
  ([ADR-0044](docs/adr/0044-user-driven-session-creation.md)): a new
  `create_session` RPC archives the current active session and opens a
  brand-new, independent `SessionKey`. The optional `carry_summary`
  flag controls whether the previous session's handoff summary is
  attached as the new session's first system message. Starting a new
  session while a turn is in flight is rejected (`turn_in_progress`).
- **`session_idle` event**: emitted when the user speaks again in a
  session that has been idle past the 12-hour threshold. It is a
  prompt only — the session is no longer switched automatically.
- **Session list RPCs** (follow-up to
  [ADR-0044](docs/adr/0044-user-driven-session-creation.md)):
  `open_session {key}` archives every active session and activates the
  given one (switching is not speaking — `last_activity_at` is left
  alone); `rename_session {key, title}` trims and persists a title, with
  an empty string clearing it; `delete_session {key}` removes the
  session and its JSONL file, creating a replacement empty session when
  the deleted one was active so the single-active invariant holds. All
  three are rejected while a turn is in flight (`turn_in_progress`).
- **Model-generated session titles**: `SessionMeta.title` holds the
  user-visible title (distinct from `goal`, which stays the summarizer's
  learning objective). A new `LlmTitler` plus the `session_title_prompt`
  prompt generate it in a detached task after the first turn ends — the
  turn handle is released first, so a slow auxiliary call never delays
  the reply. Failures fall back to the first 40 characters of the first user
  message's *visible* text (its `display_text` when present, so a
  forced-tool message yields a real title rather than "请调用工具 X 处理当前请求。"),
  and an already-set title is never regenerated, so a user
  rename survives. A new `session_title_updated` event refreshes the
  sidebar. New audit records: `SessionOpened`, `SessionRenamed`,
  `SessionDeleted`, `SessionTitleGenerated`.
- **`nickname` setting**: a free-text name (≤24 characters, empty string
  clears it — unlike `api_key`, where an empty string means "keep") stored
  in `settings.json` and returned by `get_settings`. It is display-only:
  the sidebar's account row shows it, falling back to 「同学」, and it
  never enters any prompt. The settings page's 通用 card gained the input.
- **Legacy session migration**: on startup, before sessions are loaded,
  `FileStorage` scans `sessions/*.jsonl` and splits files that hold
  several topics into independent sessions
  (`src/kernel/plugin/storage/file/migrate.rs`). Splits happen at
  `上一会话梗概：` boundary nodes, with descendants (including sibling
  branches) attributed to their nearest boundary ancestor; each segment
  gets a fresh `SessionKey`, keeps its message ids, and has its
  out-of-segment parent cleared. Only the segment holding the old
  `active_path` inherits the old status — no new active session is
  created. A file is split only when it holds at least two topics (topics =
  boundaries + a non-empty head), so a file whose first topic merely
  *starts* with a boundary node is still split — and a segment produced
  by an earlier run is left alone. The original file is renamed to
  `<key>.jsonl.bak` with its bytes intact; since `.bak` is not `.jsonl`,
  a second startup is a no-op. Failures only log a warning and leave the original in place.

### Changed

- **Session creation is now user-initiated**. One topic equals one
  independent session, and the session boundary is visible to the user.
  Previously "switching" was a silent in-tree fork inside a single
  `SessionKey`, invisible to the user and decided entirely by the model.
- **`InterruptBus` producers narrowed** to settings / memory /
  compaction: `SessionSwitched` and `GoalUpdated` have been removed
  along with their producers
  ([ADR-0023](docs/adr/0023-interrupt-bus.md)).
- **`start_new` semantics**: no longer a model-triggered action. The
  handoff summary is carried only when the user creates a new session
  and asks for it.
- **Audit record `SessionSwitched` → `SessionCreated`**
  (`{session, archived, summary_attached}`).
- **`list_sessions` result now includes `title`**, alongside the
  existing `key` / `goal` / `status` / `created_at` / `last_activity_at`
  fields.
- **The session list moved into the chat page**: the standalone
  "Sessions" page and its navigation entry are gone, replaced by a
  list (`web/src/components/SessionListPanel.vue`) with a "New
  chat" button, inline rename, a confirmation-guarded delete, and rows
  sorted by `last_activity_at` descending. The list lives permanently in the
  app sidebar, between the nav and the status line, rather than as a
  second column next to the chat; the sidebar is a 260px column that the
  user can collapse to a 72px icon rail with a button in the brand row
  (the choice is remembered in `localStorage`) — there is no
  hover-expand and no session-list toggle icon — and `App.vue` owns the
  list and the
  active session key, with `ChatPage` only reading the key. The chat page
  now renders
  only the active session instead of merging every session's messages
  into one stream, and switching sessions resets the streaming state.
  In-session message-version browsing (`edit_message` + `switch_branch`)
  is kept, but is now explicitly scoped to the current session and no
  longer carries any session-boundary meaning.
- **Sidebar reorganized into app chrome**: the 聊天 nav entry is gone.
  The top of the sidebar is now 「新对话」 (create a session and land on
  the chat page — still the only way to start one), 错题本 / 设置 moved
  down to a secondary nav at the bottom, and the status pill became an
  account row (avatar + nickname + status, the avatar's ring carrying
  the kernel state). Its card offers 下载手机端 / 帮助与反馈 / 退出登录 —
  all three are placeholders, since this is a local-only app with no
  account system, no server and no mobile build; clicking one says so
  in the card rather than doing nothing.
- **Single DeepSeek model** ([ADR-0045](docs/adr/0045-single-deepseek-model.md)):
  one `main_model` config (`deepseek-flash`, Responses API) now covers
  chat, scheduling, summarization and image understanding. The Responses
  API gained native image input (`input_image`), so user attachments are
  sent to the same model. `ModelKind` / `ModelRequest.model` and
  `RoutingModelService` are removed; `LiveSettingsModelService` rebuilds
  a single adapter. `check_balance` now queries DeepSeek only
  (`BalanceReport.main`, `AuditRecord::BalanceChecked { ok }`). Settings
  page drops the vision-model card and SiliconFlow balance item; the OOBE
  wizard is now three steps. The legacy `vision_model` field is kept in
  `settings.json` for backward compatibility but is never read.
  (Supersedes [ADR-0019](docs/adr/0019-model-plan-dual-endpoints.md).)
- **Images go straight into the model context**
  ([ADR-0046](docs/adr/0046-images-in-context-grading-archive.md)):
  uploaded images are stored as `uploads/` path references on the user
  message (`attachment_refs`) and resolved to `input_image` parts by a
  new `AttachmentResolvingModelService` at request time; the message
  tree no longer carries image bytes. `grading::upload` now takes the
  model's structured grading result (`{items: [...]}`) and only
  archives it — the plugin no longer calls the model or reads files.
  Text PDFs are extracted at the GUI boundary. User messages gain
  `display_text` on `send_user_message`, and the prompt set drops
  `vision_prompt` / `grading_system_prompt` in favour of the always-on
  `GRADING_GUIDANCE`.

### Removed

- **Model auto session switching** (all three sites removed together,
  superseding
  [ADR-0030](docs/adr/0030-main-model-session-switching.md),
  [ADR-0032](docs/adr/0032-new-message-pre-decision.md) and
  [ADR-0034](docs/adr/0034-switch-tool-call-not-in-context.md)):
  - the pre-turn decision in `SessionScheduler::on_new_message`;
  - the end-of-turn `LlmTurnDecider`;
  - the `session::switch` tool (the whole `src/kernel/plugin/session/`
    directory is gone).
- **`GuardModel` / `guard_prompt` / `StubGuard` / `turn_decider_prompt`**
  retired: the guard model's last caller is gone. The failure-fallback
  logic went with the decisions — there are no decisions left to fail.
  `complete_with_retry` moved to `session/summarize.rs` (shared with
  `LlmSummarizer`).
- **In-tree session forking**, including its read side
  (`scope_session_context`, `is_session_summary`, `fork_branch`) and the
  switch-frequency guard rail (1/hour limit).
- **`guard_model` placeholder plan**: it was only ever a note in
  [ADR-0025](docs/adr/0025-guard-model-and-summarizer-live.md) — it
  never landed in `settings.json`, so there is nothing to migrate.
- **`vision::read` tool and the `vision` plugin**
  ([ADR-0046](docs/adr/0046-images-in-context-grading-archive.md)): the
  separate image-reading step is gone; the model reads images directly
  from context. `map_model_error` moved into the `practice` plugin, and
  the `grading` plugin no longer requires the `Model` service.

> **Migration note**: legacy tree-structured sessions (those containing a
> summary node) used to send their entire path to the model — the summary
> node duplicates its ancestors' content, so token usage rose and the
> model could mistake the old goal for the current one. Such files are now
> **migrated on startup**: each `上一会话梗概：` boundary becomes its own
> session, and the original file is kept as `<key>.jsonl.bak` (byte-for-byte,
> so a manual rollback is possible). The migration is idempotent, non-fatal,
> and never creates an additional active session. Locally this turns the
> 152-message `0ad77bb4….jsonl` into 22 independent sessions, several of them
> only ~4 messages long — the fragments left behind by the old
> model-decided switching, now faithfully represented as one topic per
> session.

## [0.1.0-alpha] - 2026-08-19

First **alpha** release of Mistake Agent v2 as a standalone product. The
release captures the architectural milestones M1–M6 (kernel/services/RPC/
plugins/packaging/tests/docs, see `PROJECT.md` §10) plus the Windows NSIS
installer that has been tested end-to-end on Windows 10/11. The five
product scenarios are all wired into the chat, but the deeper
enhancements tracked in `docs/TODO.md` are deferred to v0.2.0 (see
"Known Limitations" below). The `-alpha` tag signals that the API surface
and feature depth may still change before v1.0; in particular, scenarios
3/4/5 are at MVP depth in this release.

### Added

- **Windows desktop application** (Tauri 2 + Vue 3, Rust 2024 edition).
- **Local-first architecture**: 100% on-device, no server, no Docker,
  no telemetry. Data root at `~/Documents/.mistake-agent/`.
- **Five learning scenarios** (all reachable from chat):
  - **Scenario 1 — Upload & auto-grade**: image/PDF upload → OCR
    (SiliconFlow Qwen3-VL) → DeepSeek-v4-flash (Responses API) →
    grading & mistake archival (`vision::read` + `grading::*`).
  - **Scenario 2 — Practice**: gap analysis, template/LLM/exam-pool
    generation, instant check with auto-archive on wrong answers
    (`practice::*`).
  - **Scenario 3 — Multi-period recap (MVP)**: weekly recap with
    total / correct rate / weakest knowledge points
    (`report::weekly`).
  - **Scenario 4 — Exam composition (MVP)**: weakness-driven paper
    assembly (`exam::compose`).
  - **Scenario 5 — Mastery tracking (MVP)**: snapshot + 7/14/30-day
    retest plan (`tracking::checkin`).
- **Self-developed agent kernel** (single Rust crate, `src/kernel/`
  + `src/plugin/`):
  - Agent loop with tool registration & dispatch
    ([ADR-0003](docs/adr/0003-two-phase-user-plugin-contract.md) …
    [ADR-0010](docs/adr/0010-tool-execution-order.md)).
  - Session scheduler with main-model-driven continue / update_goal /
    start_new decisions
    ([ADR-0030](docs/adr/0030-main-model-session-switching.md),
    [ADR-0032](docs/adr/0032-new-message-pre-decision.md)).
  - Message tree with edit / regenerate / branch switching
    ([ADR-0007](docs/adr/0007-message-tree.md),
    [ADR-0026](docs/adr/0026-message-tree-edit-and-compaction.md)).
  - Hierarchical, file-persisted memory route
    ([ADR-0008](docs/adr/0008-memory-route.md),
    [ADR-0024](docs/adr/0024-memory-file-persistence.md)).
  - Compute bridge to Pyodide (WASM sandbox in WebView)
    ([ADR-0016](docs/adr/0016-compute-backend.md),
    [ADR-0028](docs/adr/0028-compute-bridge-and-command-fallback.md)).
  - Audit (default full coverage, 10 MB rotation) and diagnostic logs
    ([ADR-0017](docs/adr/0017-audit-by-default.md),
    [ADR-0018](docs/adr/0018-diagnostic-log-levels.md)).
  - Storage I/O discipline: `DomainIo` (in-root) + `TmpIo` (staged) +
    `RelPath` (parse-time whitelisting) — see
    [ADR-0042](docs/adr/0042-scheduler-io-rule-and-runtime-data.md).
- **Dual-model configuration** (`settings.json`, user-only write):
  main model DeepSeek `deepseek-v4-flash` via Responses API, vision
  model SiliconFlow Qwen3-VL via Chat Completions. Settings hot-reload
  on save ([ADR-0015](docs/adr/0015-settings-ownership.md),
  [ADR-0027](docs/adr/0027-settings-rpc-and-hot-reload.md)).
- **English immersion mode** (`settings.json.english_mode`,
  [ADR-0043](docs/adr/0043-english-immersion-mode.md)) — model
  output switches to English while UI stays Chinese.
- **Plugin architecture**: 7 user plugins (hello, vision, grading,
  practice, report, exam, tracking) + 5 kernel plugins (storage,
  memory, compute, model, session) — registered via the two-phase
  `UserPlugin` / `KernelPlugin` contract
  ([ADR-0035](docs/adr/0035-kernel-plugin-two-phase-contract.md)),
  discovered at build time by `build.rs`
  ([ADR-0036](docs/adr/0036-build-time-plugin-discovery.md)).
- **Pyodide execution end** (numpy + sympy + mpmath) bundled into the
  application — fully offline, no CDN at runtime.
- **Tauri 2 GUI**: chat / mistakes / sessions / settings pages, OOBE
  first-run wizard, clipboard-paste image (Ctrl+V) → vision pipeline,
  Markdown + KaTeX + DOMPurify, attachment persistence.
- **43 ADRs** (`docs/adr/0001`–`0043`) documenting every
  architectural decision, plus `CONTEXT.md` glossary.
- **Windows installer** `错题 Agent_0.1.0_x64-setup.exe` (NSIS),
  built and run-tested on Windows 10 / 11.
- **CI** (`.github/workflows/ci.yml`): every push to `master` and
  every PR runs `cargo fmt --check`, `cargo test`, `cargo clippy
  --all-targets -- -D warnings`, and the full `make` build including
  the platform-specific bundle (AppImage on Linux, NSIS on Windows).
- **Release workflow** (`.github/workflows/release.yml`): on `v*`
  tag push, builds both bundles, uploads them as artifacts to a
  draft GitHub Release for review before publishing.

### Changed

- N/A (first release).

### Removed

- **`so-lite-agent/` local scaffold**: the agent core has been
  extracted into its own independent crate repository as planned
  in [ADR-0037](docs/adr/0037-so-lite-agent-crate-extraction.md).
  This repository no longer carries the `so-lite-agent/` subdirectory
  or its scaffold-only `Cargo.lock` entries (cleaned via
  `cargo update`). The standalone crate will be published to
  crates.io under a separate release process; the new repository
  is the source of truth for the extracted runtime.

### Security & privacy

- **AGPL-3.0 license** — any modification or network-served
  distribution must remain under AGPL-3.0 and must provide source
  code to recipients (see [`LICENSE`](LICENSE)). The
  `main_model.api_key` and `vision_model.api_key` are stored in
  `settings.json` in cleartext (a deliberate trade-off, see
  [ADR-0015](docs/adr/0015-settings-ownership.md) and
  `PROJECT.md` §14). Users are responsible for restricting access
  to their data root.
- The kernel never reads arbitrary local paths — staging goes
  through the system temp directory with a `mistake-agent-` prefix
  whitelist (`TmpIo`), and the data root goes through canonicalized
  domain enums (`DomainIo`). Plugins only see restricted service
  handles.

### Known limitations (MVP depth in v0.1.0, planned for v0.2.0+)

- **Scenario 3 (multi-period recap)** is MVP: only `report::weekly`
  with a `days` parameter. The rename to `report::overview` plus the
  `period` (daily / weekly / monthly / semester) axis, persistent
  weak-point tracking, answer-duration capture, and ECharts
  visualization are tracked in `docs/TODO.md` for v0.2.0.
- **Scenario 4 (assessment)** is MVP: `exam::compose` assembles a
  paper but has no `paper_type` (quiz / unit / midterm / final /
  gaokao) mapping, no timed-answer flow, and no exam-pass mastery
  auto-marking yet.
- **Scenario 5 (long-term tracking)** is MVP: `tracking::checkin`
  only. The knowledge graph (`tracking::graph`,
  `tracking::graph_query`), the proactive-retest loop, and the
  persistent weak-point aggregator are not yet implemented.
- **Mistake storage** is the current single-file `mistakes.json`
  form. The directory-based mistake storage with event log + mastery
  schedule (ADR-0039 / ADR-0040) is planned for v0.2.0.
- **Camera capture** (`getUserMedia` in WebView2) and **voice
  input** (SiliconFlow SenseVoice transcription) are not yet
  implemented.
- **Mobile targets** (Android via Tauri v2, then iOS / iPadOS) are
  not yet implemented.
- The full roadmap lives in [`docs/TODO.md`](docs/TODO.md). Items
  that fall under "2026 任务 3" course-project deliverables
  (demo videos, prompt evaluation report, LangChain / LangGraph
  trade-off write-up, project retrospective, agent workflow
  diagram) are also tracked there and are not part of v0.1.0.

[0.1.0-alpha]: #0100-alpha--2026-08-19
[Unreleased]: #unreleased
