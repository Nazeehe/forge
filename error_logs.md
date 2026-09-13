# Forge Reconstruction Blueprint

> A source-derived specification for rebuilding this repository from scratch.
>
> Snapshot studied: 2026-09-12. Executable code, manifests, schemas, tests, bundled assets, and build scripts are authoritative. Older prose is context only where it differs from the implementation.

## 1. Purpose and product definition

This is an implementation blueprint rather than a product pitch or a file-by-file paraphrase. It describes the behavior, architecture, protocols, persistence, safety properties, build system, and validation criteria an AI engineering agent needs to reproduce Forge without the original implementation.

Forge is **a terminal control plane for multiple AI coding agents**. It combines:

- `forge`, a Rust terminal multiplexer and orchestration TUI;
- `clikan`, a Go terminal kanban application and independent MCP server;
- an embedded React/Excalidraw collaborative whiteboard;
- MCP tools for inter-agent messaging, tasks, projects, memory, diagrams, controlled terminal work, and human contact;
- harness hooks for activity tracking and permission decisions;
- local, SSH, and OD session launchers for multiple agent CLIs.

The intended experience is “tmux meets Jira for AI pair programming”: run several agents, see what each is doing, constrain who may communicate, inspect task state, and retain knowledge across sessions.

### Shipped executables

| Executable | Language | Responsibility |
|---|---|---|
| `forge` | Rust 2021 | Main TUI, PTY/session manager, hooks, permissions, message broker, MCP, persistence, services, and integrated views. |
| `clikan` | Go | Standalone Bubble Tea kanban TUI plus newline-delimited JSON-RPC/MCP server backed by SQLite. Forge embeds it in a PTY. |

The whiteboard is not a third executable. Its Vite output is embedded in `forge`, served on loopback, and opened in a browser.

### Supported harnesses

| Harness | Default binary | Local mandatory flags | Remote launch command |
|---|---|---|---|
| Claude Code | `claude` | none | `claude --dangerously-skip-permissions` |
| Codex CLI | `codex` | `--yolo` | `codex --yolo` |
| Gemini CLI | `gemini` | `--approval-mode yolo` | same |
| MetaCode | `metacode` | none; create `opencode.json` with allow policy if absent | `metacode` |
| Pi | `pi` | none | `pi` |

Binary overrides are `CLAUDE_BIN`, `CODEX_BIN`, `GEMINI_BIN`, `METAMATE_BIN`, and `PI_BIN`.

### Terminology

- **Session**: one harness PTY, optional human shell PTY, tabs, state, metadata, and a unique live capability.
- **Run ID**: opaque per-launch session identity. It proves ownership of an incoming request; rebinding revokes the old value.
- **Communication group**: access-control group. Sessions may communicate only when they share a group; membership may overlap.
- **Conversation**: broker state for ask/response or tell/ack.
- **Injection**: text delivered into an agent PTY only at a safe idle boundary.
- **Board**: an Excalidraw whiteboard, distinct from a clikan kanban board.
- **Checklist**: Forge's agent execution plan, also distinct from clikan.

## 2. Repository and toolchain

```text
.
├── Cargo.toml                 Rust package `forge`, version 1.0.0
├── Makefile / build.sh        builds, tests, installation
├── scripts/                   release install and registration
├── src/
│   ├── main.rs                command router and startup
│   ├── app.rs                 AppState and main event loop
│   ├── session.rs             sessions and PTYs
│   ├── comms/                 MCP, messaging, files, mobile, terminal exec
│   ├── hooks/                 hook install, relay, listener
│   ├── permission/            policy, cache, audit
│   ├── memory/ tasks/ project/
│   ├── whiteboard/ visual/ remote/ ui/
│   └── timers/ teams/ vcs/ walkthrough/ tamagotchi/ ...
├── clikan/                    Go module
├── assets/whiteboard/         React/TypeScript/Vite frontend
├── assets/skills/             bundled agent skill
├── docs/                      manual and historical/design docs
├── vendor/vt100/              patched terminal-parser fork
└── vendor/renderdag/          vendored commit-graph renderer
```

The primary source trees contain roughly 113,000 lines including tests and vendors. `src/app.rs` is the largest integration point at about 17,500 lines. A clean implementation should preserve its state-machine behavior while extracting cohesive orchestration units.

Toolchains:

- Rust/Cargo, edition 2021. The README says Rust 1.88+.
- Go **1.25.1** per `go.mod`; README's Go 1.21+ statement is stale.
- Node >=24 and npm >=10.
- macOS for window screenshots and the hidden WebKit raster helper; core Rust also builds on Linux, and remote deployment targets Linux x86-64.

Important Rust libraries are ratatui/crossterm, portable-pty, patched local vt100, bundled rusqlite, serde/TOML/JSON, ureq, tiny_http/tungstenite, image/ratatui-image, syntect/two-face/ratskin, and local renderdag. macOS adds tao/wry. Go uses Bubble Tea/Bubbles/Lip Gloss and pure-Go `modernc.org/sqlite`; pure Go is required because releases use `CGO_ENABLED=0`. The frontend uses React 18, Excalidraw 0.17.6, React Markdown/GFM, TypeScript, Vite, and Vitest.

## 3. User-visible behavior

Normal flow is: launch in a project, finish first-run onboarding or recover a checkpoint, create local/SSH/OD sessions, interact directly with the active agent PTY, and use a configurable prefix (default `Ctrl+b`) for Forge commands. Agents expose hook activity, checklists, permission requests, status, visuals, walkthroughs, VCS, and optional Claudling state. Peer coordination arrives through MCP injections. Save & Quit retains topology; a crash leaves a recovery checkpoint.

Normal layout is approximately 80% main pane, 20% sidebar, plus a one-row status bar. Grid mode scales 1×1, 2×1, 2×2, 3×2, then 3×3 for up to nine sessions. There are seven logical tabs:

1. agent harness PTY;
2. Terminal with Human and Agent subviews (Agent is read-only);
3. permission/event log;
4. Tasks;
5. Walkthrough;
6. Visual;
7. VCS.

Prefix commands cover create/navigate/rename/kill session, tabs, grid, groups, permissions, timer, memory, kanban, away state, themes, team builder, terminal selection, session switcher, VCS, manual, and quit. Historical docs disagree on the Tasks/Walkthrough letters; reconstruct them from the input dispatcher and lock the map with tests.

