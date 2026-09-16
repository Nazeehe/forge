# Vendored vt100 0.15.2 + forge patches

Copied verbatim from crates.io (`vt100 0.15.2`) except for the faint
(SGR 2) support below. Upstream tracks only bold/italic/underline/inverse
and silently drops SGR 2, which renders agent ghost/prediction text
full-bright instead of faint. No newer upstream release adds it, so the
delta lives here until one does.

## Patch inventory (marked `Forge patch` in source)

- `src/attrs.rs`: `TEXT_MODE_DIM` bit (0x10) with `dim()` / `set_dim()`,
  plus a dim arm in `write_escape_code_diff`.
- `src/term.rs`: `Attrs::dim` builder bit; emits SGR 2 on, SGR 22 off
  (22 resets bold intensity too, per ECMA-48, matching the bold-off arm).
- `src/cell.rs`: `Cell::dim()` passthrough.
- `src/screen.rs`: SGR `2` sets faint; SGR `22` now clears bold AND
  faint (upstream cleared bold only).
- `src/screen.rs`: `set_scrollback` widened from `pub(crate)` to `pub`
  so forge can move the scrollback viewport for wheel scrolling on
  panes whose app never enables mouse reporting.
- `src/parser.rs`: added `screen_mut()` (upstream exposes only `screen()`)
  for the same viewport driving.
- `src/grid.rs`: `visible_rows` now windows with saturating math. The
  forge viewport clamp keys the offset to buffered history, which can
  exceed the screen height, and upstream's plain subtractions
  underflowed there (`attempt to subtract with overflow` when wheeling
  deep into a long scrollback).
- `src/grid.rs`: `scroll_up` preserves lines scrolled off the top of
  top-anchored regions (`scroll_top == 0`) into scrollback. Upstream
  discards all region scrolls, matching xterm — but a region-driven
  app like codex then keeps its transcript history only in its own
  memory and forge's wheel viewport can never reveal it.
  Bottom-anchored regions (composer/status churn) still discard, the
  alternate grid (`scrollback_len 0`) never feeds, and the now-unused
  `scroll_region_active` helper is removed. Covered by
  `tests/scrollback_viewport.rs` (`top_anchored_region_scroll_feeds_
  scrollback`, `bottom_anchored_region_scroll_still_discards`).
- `src/screen.rs`: added `visible_rows()` passthrough to the grid's
  windowed iterator. Forge's row cache walks rows sequentially with
  O(1) cells; the only public path before was `cell(r, c)`, which pays
  an O(n) `visible_row(n)` walk per row per frame. Same window either
  way. Covered by forge's `styled_rows_*` suite (positions, colors,
  and the wheel-viewport test pin the mapping).

## Refreshing

To re-vendor a newer upstream: copy the new `src/` + `Cargo.toml` over
this directory, re-apply the items above, and run
`cargo test styled_rows_carry_dim` to prove faint survives the parser
plus the `scrollback_viewport` suite for the scrollback behavior.
