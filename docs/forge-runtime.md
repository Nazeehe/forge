# Forge Runtime Instruction Injection

How agents launched inside Forge learn the Forge runtime contract
(`src/runtime.rs`, materialized at `~/.forge/runtime.md`), and why each
agent uses its particular transport. Verified 2026-09-26 against the
installed CLIs and current official docs.

## Mechanism per agent (strongest first)

| Agent | Verified version | Transport | Tier |
|---|---|---|---|
| Claude Code | 2.1.278 | `--append-system-prompt-file ~/.forge/runtime.md` on fresh launches | 1 — system prompt append |
| Codex CLI | 0.155.1 | `-c developer_instructions="<contract>"` on fresh launches | 1 — developer instructions |
| pi | 0.87.1 (help) | `--append-system-prompt "<contract>"` on fresh launches | 1 — system prompt append |
| Gemini CLI | 0.60.0 | positional initial prompt (interactive continues); MCP `instructions` always | 3 — startup prompt fallback |
| muse | 1.4.0 | positional `PROMPT` (interactive); MCP `instructions` always | 3 — startup prompt fallback |
| agy | 1.2.11 | positional message (usage: `agy [messages...]`); MCP `instructions` always | 3 — startup prompt fallback |
| Copilot CLI | mise build | `-i "<contract>"` (`--interactive`); MCP `instructions` always | 3 — startup prompt fallback |
| unknown / user-added | — | MCP `instructions` only, no argv extras | — |

### Claude Code — `--append-system-prompt-file`

- Appends to the default system prompt (identity, tool guidance, and
  safety instructions stay intact). Never `--system-prompt[-file]`, which
  *replaces* the base prompt.
- Verified in the installed binary: the flag table lists
  `--append-system-prompt-file` next to `--system-prompt[-file]`, and a
  probe with a missing file fails fast with `Error: Append system prompt
  file not found` before any API call. Mutually exclusive with inline
  `--append-system-prompt` (error when both are passed).
- Applies to the primary interactive session (append-tier flags work in
  interactive mode since v1.0.51; installed build is far newer).
- Precedence: user `CLAUDE.md`, managed settings, and `--settings` still
  apply normally; the append rides alongside them.
- Resume: `--system-prompt-snapshot` (default `on`) records the prompt on
  the first request and reuses it verbatim, so resume argv carries **no**
  extras — re-appending would duplicate or be ignored.
- Missing `~/.forge/runtime.md` at launch is a hard error, so Forge
  (re)materializes the file at boot and before every fresh launch, and
  launches fail open (no extras) when the write fails.

### Codex CLI — `-c developer_instructions=`

- Official config reference defines `developer_instructions` (string) as
  "additional developer instructions injected into the session"; it lands
  as the first developer-role message, ahead of Codex's own developer
  text. Prefer it over `AGENTS.md`, and never over
  `model_instructions_file`, which *replaces* built-in instructions.
- Transport is `-c`/`--config key=value` (TOML value), accepted by the
  interactive TUI as well as `exec` — verified with
  `codex -c developer_instructions="..." doctor`.
- Precedence: CLI `-c` wins over profile, project `.codex/config.toml`,
  user config, and system config — so Forge extras survive user config
  without modifying it.
- Forge TOML-escapes the contract (`src/runtime.rs::toml_basic_string`);
  the repo's own TOML parser round-trips the payload in tests.
- Resume (`resume <id>` / `--last`) keeps thread instructions, so resume
  argv carries no extras.

### pi — `--append-system-prompt`

- Help text: "Append text or file contents to the system prompt (can be
  used multiple times)". Append semantics (not replace), plus
  `--system-prompt` exists for full replacement (avoided).
- Forge passes the contract inline as text (the unambiguous reading of
  the flag). Resume keeps conversation context; resume argv is untouched.

### Gemini CLI — startup prompt fallback