`forge` subcommands are: no command (TUI), `install-hooks`, `uninstall-hooks`, `install-codex`, `uninstall-codex`, `install-gemini`, `uninstall-gemini`, `install-metamate`, `uninstall-metamate`, `hook-relay`, `install-mcp`, `uninstall-mcp`, `install-clikan-mcp`, `install-skills`, `uninstall-skills`, `mcp-serve`, macOS `visual-raster`, help, and version. Startup migrates `~/.ccpp` to `~/.forge`, repairs registrations, refreshes bundled skills, reads configuration/overrides, and enters the TUI. `clikan` launches its TUI, accepts `--board <name>`, and uses `clikan mcp-serve` for MCP.

## 4. Core architecture

### Single-owner state and event fan-in

All mutable application/session/UI state belongs to the main TUI thread. Workers never mutate `AppState` directly; they emit typed `AppEvent` values through MPSC.

```text
input ───────────────────────────────────────────────┐
PTY readers/exits ───────────────────────────────────┤
hook and MCP IPC listeners ─────────────────────────┤
remote/mobile pollers ──────────────────────────────┤
whiteboard HTTP/WS/persistence ─────────────────────┤
VCS, embedding, raster, decode workers ─────────────┤
                                                      ▼
                                                AppEvent queue
                                                      │
                                                      ▼
                                         single-threaded AppState
                                           │        │        │
                                      sessions   services   render
```

The main loop enters raw/alternate-screen mode, mouse capture, bracketed paste, and Kitty keyboard enhancement where supported. A panic hook restores the terminal. On each roughly 16 ms iteration it advances autosave, terminal supervisors, embedding results, timers, visuals, and pet state; drains all ready input; drains at most 100 background events to prevent starvation; tries pending injections; and renders only if dirty. Grid PTY painting is throttled to about 10 fps. All exits restore terminal state.

### Session and PTY model

A session holds ID/name/cwd, `Starting|Running|Exited(code?)`, selected tab, agent and lazy human PTYs, parsed screen/scroll, activity (`Idle|Thinking|ToolUse|Waiting|Stopped`), harness/native CLI ID/internet/run ID, local/OD/SSH state, VCS, pending injections, recent user input, tasks, walkthrough, visual, selection, sticky status, and optional supervised agent terminal.

`SessionManager` owns ordering, active ID, history, run-ID/native-ID maps, spawn/kill/resize/reorder, exited-card retention, lazy terminal/lazygit creation, and cleanup. Exited sessions stay visible until deleted, and their exit fails active conversations.

Each `portable-pty` pane has a reader thread feeding the patched vt100 parser and emitting output/exit events, plus a serialized writer thread. Injection strips an embedded bracketed-paste terminator, wraps the payload, waits about 300 ms, then submits carriage return. The vendored vt100 patches are behavioral dependencies and must be ported if the library is replaced.

Input routing is a state machine. Normal keys go to the PTY except the prefix. Prefix and modal modes consume their own keys. Paste is accumulated into one event. Text cursor operations are Unicode-aware. Holding the prefix for roughly 500 ms shows shortcuts. Selection mode supports character/line/block, Vim motion, word/line/document/page motion, anchor, copy, and ask-agent-about-selection.

## 5. Hooks and permission control

| Harness | Recognized hooks |
|---|---|
| Claude | SessionStart, Stop, PreToolUse, PostToolUse, Notification, UserPromptSubmit, PermissionRequest |
| Codex | SessionStart, Stop, PreToolUse, PostToolUse, UserPromptSubmit |
| Gemini | SessionStart/SessionEnd, BeforeTool/AfterTool, Notification, mapped internally |
| MetaCode | none; OpenCode exposes JS plugin hooks, not shell hook configuration |
| Pi | the same seven categories as Claude |

Only pre-tool and explicit permission events are synchronous when supported. Installed hooks invoke `forge hook-relay`. It reads all stdin JSON, resolves its route, sends one newline record, optionally awaits a decision, and always exits zero silently so unavailable Forge never blocks a harness.

The TUI listens on a `0600` Unix socket such as `/tmp/forge.<pid>.sock` and a dynamic loopback TCP port for reverse forwarding. It reads the first line without buffered prefetch because binary data may follow. That header distinguishes hooks, MCP/comms, and framed visual/file traffic. Cap connection pools at 32 per transport and visual ingress at two. Synchronous responses time out around three seconds; latency above 750 ms is noteworthy.

Children receive `FORGE_IPC_ENDPOINT`, `FORGE_RUN_ID`, `FORGE_SESSION_NAME`, `FORGE_SESSION_CWD`, `FORGE_CLI_TOOL`, `FORGE_PREFIX_KEY`, and `FORGE_VISUAL_RASTER_ENABLED`.

Permission modes:

| Mode | Behavior |
|---|---|
| Off | display/audit and let the harness ask |
| Safe Only | block regex first; allow allow-regex and safe reads; ask otherwise |
| AI Assisted | deterministic rules, then OpenAI/Anthropic-compatible classifier |
| YOLO | allow automatically |

Block always wins. Cache keys normalize case/whitespace, except terminal shell executions, whose exact identity must not be lossy. Manual decisions are allow-once or deny. Audit goes to memory and append-only `~/.forge/audit.log`, with `0600` enforced each write. Untrusted text is escaped for display to expose controls, Unicode confusables, bidi and invisibles; raw input remains the policy/execution input.

## 6. Cross-session messaging

Each harness runs `forge mcp-serve`, a newline-delimited JSON-RPC 2.0 server advertising MCP `2025-03-26`. Initialization includes operating instructions plus dynamic project, memory, and group context. `tools/list` is dynamic: Unix gets terminal tools. Calls become typed messages over the live route. Tool failures generally return MCP `isError`; malformed JSON-RPC uses protocol errors. A remote per-session route file supersedes stale inherited configuration.

Authorization sequence is exact: resolve caller by current run ID; require a live unique binding; resolve an exact live target; require a shared communication group; apply tool-specific local/remote rules. Names route for humans, but run IDs confer authority. Groups are ephemeral ACLs with optional label, color 0–9, overlapping membership, and first membership as primary display group.

Per-target message pressure is capped at five:

```text
undelivered injections + delivered asks awaiting response
```

Delivered tells no longer count.

Ask lifecycle:

```text
ask_session → validate → UUID conversation → queue target injection
            → return ID immediately → inject when target idle/debounced
            → target send_response → queue source injection → complete
```

It never blocks for the human/agent response. Target exit fails the conversation and queues failure to the source.

