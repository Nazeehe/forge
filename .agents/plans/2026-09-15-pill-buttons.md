## Goal

Render every clickable button in the forge TUI as a rounded pill tab (`label` with Nerd Font half-circle ends) behind the existing `[pills]` config flag: session-bar tabs, topbar tabs, sidebar mode buttons, timer Cancel buttons, and the create-dialog actions. Session-bar pills are ~80% implemented in the working tree; the rest is planned here.

## Success Criteria

- With `[pills] enabled = true` (the default), all five surfaces draw `label` pills: focused/primary containers filled, idle ones dim, destructive (timer Cancel) red.
- With `enabled = false`, every surface renders byte-identical legacy output.
- Mouse clicks on pill caps activate exactly like clicks on the label.
- Full suite green (currently 375 unit + 7 integration, plus new pill tests).
- Work lands as one commit per unit below, pushed to `main` (standing user preference).

## Context And Current Facts

- Pill foundation already in the working tree (uncommitted): `PILL_LEFT/RIGHT` constants (`U+E0B6`/`U+E0B4`), `theme::Role::TabActive` (Black on Yellow, bold) and `Role::TabInactive` (DarkGray), `BarSegment.cap`, `render_pill`, pill-aware `layout_session_bar`, `Chrome.pills`, `AppState.pill_tabs`, `config.pills_enabled` (`[pills] enabled`, default true), tui wiring for render + session-bar clicks.
- Session-bar segments now take a `pills` flag; five ui test call sites still use the old 2-arg form and do not compile as tests.
- `main` plus tree is RED for unrelated reasons: a concurrent bots implementation (`src/bot.rs`, `comms.rs`, `listener.rs`, `event.rs`, `main.rs`, parts of `config.rs`/`app.rs`) breaks `cargo check` (`str_list` arity in `extract_bots`, 5 errors). Peer session `waxen-lacerta` is the likely author; it was notified.
- Button inventory (all verified by search, no other bracket-buttons exist):
  1. Session bar, compact (`session_bar_segments`, `layout_session_bar`, `render_session_bar`, `session_at`) and wide ≥160 variant (`session_bar_segments_for_area`, accent `■` overlay).
  2. Topbar tabs (`layout_topbar`, `render_topbar`, `topbar_at`; rendered via `ChromeButton`).
  3. Sidebar `[Off]`/`[Yolo]` (`mode_button_areas`, `mode_at`, render, tui clicks).
  4. Timer `[Cancel]` (`timer_cancel_rects`, right-align math assuming 8 cells, render, tui clicks).
  5. Create dialog `[Create*]`/`[Cancel]` (custom spans in `create.rs`; keyboard-only, no mouse path).
  6. NOT buttons (excluded): group-dialog `[√]`/`[ ]` checkboxes and key hints, walkthrough overlay (no buttons), restore picker (keyboard list), `group:` headers, empty-state text.
- `ChromeButton` hit-testing is bounds-check-only (`perform(Submit)` always succeeds); pill rendering via `Paragraph` lines needs no component change. `ChromeButton` itself stays.

## Constraints And Non-goals

- distressed tree: do not commit until `cargo check`/`cargo test` are green on the merged tree; never commit the bots author's files as part of pill units.
- Single `[pills]` switch gates everything; no per-surface flags.
- Theme-role styling only (user constraint); static colors stay out except the existing group palette.
- Non-goals: checkbox restyle, walkthrough/picker changes, `ChromeButton` removal, light-theme retune, in-UI pills toggle (config file only), topbar changes in grid mode (grid hides it).

## Key Decisions

