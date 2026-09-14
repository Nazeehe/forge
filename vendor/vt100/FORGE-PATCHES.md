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

## Refreshing

To re-vendor a newer upstream: copy the new `src/` + `Cargo.toml` over
this directory, re-apply the four items above, and run
`cargo test styled_rows_carry_dim` to prove faint survives the parser.