A new `tell_session` expects `ack_message`; the ack is asynchronously delivered to its source. A tell with an existing conversation ID is an informational follow-up and needs no new ack. Automatic idle nudging and second-idle failure are disabled despite ignored tests and older comments; do not accidentally restore them. One courtesy update reminder may be delivered after acknowledgment and a 30-second grace period if no update was sent.

All queued prompts wait until hook activity is Idle/Stopped and a debounce since human typing has passed.

File transfer is a two-phase offer/accept protocol. Validate source and receiver relative paths, reject absolute/traversal/symlink/directory/special files, cap a file at 1 MiB, pending offers at four, staged bytes at 4 MiB, and TTL at five minutes. Require both live peers and a shared group. Receiver atomically creates a new `0600` file without overwrite, then confirms consume; failures release the claim for retry before expiry.

`message_user` always badges in-app and forwards when configured. Google Chat wins over Telegram. Limit three per session per 60 seconds. Correlate native threads or guarded session prefixes, accept only configured users, and inject replies safely. Google polling uses 20-second normal cadence and exponential 60–900-second failure backoff. Remote commands include `/sessions`, `/help`, `/int`, and `/clear`. Away can be manual or after the configured idle timeout.

`start_session` validates and creates local, same-as-caller, OD, or SSH sessions with directory/preset/host/harness/internet/name/prompt/group. Default children join the caller's group. `compact_session` queues `/compact`; self is allowed, another target requires a group. A session owns one replaceable status of kind info/progress/success/warning/blocked/question, bounded to 80 characters and 320 bytes with no controls or newlines.

## 7. Agent terminal execution

The Unix-only terminal MCP tools are a supervised sandbox-escape fallback, not a default shell. One lease exists per session. Every command receives a PTY/process group, sticky cwd, pager-neutral environment, bounded normalized UTF-8 output, timeout up to 86,400 seconds, and an explicit supervisor.

```text
awaiting_approval → running → terminating → draining → done
        └────────────────────────────────────────────→ aborted
```

Supervisor ordering covers observe, signal, reap, writer close, reader cancellation, and final drain to avoid zombies/hangs. Approval can expire; pager/hidden-input detection reports an incomplete reason. `terminal_read` is cursor-based, defaults to 32 KiB, caps at 256 KiB and may wait at most 5 seconds. It reports phase, final state, exit, output and incomplete reason. `terminal_send` supports Ctrl-C only. Remote terminal execution is rejected.

## 8. Persistence and configuration

### Configuration defaults

Primary configuration is `~/.forge/config.toml`. Code defaults are:

- theme `default`, prefix `ctrl-b`;
- harness commands matching their binary names;
- clikan disabled;
- permission mode Off, empty allow/block patterns;
- AI provider OpenAI, model `gpt-4.1-mini`, threshold 0.6, 10-second timeout, auto-refresh on, key environment `APE_API_KEY`;
- Claudling off and telemetry on;
- experimental memory off. README's `[memory] enabled = true` example is stale;
- embeddings off, model `text-embedding-3-small`, `OPENAI_API_KEY`, 15-second timeout, search maximum 20;
- messaging idle timeout 10 minutes;
- Google Chat off, normal poll 20 seconds, backoff 60–900 seconds, identity `confucius`, CLI `meta`;
- Telegram fields empty;
- OD presets are name/type/directory/harness records.

Configuration writes are mutex-serialized and preserve unknown TOML sections. AI credentials may refresh through `jf`, are cached for about 23 hours, and retry up to three times.

### Persistent locations

| Path | Content |
|---|---|
| `~/.forge/config.toml` | configuration |
| `~/.forge/forge.log` | rotating application log, around 1 MiB |
| `~/.forge/audit.log` | permission audit, `0600` |
| `~/.forge/ape_key.json` | cached AI credential metadata |
| `~/.forge/memory.db` | memories, events, embeddings, observations |
| `~/.forge/tasks.db` | checklists, tasks, notes |
| `~/.forge/projects.db` | projects and labeled paths |
| `~/.forge/claudling.db` | pet state/events/journal/sessions |
| `~/.forge/session_history.json` | recent directories/history |
| `~/.forge/sessions/*.json` | explicit saves and recovery checkpoints |
| `~/.forge/visuals/*.png` | last session visuals |
| `~/.forge/whiteboards/<uuid>.json` | whiteboards |
| `~/.forge/whiteboards/_archive`, `_corrupt` | archived/quarantined boards |
| `~/.clikan/clikan.db` | independent kanban data |

Saved state includes launch directory/time, active session, session records, groups, and permission mode. Each session records name/cwd/native CLI ID/internet/harness/remote metadata/visual. Save & Quit writes explicitly. A recovery checkpoint runs about every two seconds only when a topology/config signature changes, through a capacity-one coalescing worker. Writes use temp file, fsync, rename, and directory fsync. Clean exit removes its checkpoint; crash leaves it. Do not offer checkpoints whose owner PID is still alive.

Memory tables cover memories, tags, links/supersession, events, access logs, embeddings, observations, and FTS. Task tables are checklists/tasks/task_notes and migrate task data accidentally stored in old memory databases. Project tables use cascading project/project-path relationships. Claudling stores pets/events/journals/sessions.

Clikan enables WAL and stores:

- `boards(id, name UNIQUE, created_at)`;
- `settings(key PRIMARY KEY, value)`;
- `columns(id, board_id, name, position, wip_limit, created_at, UNIQUE(board_id,name))`;
- `cards(id TEXT PRIMARY KEY, board_id, column_name, title, description, assignee, tags JSON, priority, progress, due_date, timestamps, position)`;
- indexes by card column/position/board and column board.

It transactionally migrates its old single-board schema. New boards have Backlog, Todo, Doing (WIP 3), and Done.

## 9. Integrated subsystems

### Task checklists

One workspace checklist is active; a new one archives the previous. Tasks use UUIDs and `todo|in_progress|blocked|done`. Exactly one may be focused, and finishing it advances focus. Completing a checklist records a summary and archives it. Notes preserve agent/user authorship. Keep checklists distinct from shared clikan cards.

### Persistent memory

Kinds: Fact, Lesson, Decision, Warning, Preference, Summary, Event. Scopes: Global, Project, Session, Team. States: Active, Archived, Superseded. Retrieval: FTS, vector, hybrid, AI. Results support index/summary/full progressive detail.

Ranking weights are approximately scope .25, FTS .20, vector .20, confidence .10, importance .10, recency .05, utility .05, pinned .05, minus stale/unreviewed penalties. Recency half-life is seven days and stale age about 30 days. Context budget defaults around 3,600 characters and merges relevant global context into scoped requests.