- One shared pill language: `render_pill(frame, area, segment, cap)` in `ui.rs` serves all surfaces; caps always 1 cell each, layout reserves +2.
- Session pills: container = group color for grouped tabs; the selected tab always uses the default selected container (`TabActive` yellow) ignoring group color; idle ungrouped = `TabInactive` dim. Text is Black on filled containers. Timer Cancel = destructive variant (red caps + `Danger` label, no fill) so it never reads as "selected".
- Pill text is centered via symmetric 1-space inner padding (`render_pill` adds it; layout reserves +4: 2 caps + 2 pads).
- Pills mode drops the now-redundant group-color furniture: no `group:` headers and no wide-bar `■` swatch overlay (the pill itself carries the color).
- Create dialog keeps its `>` chosen-marker and `*` default-marker inside the pill label; the row's reverse-video focus stays.
- Wide (≥160) pills drop `[...]`/`│` furniture and the accent `■` overlay shifts +1 past the left cap.
- PUA halves are assumed width-1 (matches `unicode-width` and Nerd Fonts); the config flag is the fallback if a terminal disagrees.
- Glyph orientation (`E0B6` left, `E0B4` right) cannot be verified by `TestBackend`; final proof is eyeballing a nerd-font terminal (manual check below).

## Recommended Approach

Finish surfaces in dependency order (shared helper first, then consumers), keeping `pills=false` output byte-identical at every step so each unit is independently committable. Coordinate shared files (`config.rs`, `app.rs`) with the bots author before touching them; if the tree is still red, implement pills units against it but validate with `cargo test` filtered to untouched modules until green.

## Work Plan

- U1 — Session-bar pills (finish in-tree work).
  Surfaces: `ui.rs` segments/layout/render/`Chrome` flag, `app.rs` `pill_tabs`, `tui.rs` wiring, `config.rs` flag.
  Left to do: `pills: false` in the `chrome()` test helper; fix the 5 two-arg `session_bar_segments` test call sites; new tests (pill cap colors incl. grouped, +2 layout widths, cap hit-testing via `session_at`, render asserts for caps/fill/swatch offset, `pills_enabled` default + parse).
  Validate: `cargo test ui:: config::`.
- U2 — Topbar pills.
  Thread caps through `layout_topbar`/`render_topbar` (reuse `render_pill`; `topbar_at` needs no logic change, spans widen). Tests: layout widths, active/inactive pill render, click mapping.
  Validate: `cargo test ui::topbar ui::tab_hit`.
- U3 — Sidebar pills (mode buttons + timer Cancel).
  Widen `mode_button_areas`/`timer_cancel_rects` by caps (Cancel becomes 10 cells; revisit right-align math); render via `render_pill` (Cancel destructive variant); tui click paths reuse the same rects. Update row-position test asserts (`[Off] [Yolo]` lines shift).
  Validate: `cargo test ui::sidebar ui::mode ui::timer tui::permission`.
- U4 — Create-dialog pills.
  Action row spans become pills (`Create*`/`Cancel` labels keep markers); update exact-cell test asserts. No mouse path exists.
  Validate: `cargo test create::`.
- U5 — Docs, suite, publish.
  Document `[pills]` in config docs if a convention exists; full `cargo test`; manual nerd-font eyeball of all five surfaces; commit per unit (U1–U4 + this), push.

## Validation Plan

- Per unit: listed `cargo test` filters green; new tests assert both `pills=true` rendering and `pills=false` byte-parity where legacy asserts exist.
- Highest-risk check: full `cargo test` (375+7 baseline plus ~15 new) — blocked until the bots breakage clears; until then, per-unit filters plus `cargo check` watcher on shared files.
- Manual (cannot automate): run `forge` in a Nerd Font terminal, confirm rounded ends on session tabs, topbar tabs, `[Off]`/`[Yolo]`, timer Cancel, and the create dialog; click every pill including caps; toggle `[pills] enabled=false` and confirm legacy look.

## Risks / Rollback

- Red tree + concurrent author: overlapping `config.rs`/`app.rs` edits may conflict; mitigate by syncing with the bots landing first and keeping pill hunks small and `pill`-named. Rollback per unit = revert its commit(s).
- Glyph orientation/width wrong on some terminal: fallback is the config flag; no code change needed.
- Scope creep into checkboxes/modals: excluded above; any new surface needs a plan amendment.

## Open Questions

None — defaults (pills on, config-file fallback, destructive-red Cancel) are recommendations above; say so if any should differ.
