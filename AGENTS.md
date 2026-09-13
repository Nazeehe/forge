# AGENTS.md — Forge Rebuild

Read this before touching any code. It binds every agent and every session.

Always answer questions the user asked first. 

## 1. TDD IS MANDATORY — NO EXCEPTIONS

This project is rebuilt **strict test-driven development, red → green → refactor**:

1. **Write the failing test FIRST.** New behavior, bug fix, or change: start with a
   colocated Rust test (or native Go/TS test where applicable) that fails.
2. **Observe red.** Run it. Watch it fail for the right reason. A test you never
   saw fail proves nothing.
3. **Implement minimally.** Just enough production code to turn the test green.
4. **Refactor while green.** Keep the suite passing.
5. **Never weaken correct code to satisfy a self-authored test.** If your new test
   disagrees with real behavior, your assumption is the bug — fix the test.

Rules:

- No production code without a failing test first. No "quick" untested edits.
- Every phase gate in `.agents/plans/2026-09-12-forge-bringup.md` lists required
  tests. The phase is NOT done until `cargo test` (whole suite, unmodified — never
  `-k 'not …'`, `--deselect`, `#[ignore]` to dodge red) is green.
- A test that fails on code you changed is the requirement. Fix the code, never
  delete or skip the test. Rewriting an existing assertion to fit your change is
  the same violation.
- Keep tests colocated (`#[cfg(test)]` in the module) so behavior and proof live
  together. Throwaway probes go in `/tmp`, never in the repo.
- `error_logs.md` is the read-only blueprint reference. Do not delete or
  overwrite it. It is context; the code is the source of truth.

## 2. Toolchain

- Rust 2021 edition, `forge` 1.0.0. Stable toolchain (1.98.1 verified 2026-09-12).
- `cargo` lives in `~/.cargo/bin` and is **only on PATH in login shells**.
  Prefix every cargo invocation: `bash -lc 'cargo …'` or use the full path.
- No Go / Node work in this bring-up (clikan + whiteboard frontend excluded).

## 3. Architecture invariants (from blueprint, non-negotiable)

- Single-owner state: workers never mutate `AppState` directly; they emit typed
  `AppEvent` through MPSC. Bounded queues, bounded drain per tick.
- Current **run ID** (not session name) authorizes requests.
- No shared communication group → no cross-session control or data transfer.
- No injection while the target is busy or the user is typing.
- All connections, queues, frames, files, pixels, output, retries, timers bounded.
- Important files: atomic writes (temp → fsync → rename → dir fsync), `0600`
  where private. Relative-path jail: reject absolute/traversal/symlink/special/overwrite.
- Hostile text is escaped for display; raw input stays the policy input.
- Absent/slow Forge never blocks a harness hook (fail-open relay, bounded waits).
- Terminal state (raw mode, alt screen, images, cursor) restores on panic/exit.

## 4. UI stack decision (locked)

- `tuirealm 4` + `ratatui 0.30` + `crossterm 0.29`. Pinned in `Cargo.lock`.
- `tui-realm` owns chrome: modals, dialogs, switcher, settings, permission
  prompts — keyboard-first, mouse-clickable.
- The PTY grid and visual pane stay a **raw custom `ratatui` view** fed by
  portable-pty readers. Never force PTY painting through components.
- Semantic theme APIs only (`src/theme.rs`). No inline RGB. Rounded borders,
  never blank panels.

## 5. Scope (locked for this bring-up)

- **BUILD:** Rust `forge` only — TUI, sessions/PTY, hooks/permissions (Off,
  Safe-Only, YOLO first; AI-Assisted later), MCP broker + local tool surface,
  projects, checklists/tasks, walkthrough, timers, persistence/recovery,
  supervised terminal-exec, whiteboard backend stubs, team builder, VCS, Claudling.
- **SKIP entirely:** memory feature (no `memory.db`, no memory MCP tools, no
  memory config section, no embeddings). Do not scaffold it "for later".
- **EXCLUDE:** Go `clikan` tree, React whiteboard frontend, release upload path.
  Keep degrading stubs: `clikan/mod.rs` shim, whiteboard SHA-asset serve,
  macOS raster/screenshot (Linux-first, degrade off-macOS).
- **Harnesses:** ship `claude`, `codex`, `muse` first via an extensible
  `cli_tool` registry (name, binary, env override, flags, hook capabilities).
  Adding a harness MUST be a data record + hook mapping, never a refactor.

## 6. Verification before "done"

- `bash -lc 'cargo build'` and `bash -lc 'cargo test'` green in-session.
  Multi-filter runs need `--`: `cargo test -- session:: event::` (bare extra
  args are rejected by the test harness).
- Phase gate tests from the execution plan all pass, including negative cases
  (forged run ID, missing group, full queue, corrupt save, hostile text).
- Manual smoke from Phase 2 on: launch, 2 sessions, prefix commands, kill one,
  `kill -9` recovery.
