# Forge Bring-up — Execution Plan (living document)

> Updated by the agent as work completes. Status words: `TODO` / `DOING` / `DONE`.
> TDD is mandatory — see `AGENTS.md`. Blueprint: `error_logs.md` (read-only).

## Locked decisions

- In-place rebuild at repo root; `error_logs.md` stays tracked as reference.
- `tuirealm 4` + `ratatui 0.30` + `crossterm 0.29`; PTY grid is a raw custom view.
- Linux-first; macOS raster/screenshot degrade.
- **Memory feature skipped entirely** (no db, tools, config, embeddings).
- First harnesses: `claude`, `codex`, `muse` via extensible `cli_tool` registry.
- Rust 2021, `forge` 1.0.0. Cargo via login shell (`bash -lc`).

## Phase 0 — Bootstrap — STATUS: DONE (canonical `cargo test` green 3/3)

- [x] `cargo init --bin --edition 2021 --name forge` in place; `Cargo.toml`
      (forge 1.0.0), `Makefile` (`build`/`test`), `.gitignore` (`/target`); `.git` created by init.
- [x] `src/main.rs`: `--help`, `--version`, unknown-arg error, no-command TUI stub.
- [x] `tests/cli.rs`: 3 integration tests, RED observed (0/3 vs hello-world stub), GREEN after stub (3/3).
- Gate: `cargo build` + `cargo test` green — **verified**: `test result: ok.
  3 passed; 0 failed` via the project's own `cargo test` (plus manual rustc+cc
  pipeline earlier the same day).

### Blocker (2026-09-12): sandbox denied cargo/rustc child spawns — RESOLVED

- `cargo build`/`cargo test` failed with `could not execute process ... rustc ...
  (never executed) / Operation not permitted (os error 1)` under
  `bwrap --seccomp-exec proxy-only`. User lifted the sandbox restriction;
  canonical `cargo test` now green. Manual `/tmp` rustc+cc driver retired.

## Phase 1 — Foundations — STATUS: DONE (35 green: 31 unit + 4 integration)

- [x] `branding` (names/paths), `ids` (run IDs), `safe_text` (hostile-text display),
      `paths` (relative jail + symlink reject), `fs_atomic` (tmp/fsync/rename + 0600),
      `config` (defaults, unknown-section preservation, legacy migration),
      `theme` (semantic roles, no RGB), `logging` (1 MiB rotation, sanitized).
- [x] `main.rs` headless startup: migrate → load → materialize config/audit/log → run ID.
- [x] Gate tests: defaults, unknown-section roundtrip, partial-fill, corrupt/bad-value
      errors, migration 4-case matrix, hostile-text, mixed-script flag, no-RGB audit,
      atomicity, 0600, I/O-error propagation, CLI red→green, first-launch e2e.
- Known warnings (resolve as phases land): `RunId::parse` (Phase 4 MCP auth),
  `confine` (Phase 6 file transfer), `safe_text` display fns (Phase 3 permission UI).

- [ ] `branding` (naming constants), `errors/logging` (bounded, terminal-safe).
- [ ] `theme` (semantic APIs only) + `safe_text` + path-jail checks.
- [ ] `config` (`~/.forge/config.toml`, no memory section; defaults: prefix
      ctrl-b, perm Off, AI gpt-4.1-mini/thr 0.6/10s; unknown-section preserve;
      mutex-serialized writes) + `~/.ccpp`→`~/.forge` migration.
- [ ] `ids` (run-ID/enums) + `fs_atomic` (tmp/fsync/rename, 0600 helpers).
- Gate tests: config-defaults, unknown-TOML-preserved, atomicity,
  legacy-dir-collision, corrupt-save, full-disk-error, hostile-text-safety,
  theme-no-inline-RGB.

## Phase 2 — Terminal core, first pixels — STATUS: DONE, manually testable (81 green: 77 unit + 4 integration)

- [x] `AppEvent` protocol (Send) + `from_pty` mapping + single-owner `AppState` reducer.
- [x] `SessionManager` (order/active/run-index/rebind/retention) + `PtyPane`
      (portable-pty 0.8 reader/writer/resize/kill, shared-child reaping).
- [x] Prefix `Ctrl-b` router v1 (forward/prefix/quit/next/prev/cancel).
- [x] vt100 screen parse + grid render, tabs skeleton (`ui::render_grid`).
- [x] Real main-loop (~16ms, drain cap, dirty render) + terminal setup/restore
      (raw/alt-screen, panic hook, Drop guard; clean `Ctrl-b q` exit 0
      verified in live PTY).
- [x] Key-forwarding fix: `encode_key` accepts SHIFT for `Char` keys
      (terminals report uppercase/symbols as SHIFT+char); live PTY
      `echo SMOKE-OK` renders on screen. Test:
      `input::tests::shift_char_keys_encode_to_pty_bytes`.
