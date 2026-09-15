## Goal

Let a user say "visualize the flow of this code" to an agent session and
get a rendered diagram in forge's Visual tab: the agent emits Mermaid,
calls a new `visual_show` MCP tool, forge rasterizes it in-process and
paints it through the Kitty graphics protocol (Ghostty first), with a
readable fallback elsewhere.

## Success Criteria

- An agent in a session can call
  `visual_show(content, format="mermaid", title?, alt?)` and the
  diagram appears in that session's Visual overlay tab.
- Flowchart, sequence, class, and state diagrams render correctly;
  unsupported formats fail with a structured error naming the
  supported set — never a blank pane.
- Rapid re-renders never show a stale diagram; quitting/hiding the
  tab leaves no orphan images in the terminal.
- `cargo test` (whole suite, unmodified) stays green; no existing
  tool changes behavior.

## Context And Current Facts

- Blueprint (`error_logs.md:299-305`, `Visual pane`) specifies
  `visual_show(path,title?,alt?)` or
  `(content,format=mermaid|html,title?,alt?)`: background decode to
  RGBA8, generation numbers so stale results never win, main-thread
  image budgeting with LRU eviction of inactive visuals, sticky
  visuals, Kitty/iTerm2/Sixel/half-block rendering with explicit OSC
  cleanup, and manual testing on real terminals. Tool table
  (`error_logs.md:406-410`, #47) confirms the shape; `tools.md:66`
  shows #47 at 0/11 done — nothing is implemented.
- Current MCP surface is 16 tools (`src/mcp.rs:400-475`, ending at
  `walkthrough_update`); zero visual/raster/mermaid references exist
  anywhere in `src/` or `Cargo.toml`.
- Execution path for session agents: harness CLI → `forge mcp-serve`
  child → IPC record → `AppEvent::CommsRequest` → `app.rs:1215`
  (`walkthrough_tool` → `session_tool` → `broker.call`) with the
  verdict sent straight back over the reply channel
  (`src/app.rs:1215-1237`). Overlay/session-owned state is handled
  in the `session_tool` layer, which is where `visual_show`
  belongs.
- `Visual` is a tab slot only today (`OVERLAY_TABS` in
  `src/app.rs:65`); no image code backs it.
- `mermaid-rs-renderer` is MIT, pure Rust, no browser/Node
  ([repo](https://github.com/1jehuang/mermaid-rs-renderer):
  "A fast native Rust Mermaid diagram renderer. No browser
  required"). The registry shows 7 versions up to **0.3.1** with 23
  diagram types and `default = ["cli", "png"]` feature flags
  ([API](https://crates.io/api/v1/crates/mermaid-rs-renderer)).
  The render API (`render_svg` → SVG string, `write_output_png` →
  rasterized file) was verified on the
  [0.1.0 render module](https://docs.rs/mermaid-rs-renderer/0.1.0/mermaid_rs_renderer/render/)
  ([render_svg](https://docs.rs/mermaid-rs-renderer/0.1.0/mermaid_rs_renderer/render/fn.render_svg.html),
  [write_output_png](https://docs.rs/mermaid-rs-renderer/0.1.0/mermaid_rs_renderer/render/fn.write_output_png.html));
  the spike re-confirms it on 0.3.1 before any wiring.
- Kitty display semantics come from the authoritative
  [graphics protocol spec](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
  (transmit/display/delete commands, chunked base64 payloads).

## Constraints And Non-goals

- No approval-bypass or Chromium/WebView anywhere: the blueprint's
  macOS `visual-raster` child and static-HTML rendering are
  explicitly out of scope — this plan replaces them with the Rust
  rasterizer cross-platform.
- Additive only: the existing 16 tools, checkpoint format, and
  `agents.json` schema do not change.
- Deferred (not rejected): PNG/JPEG `path` input, zoom 100–800% /
  pan / semantic targets, `~/.forge/visuals/` persistence,
  iTerm2/Sixel backends, whiteboard integration. Each is noted at
  the work unit that would naturally grow it.

## Key Decisions

- **Crate 0.3.1, not the pinned 0.1.0**: 23 diagram types vs 4, and
  feature flags let us take `png` without `cli` (forge hand-rolls
  argv; clap stays out of the tree). MIT license is compatible.
- **`format="mermaid"` content only in MVP**: satisfies the stated
  UX ("visualize the flow of this code" always produces Mermaid).
  `path` input needs an image-decode crate plus MIME sniffing;
  `html` needs a browser engine — both phase 2+, with clear
  `unsupported format` errors meanwhile.
- **Rasterize on a background worker**: blueprint mandates
  background decode plus generation numbers. Mermaid layout is cheap
  but rasterizing large diagrams can exceed a frame budget; the
  worker posts `(generation, RGBA8, dims)` and the main thread keeps
  only the newest generation per session.
- **Kitty primary, half-block fallback**: blueprint's backend list
  is Kitty/iTerm2/Sixel/half-block. MVP ships Kitty (Ghostty is the
  eyeball target) plus a dependency-free half-block downscale so the
  tab is never blank on unsupported terminals. iTerm2/Sixel join
  only on demand.
- **Fit-to-overlay, no zoom/pan in MVP**: fixed fit keeps the first
  version shippable; the blueprint's zoom/pan/targets are the
  natural follow-up unit.
- **Cargo feature `visual`, default on**: bounds the rendering
  stack's build-time and binary-size cost behind
  `--no-default-features` while keeping the normal build whole.
- **Budget before allocation**: blueprint caps honored — input
  chars, raster dimensions, and decoded bytes checked before any
  buffer exists; one global LRU over decoded visuals evicting
  inactive sessions first; visuals sticky (no TTL).

## Recommended Approach

New `visual` module owns the pipeline in four stages, each testable
without a terminal except the last: (1) validate + background-raster
Mermaid to RGBA8 with generation stamps; (2) AppState slot per
session plus global LRU budget; (3) Kitty transmit/display/delete
framing around ratatui draws with explicit cleanup on tab-hide,
session-end, and quit; (4) half-block fallback renderer. Tool
plumbing (`ToolDef` + `session_tool` arm + single-line JSON
verdicts carrying published dimensions or structured errors) lands
before display so agents get useful failures from day one.

## Work Plan

- **U1 — Spike (no wiring).** Add the crate (`default-features =
  false, features = ["png"]`, behind `visual`), render one
  hardcoded flowchart to PNG bytes in a unit test, assert non-empty
  output with sane dimensions. Proves the 0.3.1 API is intact,
  measures clean-build time impact, flushes out system-dependency
  surprises. Commit alone.
- **U2 — Tool plumbing.** `visual_show` ToolDef (content, format,
  title?, alt?); `session_tool` arm validating format/caps and
  returning `unsupported format` errors; generation counter;
  verdict carries dimensions or error. Unit tests on verdicts,
  caps-before-allocation, unknown-format errors. Commit alone.
- **U3 — Raster worker + state.** Background worker
  Mermaid→RGBA8; per-session sticky slot; global LRU budget with
  inactive-first eviction; stale generations dropped. Unit tests
  with synthetic frames (no terminal). Commit alone.
- **U4 — Kitty display + fallback.** Transmit/display/delete
  framing sized to the Visual overlay rect; delete on hide/quit;
  half-block downscale fallback; `alt` text always rendered for
  screen-reader/keyboard parity per `ui_guidance.md`. Unit tests on
  framing math and cleanup paths; manual eyeball in Ghostty (the
  blueprint's "test real terminals" rule). Commit alone.
- **U5 — End-to-end acceptance.** Agent in a session runs
  "visualize the flow of this code": Mermaid → tool → visible
  diagram; rapid double-render shows only the newest; quit leaves
  no orphan image. Manual script plus a committed regression test
  for the tool round-trip. Commit alone.

## Validation Plan

- U1: `cargo test visual_spike` — PNG bytes, dimensions sane;
  `cargo build --timed` (or wall clock) records the dependency
  cost for the plan record.
- U2–U3: focused `cargo test visual` suites, then whole-suite
  `cargo test` unmodified per repo rule.
- U4: framing/cleanup unit tests green; manual Ghostty eyeball
  (diagram correct, no orphans after tab switch + quit) observed,
  not inferred.
- U5: scripted agent session producing a diagram from real code;
  stale-render race exercised by double-fire. Highest-risk step is
  the U4 eyeball — Kitty placement around ratatui draws is the one
  thing unit tests cannot prove.

## Risks / Rollback

- Crate API churn (0.x, single owner, 4% documented): contained by
  the U1 spike; rollback is dropping the feature flag and reverting
  to no-Visual (tab slot stays, renders its current empty state).
- Diagram text depends on system fonts: if labels render as boxes
  on a minimal system, font availability is the first suspect. MVP
  surfaces raster errors plainly; bundling a fallback font is the
  deferred fix (binary-size tradeoff).
- Terminal matrix: Kitty-first means other terminals get the
  coarse half-block view until a backend is added per demand.
- Large-diagram memory: mitigated by pre-allocation caps and LRU;
  pathological inputs fail closed before allocating.

## Open Questions

None. Version, scope cuts, fallback chain, and flag defaults above
are recommendations for approval; alternatives and deferrals are
recorded inline.

## Sources

- https://crates.io/api/v1/crates/mermaid-rs-renderer
- https://docs.rs/mermaid-rs-renderer/0.1.0/mermaid_rs_renderer/render/
- https://docs.rs/mermaid-rs-renderer/0.1.0/mermaid_rs_renderer/render/fn.render_svg.html
- https://docs.rs/mermaid-rs-renderer/0.1.0/mermaid_rs_renderer/render/fn.write_output_png.html
- https://github.com/1jehuang/mermaid-rs-renderer
- https://sw.kovidgoyal.net/kitty/graphics-protocol/