Optional OpenAI-compatible embeddings run in a worker. Queries use a read-only worker so SQLite cannot stall the UI. MCP initialize adds matching project and team memory. Hooks observe edits/writes/side-effect shell operations; Stop may prompt consolidation. Team-scoped saves broadcast to peers.

### Projects and walkthroughs

A project has unique name, display name, description, optional root, and labeled paths. Match a cwd beneath root/path or a configured path beneath cwd. Matching context is injected during MCP initialization.

A session walkthrough contains title, loaded files, ordered line-range/Markdown steps, and pending Q&A. Read locally or via remote SSH `cat`. The user navigates steps; a question is injected in `<walkthrough-question>` markup and the agent answers through MCP.

### Whiteboard

This is a collaborative browser Excalidraw surface. Main-thread `BoardState` owns UUID, secret token, creator, revision, elements JSON, chat, highlights, and completed state.

The ephemeral loopback server exposes unauthenticated shell `/b/<id>`, embedded static assets, bearer-authenticated `/api/board/<id>`, and WebSocket `/ws` with exact loopback Origin validation. Browser token is in the URL fragment. Subscribe before initial state. Subscriber queues are bounded to 256; stale clients expire after about 30 seconds. Rate limit burst 30, sustained 10.

Persistent mutations carry a base revision: first valid writer wins; stale writers resync. MCP update is all-or-nothing and rejects unknown IDs; browser update skips unknown IDs to tolerate delete races. Add/update/delete advance revision and persist. Highlights are transient and revision-free.

Chat projects selected element data into an agent injection. Answers append and clear pending state. Semantic summaries classify structural/substantive/cosmetic changes with about one-second debounce and five-second rate cap. Persistence is temp/fsync/rename/fsync and corrupt JSON is quarantined. Production assets are embedded/extracted under content SHA; `whiteboard_live_assets` reads development `dist/` and is not for releases.

Frontend files divide responsibilities: `App.tsx` lifecycle/layout, `ws.ts` authenticated reconnect/protocol, `normalize.ts` element sanitation/defaults, `sceneDiff.ts` patches, `ChatSidebar.tsx` Markdown chat, `HighlightOverlay.tsx` transient focus, and `sidebarWidth.ts` responsive sizing.

### Visual pane

`visual_show` accepts caller-local PNG/JPEG or, with macOS raster capability, Mermaid/static HTML. Detect MIME by bytes. Decode in background to RGBA8 and use generation numbers so stale results never win. A JSON line header plus exact binary body carries visual/file frames; never prefetch across the boundary. Enforce limits before allocation.

The main thread budgets images and evicts inactive LRU first. Visuals are sticky; legacy TTL input is accepted but ignored. Zoom is 100–800%, with pan and semantic targets in 0–10,000 coordinates. Cap targets at 256, IDs at 512 bytes, labels at 2,048 bytes.

Render through Kitty/iTerm2/Sixel/half-block fallback and explicitly clean images/pointer OSC state. The macOS hidden Wry/WebKit renderer uses bundled offline Mermaid in strict mode and sanitizes static HTML. Test real terminals manually.

### Remote sessions

Local Unix IPC is supplemented with loopback TCP reverse forwarding for SSH/OD. The connection model explicitly tracks OD allocation, host session, and one leased SSH ControlMaster. OD launches via `dev connect --enable-control-master --reverse-port-forward`, detects a host marker, adopts safe existing sockets, and launches mux slaves fail-closed.

Bootstrap detects OS/architecture, fetches the Linux helper through the embedded Everstore `jf` handle with staged SCP fallback, installs/updates it, atomically writes private remote MCP/hooks/route configuration, and starts the harness in a sanitized remote tmux with its prefix/status disabled.

Exit 255 triggers reconnect only after at least 15 seconds of prior health, avoiding bad-host loops. OD adds a 60-second cooldown. Fast failures reopen the launch dialog with the error. Host discovery tries `dev list --json` for 10 seconds, then local session/frecency files, DNS de-duplicates, and validates presets.

### VCS, teams, timers, and Claudling

VCS detects Sapling/Hg versus Git. Sapling has a cached, background-fed built-in smartlog using renderdag, navigation/goto/refresh and agent explanation. Git launches lazygit or shows an intentional missing-tool state. Scan on open/manual refresh, not recursive periodic polling.

The live team builder is an ephemeral tree, not the older persistent team design in docs. Nodes have cwd/name/role/internet/harness; root is coordinator. Arena/tombstone IDs keep selections stable. Preflight validates names, directories, addresses and collisions. Launch creates sessions, a labeled comm group and clikan board, then injects `[FORGE-TEAM]` JSON to the coordinator. The bundled `forge-team-mode` skill defines protocol. Watch delivery 90 seconds; retry partial failure without duplicating nodes.

Timers are once/interval/daily-weekday, target one session, carry prompt and optional clear-context, and clean up on exit. Interval fires immediately as a test. Daily recurrence currently advances fixed 24 hours; retain unless deliberately redesigning DST behavior.

Claudling is an optional persistent ASCII pet. Stats: hunger, happiness, stress, energy, trust, intelligence. Hooks affect XP/mood; persist clicks, streaks, reactions, warm/cold return, and a daily post-8-AM journal. Tiers: Hatchling 1–5, Familiar 6–15, Companion 16–40, Elder 41–79, Mythic 80+. Missing DB must degrade safely.

Logging is bounded and terminal-safe. Instrument hook queue/phase latency and summarize notable cases periodically. Telemetry is on by default, nonfatal, reports aggregate host/platform/version/runtime/session/permission metadata to the configured internal endpoint, and gets at most two seconds to flush on exit.

## 10. UI contract

All styles come from semantic APIs in `src/theme.rs`; no inline RGB or arbitrary foreground styles. Green means running/success, amber means active/brand, teal means information/keys, yellow means warning/command, red means danger/exited, muted gray means inactive.

Use rounded borders. Focused/unfocused panels use semantic borders; command-mode terminal border is yellow; modals use modal background. Status glyphs: green `●` running, yellow `○` starting, red `×` exited. Focused fields begin `▸`. Key hints alternate `key_hint()` and `key_desc()`. Never show blank panels: explain content and how to populate it. Use two-space content indentation and padded status spans. Separate testable content generation from frame rendering; derive modal data from mode, then draw.

## 11. Complete MCP interface

### Forge: communication and control (15)