- [x] Multiline pane bodies: `encode_multiline_for_display` preserves `\n`
      row breaks (single-line encoding flattened the screen to one line).
- [x] Resize-on-spawn: new panes are fitted to their grid area immediately
      (were hardcoded 24x80 until the next outer resize; broke lazygit
      layout). Test: `tui::tests::new_session_fits_current_grid`; live
      `stty size` reports the fitted 21x78.
- [ ] Pane-content color: `screen_text` is plain text end to end (vt100 cell
      attributes dropped, `Paragraph` has no spans), so full-screen apps
      render monochrome. Needs a styled-row view model + SGR→theme mapping
      (+ advertise a capable `TERM` to children). Next render-fidelity item.
- [ ] `tuirealm` chrome host (buttons/mouse) — deferred to Phase 3+ chrome work.
- Still open: flood-cannot-starve-input gate test.

## Phase 3 — Hooks + permissions — STATUS: TODO

- [ ] `hook-relay` (fail-open, exit 0, ~3s sync timeout) for claude/codex/muse.
- [ ] Listener: `0600` socket + loopback TCP, 32-conn cap, no-prefetch first line.
- [ ] Policy/cache/audit (`audit.log` 0600, block-wins); modes Off, Safe-Only,
      YOLO (AI-Assisted deferred to Phase 7).
- [ ] `tui-realm` permission modal (keyboard nav + mouse click).
- Gate tests: relay-never-blocks; block-wins; audit-0600-enforced;
  hostile-display-safe.

## Phase 4 — Comms core — STATUS: TODO

- [ ] `mcp-serve` (newline JSON-RPC 2.0, MCP 2025-03-26, dynamic init context).
- [ ] Run-ID auth + group ACL + pressure cap 5; idle/debounce injection.
- [ ] Tools: `ask/send_response/tell/ack/list_sessions`; exit-fails-conversation.
- Gate tests: no-group-no-transfer; forged/stale-run-ID rejected; pressure-cap;
  target-exit-determinism. Manual: two live sessions ask/tell.

## Phase 5 — Projects, tasks, walkthrough, timers, recovery — STATUS: TODO

- [ ] Projects registry + cwd matching; checklists/tasks (10 tools); walkthrough
      (5 tools) with `<walkthrough-question>` injection; timers (10 tools' worth:
      schedule/cancel + once/interval/daily).
- [ ] Persistence: `forge.log`, `tasks.db`, `projects.db`,
      `session_history.json`, `sessions/*.json` checkpoints (~2s coalesced) +
      Save & Quit + crash recovery.
- Gate tests: migration/CRUD/reorder/focus invariants; checkpoint round-trip;
  corrupt-save quarantine; live-checkpoint refusal.

## Phase 6 — Full local tool surface — STATUS: TODO

- [ ] `terminal_exec/read/send` (Unix, one lease/session, supervisor reap).
- [ ] `compact/start_session/status/message_user`, file offer/accept
      (1MiB/file, 4 pending, 4MiB staged, 5min TTL, 0600 no-overwrite).
- [ ] `visual_show` (decode/budget/generation ordering; raster stubbed).
- Gate tests: supervisor no-zombie/CTRL-C-race/UTF-8-truncation; file
  digest/expiry/symlink-swap negatives; tool-count check.

## Phase 7 — Deferred: teams, VCS, whiteboard-bk, remote, AI — STATUS: TODO

- [ ] 7a: ephemeral team builder (+ `forge-team-mode` skill), VCS
      (Git lazygit / Sapling smartlog), whiteboard backend + limits,
      Claudling.
- [ ] 7b: SSH/OD remote + reverse-forwarding, mobile (GChat/Telegram),
      macOS raster/screenshot, AI-Assisted permissions.
- Gate tests per subsystem negatives (stale revision, 255-flap gating,
  spoofed sender, classifier timeout).

## Progress log

- 2026-09-12: plan created; decisions locked (tui-realm, no-memory, 3 harnesses).
  Rust 1.98.1 found via rustup (login-shell PATH only). Nothing implemented yet.
- 2026-09-12: Phase 0 DONE via manual rustc+cc pipeline (3/3 CLI tests green).
  `cargo` unusable in-sandbox (blocker above); canonical `cargo test` still needs
  a user-side run or sandbox fix.
- 2026-09-12: sandbox restriction lifted; canonical `cargo test` green.
  Phase 1 DONE (35 green). Deps so far: `toml 1`, `ratatui 0.30`, `crossterm 0.29`.
  Phase 2 started: `session` (SessionId/State/Activity) + `event` (Send AppEvent)
  red→green (40 total).
- 2026-09-12: Phase 2 core logic done — `pty` (real-PTY pane, kill-not-hangup
  fix), `session` manager (rebind/revoke/retain), `app` reducer, `input` router
  v1. 60 green (56+4). Deps add: `portable-pty 0.8`, `vt100 0.15` (parser not
  wired yet). Next: vt100 screen + grid render, main loop, tuirealm host.