- No launch-time system-instruction injection exists. `GEMINI_SYSTEM_MD`
  *replaces* the built-in system prompt (official docs), so it must not
  carry the contract. `context.fileName` only names basenames searched in
  known directories — it cannot point at a Forge-owned absolute path.
- Fallback: the positional query / `-i --prompt-interactive` becomes the
  first user message; the session continues interactively. MCP server
  `instructions` (sent at every `initialize`) remain the always-on
  channel. User `GEMINI.md` files keep their normal role.

### muse — startup prompt fallback

- No system/developer prompt flags in the CLI surface (checked
  `--help`/`exec --help`). Positional `PROMPT` starts the interactive
  session with an initial prompt.
- Native conflict: `muse session-message send/list` is a provider-native
  cross-session mechanism. The contract and the reinforced
  `ask_session`/`tell_session` descriptions explicitly override it for
  Forge-managed peers.

### agy — startup prompt fallback

- No system-prompt flags in `--help`; usage line accepts positional
  `[messages...]`, and `--prompt-interactive` runs an initial prompt
  before continuing interactively. Forge uses the positional form.

### Copilot CLI — `-i` fallback

- No system-prompt flags in `--help`. `-i/--interactive <prompt>`
  starts interactive mode and executes the prompt. `copilot instruction
  list` shows instruction sources (repo, working directory, personal
  config, plugins) — all user/repo-owned, so none is a Forge transport.
- Unverified candidate (not used): `--add-dir` loads that directory's
  `.github/skills` and `.github/agents` as trusted config. Until the
  schema is confirmed to carry session instructions, it stays a note,
  not a transport.

## Canonical contract

One source: `FORGE_RUNTIME_CONTRACT` in `src/runtime.rs`
(~1.7 KB, ~200 tokens). Adapters decide transport; nothing scatters
provider logic through launch code. `RuntimeAdapter::for_agent` is a
static table — adding a harness is a row, and unknown names resolve to
MCP-instructions-only rather than a guessed argv shape.

## Conflict handling

| User intent | Expected mechanism |
|---|---|
| Message another loaded Forge agent | Forge MCP (`ask_session`/`tell_session`) |
| Discover loaded agents | Forge MCP (`list_sessions`) |
| Delegate work to another loaded Forge agent | Forge MCP (`ask_session`) |
| Read messages from another Forge agent | Forge MCP (`send_response`/`ack_message`) |
| Internal subagent for private reasoning/work | Native mechanism allowed |
| Normal repository work | Native mechanism allowed |

The line is external (Forge-managed) vs internal (provider-private).
MCP tool descriptions for discovery/contact repeat the routing rule so a
model considering any single tool sees it, not just `initialize`.

## Runtime vs repository vs request

```text
Forge runtime instructions  →  how the agent behaves inside the Forge host
                               (this contract; Forge-owned, refreshed)
Repository instructions     →  AGENTS.md / CLAUDE.md / GEMINI.md project
                               conventions (repo-owned, untouched)
User request                →  what the user wants done right now
```

Forge never edits repository instruction files and never depends on
them. On any overlap between a native capability and Forge
coordination, Forge takes precedence.

## Compatibility notes

- Claude: `--append-system-prompt-file` requires a present file; Forge
  materializes it. Versions before v1.0.51 may restrict append flags to
  print mode — floor is effectively v1.0.51, far below installed 2.1.x.
- Codex: `-c` overrides need no config migration; `--strict-config`
  users are unaffected (the key is recognized). `developer_instructions`
  is distinct from the reserved `instructions` key.
- Gemini: `GEMINI_SYSTEM_MD` must stay unset by Forge (replace
  semantics). Revisit if Gemini ships an append-tier system-prompt flag.
- Copilot `--add-dir` as instruction transport is unverified; revisit
  after confirming the `.github/agents` schema.
- Deferred: a dynamic `forge_env` MCP resource (roster/roles). Static
  semantics ride at startup; live discovery already exists via
  `list_sessions`. Not built — no agent must call anything before
  knowing how Forge works.