| # | Tool and inputs | Result and semantics |
|---:|---|---|
| 1 | `ask_session(target,message)` | Conversation UUID immediately; question and answer arrive by asynchronous PTY injection. |
| 2 | `list_sessions()` | Text containing name, cwd, status, current and reachable state. |
| 3 | `send_response(conversation_id,message)` | Confirms response was queued to asker. |
| 4 | `tell_session(target,message,conversation_id?)` | New tell returns ID and expects ack; existing conversation is a no-ack follow-up. |
| 5 | `ack_message(conversation_id)` | Marks tell acknowledged and queues notice to sender. |
| 6 | `send_file(target,path)` | Offers one bounded relative file; yields offer ID, digest and metadata. |
| 7 | `accept_file(offer_id,destination_path)` | Atomically creates a private non-overwriting relative file, then consumes offer. |
| 8 | `compact_session(target)` | Queues `/compact` through idle injection. |
| 9 | `schedule_prompt(prompt,delay_seconds=0,clear_context=false)` | Returns future self-injection timer ID. |
| 10 | `cancel_scheduled_prompt(timer_id)` | Cancels it. |
| 11 | `message_user(message)` | Returns conversation ID; badges TUI and optionally forwards remotely. |
| 12 | `screenshot(window_name)` | macOS app/title match; returns an MCP base64 PNG image block plus caption, else error. |
| 13 | `start_session(path?/od_preset?,name?,harness?,internet?,prompt?,connection?,host?,od_type?,communication_group?)` | Creates validated session and returns ID/name text. |
| 14 | `set_session_status(kind,message)` | Replaces caller's bounded sticky status. |
| 15 | `clear_session_status()` | Clears it. |

### Memory (7)

| # | Tool | Result |
|---:|---|---|
| 16 | `memory_save(kind,title,content,scope_type?,scope_key?,tags?,confidence?,importance?)` | ID and metadata. |
| 17 | `memory_search(query,scope filters,kinds,tags,limit,detail)` | Ranked formatted matches at requested detail. |
| 18 | `memory_get(id)` | Full record. |
| 19 | `memory_get_context(scope_type,scope_key,goal,max_items)` | Curated bounded context. |
| 20 | `memory_record_event(event_type,content,scope...)` | Event ID. |
| 21 | `memory_timeline(scope,limit,event_types)` | Reverse chronological events. |
| 22 | `memory_archive(id)` | Archive confirmation. |

### Checklists and tasks (10)

| # | Tool | Result |
|---:|---|---|
| 23 | `checklist_get_active` | Active checklist, progress, tasks. |
| 24 | `checklist_get_archived` | Archived lists and summaries. |
| 25 | `checklist_create(title,description?)` | UUID; archives prior active workspace list. |
| 26 | `checklist_complete(summary)` | Archives completed list. |
| 27 | `task_create(checklist_id,title,position?)` | Task UUID. |
| 28 | `task_update(task_id,title?,blocked_reason?)` | Updated task. |
| 29 | `task_set_status(task_id,status)` | Validated state update. |
| 30 | `task_set_focus(task_id)` | Makes task sole focus. |
| 31 | `task_reorder(task_id,new_position)` | Transactional reorder. |
| 32 | `task_add_note(task_id,note_body)` | Attributed note. |

### Walkthrough (5)

| # | Tool | Result |
|---:|---|---|
| 33 | `walkthrough_start(file_path,title,steps?)` | Loads file, switches tab, returns file/step information. |
| 34 | `walkthrough_add_step(start_line,end_line,explanation,file_path?,position?)` | Inserts step. |
| 35 | `walkthrough_update(step_index,start_line?/end_line?/explanation?)` | Updates step. |
| 36 | `walkthrough_end(summary?)` | Ends walkthrough. |
| 37 | `walkthrough_answer(answer)` | Resolves latest pending question. |

### Whiteboard (9)

| # | Tool | Result |
|---:|---|---|
| 38 | `whiteboard_start(title,elements?)` | JSON text with board ID, authenticated URL, revision, count and client-ID map; opens browser. |
| 39 | `whiteboard_list(session_id=self|*|exact,include_completed=true,include_archived=false,limit=50)` | Summaries. |
| 40 | `whiteboard_open(board_id)` | JSON text with URL/revision/count; opens browser. |
| 41 | `whiteboard_add(board_id,elements)` | Atomic create; revision, ID map, count. |
| 42 | `whiteboard_update(board_id,elements)` | Atomic full replacements; revision and IDs. |
| 43 | `whiteboard_delete(board_id,element_ids)` | Revision plus deleted/missing IDs. |
| 44 | `whiteboard_highlight(board_id,element_ids,reason?,ttl_seconds=15)` | Transient overlay, no revision. |
| 45 | `whiteboard_answer(board_id,answer)` | Appends chat answer and clears pending. |
| 46 | `whiteboard_end(board_id,summary?)` | Completes, makes read-only, persists. |

### Visual, projects, and terminal (11)

| # | Tool | Result |
|---:|---|---|
| 47 | `visual_show(path,title?,alt?)` or `(content,format=mermaid|html,title?,alt?)` | Published dimensions/content type/protocol/warning; schema depends on raster capability. |
| 48 | `project_create(name,display_name?,description?,root_path?,paths?)` | New registry record. |
| 49 | `project_get(name)` | Full project. |
| 50 | `project_list()` | All projects. |
| 51 | `project_update(name,display_name?/description?/root_path?)` | Updated project. |
| 52 | `project_delete(name)` | Cascading deletion confirmation. |
| 53 | `project_path_add(project_name,path,description?)` | Adds path. |
| 54 | `project_path_remove(project_name,path)` | Removes path. |
| 55 | `terminal_exec(command,cwd?,timeout_seconds?)` | Starts or requests approval; execution ID/state. Unix only. |
| 56 | `terminal_read(exec_id?,wait_ms?,max_bytes?)` | Cursor-based output/status. Unix only. |
| 57 | `terminal_send(key)` | Ctrl-C only. Unix only. |

Forge therefore advertises 57 tools on Unix and 54 without terminal support.

### Independent clikan MCP (8)

Clikan initializes as protocol `2024-11-05`, server `clikan` 1.0.0, with instructions to claim, update, and finish cards.

