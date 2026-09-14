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
  `confine` (Phase 7 file transfer), `safe_text` display fns (Phase 3 permission UI),
  `CellFormat::plain` (test + future-chrome use).

## Phase 2 — Terminal core, first pixels — STATUS: DONE, manually testable (81 green at the time; now 300+; see progress log)

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
- [x] Pane-content color — LANDED 2026-09-14: styled-row view model +
      SGR→theme absolute-role map (hue carried through child output, readable
      against measured bg); capable `TERM` advertised to children.
- [ ] `tuirealm` chrome host (buttons/mouse) — SUPERSEDED (deliberate,
      2026-09-14): chrome is manual render + hit-test (session bar buttons,
      opaque modals, restore picker); tui-realm stays for dialog form inputs
      only. No component-tree migration planned.
- Still open: flood-cannot-starve-input gate test.

## Phase 2.6 — Session layout model (80/20 + session bar) — STATUS: DONE (131 green: 126 unit + 5 integration)

Correction: sessions must not tile the screen. One focused session fills
the 80% main pane (20% sidebar stays); a bottom session bar holds one
button per session, switched by click or `Ctrl-b` + number. Sidebar shows
sessions + pending approvals + mode (never blank). Only the active pane is
fitted; background panes refit on focus. Session bar is manual render +
hit-test (tuirealm stays reserved for the permission modal).

- [x] `chrome_areas` (main/sidebar/session-bar/status) + button layout +
      hit-test + sidebar content builder.
- [x] Render single focused session + chrome; digits in prefix mode;
      click-to-switch; fit-active on spawn/switch/resize. The old tiling
      grid is gone (deliberate model change).
- Gate: live two sessions — `Ctrl-b 1` focuses, session-bar click switches
  with refit, sidebar shows focus/pending/mode.

Goal: full shell + full-screen app support (lazygit, htop, vim open/type/quit).
Spec sources: VT100 User Guide on vt100.net (DEC core) + xterm ctlseqs on
invisible-island.net (SGR-256/RGB, key modify-params, mouse encodings),
pulled per-slice as needed.

- [x] Cursor: position + show/hide on the focused pane (`pty::cursor`,
      `PaneView.cursor`, clamped `cursor_screen_pos`; outer cursor hidden
      when the pane hides its). Tests: live-PTY move/hide/reshow +
      TestBackend placement.
- [x] SGR color + attrs → styled spans via per-cell walk (16/256/RGB
      pass-through; theme stays chrome-only). `PaneView.body` is now
      `lines: Vec<Vec<SpanView>>`; the old newline-flattening encoder is
      gone (rows are structural). Tests: live-PTY SGR + mapping table +
      TestBackend cell-fg assert; live smoke shows real SGR bytes.
- [x] Full key encoding: arrows (normal vs application-cursor SS3), nav keys,
      F-keys, Ctrl/Alt/Shift `1;Nm` chords; honor the pane's
      application_cursor mode (keypad deferred: crossterm reports no
      numpad-distinct events). Live: Down scrolls `less`, Ctrl-C kills.
      Note: lazygit Files panel is a tree — single visible file means
      Up/Down only moves within tree rows, not between diffs.
- [x] Mouse: outer capture enabled; events inside the active pane encoded
      per its requested mode/encoding (X10/SGR/UTF-8, motion gating);
      chrome keeps the rest. Live: click bytes reach the child.
- [x] Bracketed paste wrap when the pane requests it (live: markers in file).
- [x] Alt-screen audit (isolated grids pinned by test; lazygit renders) +
      capable `TERM` advertised when inherited is missing/dumb/unknown
      (live: dumb parent → xterm-256color child).
- Gate: lazygit/htop/vim live usable; key-table unit tests per chord class.

## Phase 3 — Hooks + permissions — STATUS: DONE (gate tests listed under 3a–3c landed; see progress log)

- [x] 3a `hook-relay`: fail-open relay (stdin JSON → route → one newline
      record → optional ~3 s decision; always exit 0 silent). No serde:
      field scan + CR/LF strip are exact on valid JSON. Tests: route table,
      envelope, no-listener/empty/unreachable/silent-listener/timeout,
      decision relay, CLI fail-open gate.
- [x] 3b TUI listener: `0600` socket + loopback TCP, 32-conn cap, no-prefetch
      first line, `HookRequest` fan-in with bounded pending queue. Live:
      socket 0600, async instant, sync waits out the relay timeout
      fail-open, socket file removed on quit.
- [x] 3c Policy/cache/audit (`audit.log` 0600, block-wins); modes Off,
      Safe-Only, YOLO (AI-Assisted asks; classifier deferred to Phase 8).
      Shell cache keys stay exact, others normalize; Ask never cached;
      audit repairs 0600 drift and escapes hostile fields losslessly. Live:
      Safe-Only denies `rm -rf` and allows Read instantly with audit lines.
