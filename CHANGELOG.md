# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