| # | Tool | Result |
|---:|---|---|
| 58 | `board_list()` | Boards and per-column counts. |
| 59 | `board_get(board_name? or board_id?)` | Columns/cards with WIP, priority, assignee, progress. |
| 60 | `card_create(board_name,title,column=Backlog,description?,assignee?,priority?,tags?,due_date?)` | New card ID/details. |
| 61 | `card_move(card_id,column)` | Move subject to WIP; final column sets progress 100. |
| 62 | `card_update(card_id,title?,description?,assignee?,priority?,progress?,tags?,due_date?)` | Partial update; progress clamped 0–100. |
| 63 | `card_delete(card_id)` | Confirmation. |
| 64 | `card_assign(card_id,assignee?)` | Explicit assignee or `FORGE_SESSION_NAME`/`unknown`; moves to Doing if possible. |
| 65 | `board_create(name,columns?)` | Creates board. Current handler ignores custom `columns` and uses defaults. |

Except screenshot, results are text content; some contain JSON text. Both servers read/write one JSON object per line. Clikan caps input lines at 1 MiB.

## 12. Build, installation, and release

Developer validation:

```bash
cargo build
cargo test
cd clikan && CGO_ENABLED=0 go build -o clikan ./cmd/clikan
cd clikan && CGO_ENABLED=0 go test ./...
cd assets/whiteboard && npm run build
```

`make build` performs release Rust and static Go. `make test` runs Rust and Go but **omits frontend validation**, so CI must add the frontend build.

`make install` first cross-compiles Linux x86-64 with cargo-zigbuild, uploads it through `jf`, writes `linux-binary-handle.txt`, then builds macOS Forge and clikan, copies to `~/.local/bin`, makes executable, ad-hoc signs, and registers. That order embeds the fresh Linux handle. Remove old macOS binary inodes before replacement because code-sign verdicts are inode-cached.

The release installer selects Darwin/Linux arm64/amd64 assets from internal GitHub Enterprise, installs and verifies hooks plus MCP servers. All registration must be idempotent. Skill updates preserve user edits and write `.new` instead of overwriting modified content.

## 13. Tests and observed validation

### Results on 2026-09-12

| Stack | Command | Observed result |
|---|---|---|
| Rust | `cargo test` | **2,753 passed, 0 failed, 14 ignored; 2,767 registered** |
| Rust | `cargo build` | passed |
| Go | `CGO_ENABLED=0 go test ./...` | passed; 9 storage tests; other packages have no tests |
| Go | static `go build ./cmd/clikan` | passed |
| Frontend | `npm run build` | typecheck passed; **103 passed, 11 skipped** in 5 files; production build passed |

Vite warned that the main JS chunk is about 1,534.52 KiB (449.51 KiB gzip), above its advisory 500 KiB limit. No line/branch coverage tool is configured, so no percentage should be claimed.

Rust tests occur in roughly 111 modules and densely cover AppState, modals, remote launch, input, sessions, broker, PTY panes, MCP, status bar, Google Chat, supervised terminal, whiteboard HTTP/WS/state, configuration, visual limits/protocol/budget, permissions, databases, tasks, memory scoring, projects, timers, VCS parsing, and pet behavior.

The 14 ignored Rust tests are:

- one serial visual RSS stress gate;
- six broker nudge/second-idle-failure tests for disabled behavior;
- one credentialed live Google Chat round trip;
- five memory evaluations: retrieval accuracy, search precision, scope merge, team isolation, token budget;
- one manual fbsource smartlog dump.

Go's tests cover SQLite but domain card/WIP logic, MCP dispatch/schemas, and Bubble Tea UI lack dedicated tests. Add them before changing those layers.

Frontend coverage: scene diff 14, sidebar width 12, normalization 38 active plus 11 skipped, WebSocket 46, Chat sidebar 4. Skipped normalization tests document label materialization moved server-side; client-side synthesis previously orphaned bound Excalidraw text.

Manual release tests remain necessary for Kitty/iTerm/Sixel and outer tmux, macOS screenshot/WebKit, real reverse tunnels/OD, mobile APIs, harness-version compatibility, and raw terminal appearance.

## 14. Reconstruction plan (strict TDD)

For every module: write colocated Rust tests (or native Go/TS tests), run and observe red, implement minimally, then refactor while green.

1. **Foundations:** branding, errors/logging, semantic theme, config/migration, IDs/enums, safe text/path checks, atomic private-file helpers. Gate: serialization/default/unknown-config tests pass.
2. **Terminal core:** PTY reader/writer/parser, SessionManager/run-ID rebinding, input modes/paste/selection, AppEvent loop, dirty/grid render. Gate: multiple echo PTYs run/resize/switch/exit and terminal restores on error.
3. **Hooks/permissions:** harness normalization, no-prefetch listener, fail-open relay, deterministic policy/cache/audit/AI adapter, idempotent installers. Gate: no hook can hang the CLI; block wins; hostile display is safe.
4. **MCP/broker:** JSON-RPC, schemas, authentication, groups, pressure, injection, ask/tell, status/timers/start/compact, framed files, mobile adapters. Gate: fake sessions communicate only with shared group and all adverse exits are deterministic.
5. **Services:** projects, tasks, memory, walkthrough, timers, history/recovery, then Claudling. Gate: migration/CRUD/corruption/scope tests; slow I/O never runs on UI thread.
6. **Whiteboard/visual:** board model/revisions/persistence, HTTP auth/Origin/WS limits, React client, framed visual/decode/budget, platform raster/screenshot. Gate: stale resync, atomic mutations, quarantine, allocation caps, generation ordering, embedded release assets.
7. **Remote/VCS/teams/clikan:** command-builder tests, remote state/reconnect, VCS abstraction, team preflight/retry, Go domain/storage/TUI/MCP. Gate: bounded actionable remote failure, idempotent retry, CGO-off build.
8. **Integration/release:** registration repair, restore/reconnect, Linux-helper upload order, all automated suites, real-terminal/remote/mobile smoke matrix.

## 15. Non-negotiable invariants

- Workers never mutate UI/session state directly.
- PTY/network floods cannot starve input or rendering.
- Raw mode, alternate screen, images, cursor and pointer state restore on panic/exit.
- Current run ID—not a session name—authorizes requests.
- No shared group means no cross-session control or data transfer.
- No injection while target is busy or user is typing.
- Connections, queues, frames, files, pixels, output, retries and timers are bounded.
- Important files are atomic; database migrations are transactional.
- Relative confinement rejects traversal, symlinks, specials, and overwrite.
- Approval/audit displays cannot be spoofed by hostile text.
- An absent/slow Forge instance cannot indefinitely block a harness hook.
- Stale remote routes repair; reconnect loops are gated; shell inputs are validated/quoted.
- Five harnesses launch with their exact flags and hook capabilities.
- Tool surface matches 57 Forge Unix tools plus 8 clikan tools.
- Rust, static Go, frontend typecheck/tests/build all pass; ignored tests stay documented.
- Release binaries are self-contained and registrations idempotent.