- [x] 3d `tui-realm` permission modal (keyboard nav + mouse click).
  Sync hooks open a centered modal (Allow once/Deny); all keys, mouse,
  and paste are captured while open; deny caches, allow stays allow-once.
  Live: modal renders, Down+Enter denies with audit line, repeat auto-denies.
- Gate tests: relay-never-blocks; block-wins; audit-0600-enforced;
  hostile-display-safe.
- (Stale duplicate checklist removed 2026-09-14: repeated 3a–3d, all landed.
  Off-phase additions: opaque restyled modals, group modal with auto-focus,
  300 ms staged Enter injection, wheel-scroll on alt-screen, SCM lazygit tab,
  Omarchy live theme follow — see progress log.)

## Phase 4 — Comms core — STATUS: DONE

- [x] 4a `mcp-serve` (newline JSON-RPC 2.0, MCP 2025-03-26; hand-rolled
      parser, no new deps) with stdio serving and IPC bridge.
- [x] 4b Broker: run-ID auth + group ACL + pressure cap 5 (queued +
      delivered-awaiting-response, counted once); conversations with UUIDs.
- [x] 4c Tools `ask/send_response/tell/ack/list_sessions`; exit-fails-
      conversation; idle/debounce injection into panes; pane env
      (`FORGE_RUN_ID/SESSION_NAME/SESSION_CWD`); `Ctrl-b g` peers toggle.
      Dynamic project/memory init context deferred to Phase 6; per-session
      route file only matters for remote (Phase 8).
- Gate tests: no-group-no-transfer; forged/stale-run-ID rejected;
  pressure-cap; target-exit-determinism. Manual: two live sessions
  ask/tell/ack plus live no-group refusal — all passed.

## Phase 5 — Agent CLI sessions (pulled forward) — STATUS: MOSTLY DONE (5a/5c/5d landed 2026-09-13/14; 5b partial)

- [x] 5a `cli_tool` registry: `claude`/`codex`/`muse` (binary, env
      override, model flag, launch flags per blueprint table; muse binary
      is `muse`, not `metacode`); binary-missing errors.
- [ ] 5b Installers: muse hooks + MCP entries DONE 2026-09-13 (user `hooks`
      block in ~/.config/muse/settings.json, 5 events verified live on
      1.2.1; scrubbed hook env bridged via ~/.forge/endpoint.json +
      harness-ID attribution; `mcp_servers.forge` stdio entry, ${VAR} env
      expansion verified live). Still open: claude settings.json, codex
      `hooks.json` (best-effort, unverified without API auth),
      `install-skills/uninstall-skills` (+codex/gemini/metamate variants),
      startup repair of registrations + bundled-skills refresh (`.new`
      preserves edits).
- [x] 5c Create-session UI — LANDED, trimmed per user: harness picker,
      session name, folder path (no internet/model/local fields);
      keyboard-first opaque dialog; new session auto-focuses on create.
- [x] 5d Launch + triple-tab sessions: harness tab + lazy terminal tab +
      lazy SCM tab (lazygit, created on first switch); tab switch keys;
      `FORGE_CLI_TOOL` env; hook→session attribution via caller run ID in
      the relay record + activity transitions; staged injection (300 ms
      before Enter).
- Gate tests: tab switch + lazy spawn + SCM fit landed
  (`new_session_fits_current_grid`, `topbar_click_fits_lazy_scm_pane_to_main`,
  `staged_enter_beat_is_300ms`, dialog validation). Still open: registry
  matrix (flags/env/missing), installer round-trips on scratch homes,
  attribution test. Manual: launch codex + muse + claude, cross-talk both
  directions via injection, MCP registered per CLI.

## Phase 6 — Projects, tasks, walkthrough, timers, recovery — STATUS: TODO

- [ ] Projects registry + cwd matching; project tools 48-54
      (`project_create/get/list/update/delete`, `project_path_add/remove`);
      `tasks.db`/`projects.db` via `rusqlite` bundled (open decision:
      blueprint toolchain names bundled rusqlite; no SQLite dep yet).
- [ ] Checklists/tasks (10 tools 23-32); walkthrough (5 tools 33-37) with
      `<walkthrough-question>` injection; timers (`schedule_prompt`/
      `cancel_scheduled_prompt` + once/interval/daily).
- [ ] TUI surface for tasks/walkthrough: views + prefix keys (blueprint
      views 4-5; key letters locked with tests like the dispatcher map);
      theme switching UI: SUPERSEDED in part — Omarchy live auto-follow
      shipped 2026-09-14 (no restart); manual cycle key + config persist
      still an open user decision.
- [ ] Persistence: PARTIAL off-phase — `sessions/*.json` checkpoints +
      crash recovery + startup triage + restore picker + graceful
      SIGTERM-on-quit landed 2026-09-14. Still open:
      `session_history.json`, ~2s coalesced capacity-one worker,
      signature-gating, Save & Quit. (`forge.log` already implemented —
      see backlog.)
- Gate tests: migration/CRUD/reorder/focus invariants; checkpoint round-trip;
  corrupt-save quarantine; live-checkpoint refusal.

## Phase 7 — Full local tool surface — STATUS: TODO

- [ ] `terminal_exec/read/send` (Unix, one lease/session, supervisor reap).
- [ ] `compact/start_session/status/message_user`, file offer/accept
      (1MiB/file, 4 pending, 4MiB staged, 5min TTL, 0600 no-overwrite).
- [ ] `start_session` (local harness sessions from the registry) +
      session records gain internet/native-CLI/harness/VCS/remote metadata.
- [ ] `screenshot` tool (macOS-gated) + `visual-raster` subcommand.
- [ ] `visual_show` (decode/budget/generation ordering; raster stubbed).
- Gate tests: supervisor no-zombie/CTRL-C-race/UTF-8-truncation; file
  digest/expiry/symlink-swap negatives; tool-count check calibrated to
  50 on Unix (57 blueprint − 7 skipped memory tools); flood-cannot-
  starve-input gate (open since Phase 2).

## Phase 8 — Deferred: teams, VCS, whiteboard-bk, remote, AI — STATUS: TODO

- [ ] 7a: ephemeral team builder (+ `forge-team-mode` skill), VCS: Git
      lazygit pane LANDED 2026-09-14 (lazy SCM tab, fitted; fallback text
      when lazygit missing; Sapling smartlog still open), whiteboard backend
      + limits (9 tools 38-46: start/list/open/add/update/delete/highlight/
      answer/end), Claudling.
- [ ] 8b: SSH/OD remote + reverse-forwarding, mobile (GChat/Telegram),
      macOS raster/screenshot, AI-Assisted permissions.
- Gate tests per subsystem negatives (stale revision, 255-flap gating,
  spoofed sender, classifier timeout).

## Backlog — Deferred / needs-decision (from 2026-09-13 audit)

- [ ] `clikan` Go module + `install-clikan-mcp` + kanban view: confirmed
      for a later build (post-8), forge-first until then.
- Dropped 2026-09-13: selection mode/copy; telemetry reporting (local
  `forge.log` logging stays and is already implemented).
- [ ] Prefix-hold (~500ms) shortcuts overlay; manual viewer; away state
      (pairs with 8b mobile).
- [ ] `message_user` remote forwarding rides on 8b transports.

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
- 2026-09-13/14: terminal fidelity + session UX (unplanned, off-phase).
  `vt100` vendored to `vendor/vt100` (upstream 0.15.2 + forge patches: faint
  SGR 2 so agent ghost/prediction text renders dim not full-bright, public
  `set_scrollback`/`screen_mut` for viewport driving — see
  `vendor/vt100/FORGE-PATCHES.md`). Group modal (`Ctrl-b g`): typed group
  names (`n`), checkbox session picker (`a`), up/down selects member rows so
  `r` removes from group. New sessions auto-focus on create. Broker
  injections wait 300 ms before the Enter key sequence.
- 2026-09-14: mouse scrolling for all sessions (terminal + harness).
  Mouse-off normal screen moves the scrollback viewport (3 lines/notch,
  output/input snap back to live); mouse-on apps keep SGR passthrough;
  alt-screen mouse-off sends Up/Down arrows (DECCKM-aware), which is the
  only affordance for codex/claude — verified codex 0.154.0 never enables
  mouse reporting (no EnableMouseCapture strings; init emits only
  focus/bracketed-paste/sync-output/kitty/DSR/DA queries, no `?1000h`).
  Fixed `Grid::visible_rows` subtract-overflow crash on offsets deeper than
  the screen (saturating window; regression tests reproduce the exact
  `grid.rs:125:42` panic on unfixed code). Test gotcha: cooked-pty children
  echo ESC back as `^[` (ECHOCTL), so wheel tests drive `stty raw -echo`
  children like real fullscreen apps. Suite now 284 unit + 7 integration,
  all green; `cargo fmt` not enforced (whole repo unformatted, Makefile
  gates are build+test only).
- Queued (user-requested, not started): graceful SIGTERM of agents on quit
  so they can save state. Uncommitted work sits in the tree (scroll feature
  + crash/arrows fixes); commit/push on user request.