## 16. Source-of-truth cautions

Current code, not older plans, establishes that:

- memory is experimentally gated and defaults off;
- Forge exposes 57 Unix MCP tools, not older smaller counts;
- automatic broker idle nudges are disabled;
- teams are an ephemeral tree builder, not a named persistent registry;
- visual TTL is accepted but ignored; visuals stay sticky;
- Go is 1.25.1;
- `make test` omits frontend tests;
- clikan `board_create.columns` is exposed but ignored;
- server-side, not client-side, materializes bound whiteboard labels.

Changing any of these requires a failing compatibility test plus synchronized MCP instructions, manual/config documentation, and migration behavior. This prevents executable behavior and agent/human contracts from drifting again.

## Appendix A: Rust module map

This map is the recommended ownership boundary when recreating the source tree.

| Module | Responsibility |
|---|---|
| `main.rs` | CLI parsing, legacy migration, self-repair, config/env resolution, TUI launch. |
| `branding.rs` | Central product/binary/config naming constants so a rename cannot leave scattered literals. |
| `app.rs` | `AppState`, startup/shutdown, main loop, event reduction, cross-subsystem orchestration and high-level input actions. |
| `event.rs` | Typed cross-thread event protocol. |
| `input.rs` | Input modes and pure state for dialogs, text fields, OOBE, prefix, switcher, teams and selection. |
| `commands.rs` | User-command definitions/dispatch helpers. |
| `session.rs` | Session/PtyPane/SessionManager core; maps, lifecycle, PTY threads. |
| `session_launch.rs` | Spawn argument/environment construction and launch preparation. |
| `session_status.rs` | Status kinds, validation, bounded display state. |
| `session/agent_terminal.rs` | Per-session agent terminal model and commands. |
| `session/capture.rs` | Bounded terminal output capture and cursor reads. |
| `session/exec_unix.rs` | Unix PTY/process-group execution primitives. |
| `session/exec_supervisor.rs` | Command lifecycle supervision, timeout, termination, drain and reap. |
| `cli_tool.rs` | Harness abstraction: names, commands, flags, hook mappings, resume behavior. |
| `hooks/install.rs` | Idempotent harness hook installation/removal. |
| `hooks/relay.rs` | Short-lived fail-open relay executable path. |
| `hooks/listener.rs` | Unix/TCP listener, connection bounds, first-line dispatch. |
| `hooks/mod.rs` | Hook types, payload normalization and shared definitions. |
| `permission/mod.rs` | Policy orchestration and mode decisions. |
| `permission/rules.rs` | Deterministic allow/block/safe classifications. |
| `permission/cache.rs` | Normalized one-shot/reusable decision cache. |
| `permission/audit.rs` | Private append-only audit output. |
| `ai/openai.rs`, `ai/anthropic.rs` | Provider-specific permission/memory HTTP adapters. |
| `ai/prompt_loader.rs` | System prompt loading; `ai/mod.rs` supplies common interface/config. |
| `comms/mcp.rs` | Tool schemas, argument validation, mapping calls into messages. |
| `comms/serve.rs` | Stdio JSON-RPC lifecycle, initialize instructions/context, route client. |
| `comms/broker.rs` | Conversation state machine, queues, pressure and delivery. |
| `comms/groups.rs` | Group model, overlap ACL and colors. |
| `comms/file_transfer.rs` | Offer limits, staging, claims, digest and consume protocol. |
| `comms/terminal_exec.rs`, `terminal_text.rs` | MCP terminal adapter and safe output shaping. |
| `comms/gchat.rs`, `telegram.rs` | Mobile transports, polling, correlation and backoff. |
| `comms/screenshot.rs` | macOS window capture adapter. |
| `comms/install.rs` | MCP registration for Claude/Codex/Gemini/MetaCode and clikan. |
| `comms/msgtext.rs` | Stable injected message envelopes. |
| `memory/db.rs` | Schema, migrations and SQL. |
| `memory/service.rs` | CRUD/search/context business rules. |
| `memory/scoring.rs` | Ranking math. |
| `memory/embedding.rs` | Provider request/response and vectors. |
| `memory/worker.rs` | Nonblocking read/embedding worker. |
| `memory/observations.rs` | Hook-to-observation extraction. |
| `memory/broadcast.rs` | Team-memory announcements. |
| `memory/mcp.rs` | Seven tool definitions and handlers. |
| `memory/evals.rs` | Ignored quality/evaluation gates. |
| `tasks/db.rs`, `service.rs`, `mcp.rs` | Checklist persistence, business invariants, ten tools. |
| `project/db.rs`, `mcp.rs`, `mod.rs` | Registry persistence, tools and cwd matching types. |
| `walkthrough/mod.rs`, `mcp.rs` | State/file reading and five tools. |
| `whiteboard/mod.rs` | Main board types/state/revision/subscribers. |
| `whiteboard/server.rs` | HTTP/WebSocket parsing, auth, rate/liveness limits. |
| `whiteboard/persist.rs` | Atomic save, load, archive and quarantine. |
| `whiteboard/assets.rs` | Embedded SHA-addressed frontend bundle. |
| `whiteboard/summarize.rs` | Semantic scene-change classification/debounce. |
| `whiteboard/mcp.rs` | Nine board tool schemas and mutation adapters. |
| `visual/limits.rs`, `protocol.rs`, `ingress.rs` | Validation caps, framed protocol, concurrent permits. |
| `visual/format.rs` | Image type/normalization helpers. |
| `visual/state.rs`, `budget.rs` | Per-session current visual and global decoded-memory eviction. |
| `visual/view_controller.rs`, `kitty_viewport.rs` | Zoom/pan/click coordinates and protocol-specific placement. |
| `visual/raster/*` | macOS child process and hidden WebView rendering. |
| `visual/metrics.rs` | Bounded diagnostics. |
| `remote/connection.rs` | OD/SSH connection and ControlMaster state machine. |
| `remote/od.rs` | Discovery/presets/parsing. |
| `remote/mod.rs` | bootstrap command generation and shared remote types. |
| `vcs/backend.rs` | Repository detection/command abstraction. |
| `vcs/mod.rs` | smartlog state, background requests, lazygit integration. |
| `teams/mod.rs` | Team tree, validation, spawning/brief semantics. |
| `timers/mod.rs`, `schedule.rs` | Runtime timer model and recurrence calculation. |
| `tamagotchi/state.rs`, `feed.rs`, `spawn.rs`, `render.rs`, `db.rs` | Claudling model, event reactions, generation/animation, view and persistence. |
| `persist.rs`, `history.rs` | Session checkpoint/save and directory history. |
| `selection.rs` | Terminal selection geometry and extracted text. |
| `theme.rs` | All semantic colors/styles. |
| `ui/*` | Frame rendering and testable view models for panes, bars, modals, tasks, memory, teams, VCS, visual, walkthrough, whiteboard. |
| `skills/install.rs` | Bundled skill installation with user-edit preservation. |
| `key_refresh.rs` | Credential refresh scheduling. |
| `instrument.rs`, `logging.rs`, `telemetry.rs` | latency metrics, file logging, bounded analytics. |
| `clikan/mod.rs` | Forge-side embedded clikan process integration. |
| `safe_text.rs` | Security-grade untrusted-text display encoding. |