- 2026-09-14: dialog restyle per `ui_guidance.md` + status bar removed.
  New Session / Communication Groups (+ name prompt, member picker) are
  opaque now (`Clear` + `BorderModal` cyan border; the yellow flood was the
  yellow `Block` style painting the whole rect). Focus is `>` + reverse
  cyan (`theme::focus_row`, additive — chrome `Role::Focus` untouched);
  selection is `(o)`/`[√]` in accent yellow, default `[Create*]`; values
  no longer button-styled; errors carry `! `. Rows render by hand from
  component state (stdlib views bypassed). Status bar gone: session bar
  owns the last row, `status_text`/`Chrome.status` deleted, main grows one
  row. Suite 288 unit + 7 integration green (new render-style tests assert
  cell fg/modifier per mark; `▸` replaced by `>`).
- 2026-09-14: create form trimmed to Directory/Name/CLI Tool/Comm Group
  (internet/model/connection rows deleted; spec hardcodes model "" so the
  CLI default applies; dialog shrunk to 78x12). Topbar tabs use emoji
  icons (🤖💻🔔📝📷🔀 — single codepoints, default emoji presentation, no
  VS16; layout already measures display width, locked by a wide-char hit
  test). Suite stays 288 + 7 green.
- 2026-09-14: Omarchy theme support, live without restart. `theme.rs`
  reads `current/theme/colors.toml` (`mode`, else background luminance)
  but only overrides absolute roles (Text/KeyDesc/modal fill — hue roles
  already track the terminal palette, which Omarchy redefines per theme).
  Dark setups are pixel-identical to builtin; light setups get black
  text + opaque light modal fill. A `ThemeWatcher` owned by the main
  loop polls `theme.name` per tick and repaints on change (torn reads
  retry next tick; missing state polls false forever). Thread-local
  ambient map keeps parallel tests race-free (`hold_delta` guard
  restores on drop). Verified against the live aqua-glass file. Suite
  296 + 7 green.
- 2026-09-14: SCM sizing fix. Lazygit rendered cornered at 80x24 because
  the topbar click handler only refit tabs 0-1 after select; the lazily
  spawned SCM pane never took the main area. Refit now runs for every
  live pane tab (`!overlay_active`), proven by a click test asserting
  (36,133) that fails with (24,80) on the old condition. Suite 298+7.
- 2026-09-14: SCM tab loads lazygit. New `TabKind::Scm`: agent sessions
  are born with [Agent, Terminal, Scm], the SCM pane spawning lazily on
  first view in the session cwd (same panelless pattern as the terminal
  tab; lazy spawn shared via `spawn_lazy_tab`, missing binary renders as
  an exited child). Strip is Agent/Terminal/SCM + Events/Tasks/Visual
  overlays. Proven end-to-end: lazygit 0.65 renders its full TUI through
  a forge pane in ~1s (needs /dev/tty, which portable-pty provides; no
  extra query answering required). Suite 297 + 7 green.
- 2026-09-14: plan reconciliation (user: "what have we not done yet").
  Removed two stale duplicate checklists (Phase 1 tail, Phase 3 tail);
  Phase 3 DOING→DONE; Phase 5 TODO→MOSTLY DONE (5a/5c/5d checked, 5b
  narrowed to claude/codex installers + skills + startup repair); Phase 2
  color item checked (absolute-role map shipped), chrome-host marked
  SUPERSEDED (manual chrome is deliberate); Phase 6 persistence marked
  PARTIAL, theme-cycle UI half-superseded by Omarchy auto-follow; Phase 8
  7a notes the shipped lazygit pane. Remaining: graceful SIGTERM on quit
  (queued, next), Phase 6 tasks/walkthrough/timers/projects +
  session_history/coalesced-checkpoints/signature-gating/Save & Quit,
  Phase 5b remainder, Phase 7 full tool surface, Phase 8 teams/whiteboard/
  remote/AI, backlog (clikan post-8, prefix-hold overlay, manual viewer,
  away state). Open gates: flood-cannot-starve-input, registry matrix,
  installer round-trips, attribution test.
- 2026-09-14: graceful SIGTERM on quit (queued risk: agents killed
  mid-thought). `PtyPane::terminate` (SIGTERM via new `libc` dep, no
  waitpid — the reader keeps sole reap ownership so exit codes stay
  accurate) + `child_alive` (`kill(pid,0)` probe) + `terminate_gracefully`
  + `SessionManager::shutdown_gracefully` (TERMs all live panes, one
  shared 2 s grace via `SHUTDOWN_GRACE` in `tui::run`; stragglers die by
  the existing drop-SIGKILL and are logged to the app log, not stderr).
  Tests: trap-exit reports the trap's code (SIGTERM proof), TERM-ignorer
  times out then dies by SIGKILL fallback, manager fan-out covers two
  panes. Test lessons: SIGTERM races trap installation (READY handshake
  required), portable-pty maps signal-death to code 1 (not None), and
  `poll_exit`-style per-id drains eat other sessions' Exited events —
  collect multi-session exits in one drain. Suite 301 + 7 green.