## Appendix B: Event and concurrency contract

`AppEvent` is not a miscellaneous notification bag; it defines the thread boundary. Recreate these families:

| Event family | Producers | Main-thread effect |
|---|---|---|
| Tick/Input/Resize | terminal event source | advance timers/modes, route key, resize panes |
| PTY output and agent/terminal/SCM exit | PTY readers/waiters | mark dirty, transition/retain session or pane |
| OD allocated/master ready/master failed/allocation exit/discovery complete | remote workers | advance connection state or surface launch error |
| HookEvent | listener | authenticate, update activity/log, decide permission through response channel |
| CommsMessage | MCP connection | reduce broker/control request and answer request channel |
| File authorize/commit/accept/release/confirm | framed connection workers | enforce two-phase offer state on owner thread |
| RemoteReply/RemoteCommand/Heartbeat | mobile poller | inject/carry out bounded operator action/health update |
| Visual begin/frame/render-source/rasterized/restarted/failed/ready | ingress/decode/raster workers | reserve budget, publish latest generation, redraw or release guards |
| VcsData | scan worker | replace matching session result only |
| Whiteboard events | HTTP/WS/persist workers | subscribe/unregister/heartbeat, patch/chat, snapshot, acknowledge persistence |

Response channels are deliberately one-shot/synchronous only at boundaries where the producer must know authorization, acceptance, or a hook decision. They have bounded waits. Large work—network calls, image decode, SQL retrieval, remote commands—runs off-thread and reports immutable results. Include session/request generations in results whenever cancellation cannot stop the underlying work, and discard stale generations on receipt.

Whiteboard subscription ordering is an important miniature protocol: validate board/token, create subscriber ID, send `scene.init` **before** inserting the sender into the broadcast list, return the ID, then allow broadcasts. Otherwise a patch can race ahead of initial state.

## Appendix C: Clikan behavior and UI

The Go architecture is intentionally conventional:

- `cmd/clikan/main.go`: choose TUI versus `mcp-serve`, resolve `~/.clikan/clikan.db`, adapt storage to UI interface;
- `internal/kanban/types.go`: Board/Column/Card/Priority and pure movement/reordering behavior;
- `internal/storage/sqlite.go`: WAL, migration, board/card/column CRUD and last-board setting;
- `internal/mcp/server.go`: JSON-RPC loop, eight schemas and handlers;
- `internal/tui/board.go`: Bubble Tea root model and 500 ms hash-based database polling;
- separate picker, card edit/details, column edit and help models.

Cards contain string ID, title, description, current column, assignee, string tags, enum priority, created/updated times, optional due date and progress 0–100. Moving into the last column sets 100%. A move fails if the target is absent or its nonzero WIP limit is full. Persistence positions preserve card and column order.

The TUI reloads from SQLite every 500 ms and only replaces its board when a content hash changes, which is how MCP updates become visible without direct IPC. It clamps selections after external deletion and maintains per-column scroll offsets.

Clikan keyboard contract:

- arrows or `h/l` columns, `j/k` cards, `1`–`4` direct column, Tab cycle, `g/G` first/last;
- PageUp/PageDown or Ctrl-U/Ctrl-D scroll; Home/End or Ctrl-A/Ctrl-E extremes;
- Space/`.` next column, Backspace/`,` previous, `>` Done, `<` Backlog;
- Enter details, `a` add, `d` delete, `e` edit, `t` urgent tag, `p` priority, `@` assignee, `x` complete, `+/-` progress;
- `K/J` reorder one, `T/B` top/bottom;
- `C` column editor (add/rename/delete/WIP/reorder), `W` board picker (switch/add/rename/delete);
- `?` help, `r` refresh, `q` or Ctrl-C quit.

Unlike Forge UI, clikan currently owns Lip Gloss colors locally. If rebuilding as a compatible standalone program, preserve its rounded selected-column/help borders, clear selection, priority/progress cues, and responsive one/two-column help.

## Appendix D: Failure and security checklist

Before declaring any implementation production-ready, exercise these negative cases:

- malformed/oversized JSON-RPC lines, missing IDs, notifications, unknown methods/tools and bad schemas;
- forged/stale run ID, duplicate name, missing group, exited target, full pressure queue;
- target exit before delivery, after ask delivery, before tell ack, and during response delivery;
- file size mismatch/digest mismatch, expired/claimed offer, symlink swap, parent symlink, existing destination, disconnect after write but before confirm;
- hook socket absent/refused/slow, receiver dropped, main event queue delayed, invalid harness payload;
- regex block and allow both matching, classifier timeout/malformed reply, audit permission drift;
- PTY child exits before reader starts, timeout during output, Ctrl-C race, descriptor leak, invalid UTF-8, output-ring truncation;
- stale visual generation, decompression bomb dimensions, wrong MIME, overlong header, raster child crash/restart, terminal protocol cleanup;
- stale whiteboard revision, slow subscriber, invalid token/origin, oversized patch/chat, reconnect before subscribe, persistence failure/corrupt recovery;
- config partial write, unknown fields, legacy-dir collision, live checkpoint, corrupt save, full disk;
- SSH quoting attacks, host-key/auth fast failure, exit 255 flapping, stale reverse route, OD discovery timeout;
- mobile spoofed sender, duplicate poll results, API throttle/backoff, reply to deleted session;
- clikan WIP limit, duplicate board, deletion under selection, external MCP mutation during editor, v1 database migration rollback.

Treat every network, hook, PTY, subprocess, and database error as recoverable unless the main terminal itself cannot initialize. Surface actionable state in the correct panel, release permits/claims/child descriptors, and keep unrelated sessions operational.
