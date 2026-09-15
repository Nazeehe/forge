# Forge agent configuration guide

You are configuring **forge**, a terminal multiplexer that runs AI coding
CLIs ("agents") in session panes. All settings live in `~/.forge/`.
Edit them with targeted changes, then restart forge to apply. Never
invent settings: every key below is load-bearing, and both structured
files fail startup on invalid content rather than guessing.

## Files in `~/.forge/`

| File | Purpose | If invalid |
|---|---|---|
| `config.toml` | General settings (below) | Startup error |
| `agents.json` | Agent CLI definitions (below) | Startup error |
| `AGENTS.md` | This guide | Ignored (docs only) |
| `sessions` | Auto-managed restore snapshots | Leave alone; forge dedupes and prunes these itself |
| `audit.log`, `forge.log` | Logs | Never edit |
| `endpoint.json` | Live IPC endpoint (hook relays reach the TUI through it) | Never touch; removed at shutdown |

`config.toml` preserves unknown sections across saves; `agents.json`
rejects unknown fields. A missing `config.toml` or `agents.json` is
recreated from packaged defaults; a missing `AGENTS.md` is re-dropped
unchanged. Your edits to any of them are never overwritten.

## `config.toml` reference

```toml
theme = "default"          # theme name; "default" follows the OS theme
prefix = "ctrl-b"          # command prefix chord

[permission]
mode = "yolo"              # off | safe-only | ai-assisted | yolo
allow = []                 # extra allowed tool patterns
block = []                 # blocked tool patterns (win over allow)

[ai]
provider = "openai"
model = "gpt-4.1-mini"
threshold = 0.6
timeout_seconds = 10
auto_refresh = true
key_env = "APE_API_KEY"    # env var holding the credential, not the key

[claudling]                # optional integrations, all default off except noted
enabled = false
[telemetry]
enabled = true
[clikan]
enabled = false
[pills]                    # rounded Nerd Font pill buttons; plain labels when off
enabled = true

[messaging]
idle_timeout_minutes = 10

[[bots]]                   # operator-registered external bot clients
name = "relay-bot"
token_file = "/run/secrets/relay-bot.token"  # secret lives here, never in this file
groups = ["peers"]
grants = []
```

`mode` must be exactly one of `off`, `safe-only`, `ai-assisted`,
`yolo`. `[[bots]]` entries need a non-empty `name` and `token_file`;
a missing token file fails that client closed at boot, not the boot.

## `agents.json` reference

Top level: `{"version": 1, "agents": [...]}`. Names must be unique,
non-empty, with no whitespace or `/`. The name is the session
`cli_tool` string, the create-dialog choice, and the installer key.

```jsonc
{
  "version": 1,
  "agents": [
    {
      "name": "claude",
      "binary": "claude",          // resolved through PATH at spawn
      "env_override": "CLAUDE_BIN", // explicit path wins when set and non-empty
      "model_flag": "--model",     // fresh launch: <binary> [--model <model>] [extra_args...]
      "default_model": "",         // empty = the CLI's own default
      "extra_args": [],            // always appended: launch after the model, restore after the resume tail
      "resume": {
        "subcommand": null,        // or "resume": prepended after the binary
        "with_id": { "flag": "--resume" },  // or { "positional": true }: resume <id>
        "without_id": ["--continue"]        // literal fallback argv, must not be empty
      },
      "supports_hooks": true,
      "session_attribution": "hook_env"     // or "cwd_window", see below
    }
  ]
}
```

Rules the parser enforces (violations are startup errors):

- `version` must be `1`; `agents` must be a non-empty array.
- `with_id` takes exactly one of `flag` / `positional`.
- `session_attribution` is `hook_env` or `cwd_window`. Use
  `cwd_window` only when the CLI scrubs the hook environment so forge
  cannot attribute hook events directly (today: muse); it falls back
  to matching a recently spawned, still-unbound session in the same
  cwd. Everything else uses `hook_env`.
- No approval bypasses, ever: any arg containing `yolo` or
  `dangerously` in `extra_args` or `without_id` is rejected. Forge
  gates through hooks where the CLI supports them instead.
- Only `name`, `binary`, and `resume` are required; the rest have
  safe defaults (`--model`, empty model/args, no hooks).

### Adding a new agent CLI

You are running inside forge, and you do the whole job: discover the
CLI, write its `agents.json` entry, install hooks/MCP/skills directly
into the CLI's own configuration files, verify, and report. Back up
every file before you edit it. `forge install-hooks` / `install-mcp`
/ `install-skills` only cover the built-in agents — for a new CLI,
your hands are the installer. Never bypass validation: an invalid
`agents.json` is a startup error naming the field — fix it and retry.

File rules for every settings file you touch (these mirror the
installer exactly): a missing file starts as `{}`; a corrupt file is
a hard stop, never clobber it; preserve every foreign entry; match
existing forge entries by the `hook-relay` command substring (hooks)
or the `"forge"` server key (MCP) so re-running never duplicates;
write atomically (temp file plus rename).

1. **Launch.** From `<bin> --help`: the model flag (usually
   `--model`) and any env-var binary override convention. If the CLI
   has no model flag, leave both `model_flag` and `default_model`
   out — an empty `default_model` means the CLI's own default, and a
   set one is passed with `model_flag` on every fresh launch.
2. **Resume.** Find the continue form: flag style (`--resume <id>`
   with a no-id fallback flag) or subcommand style (`resume [<id>]`
   with `resume --last`). Fill `subcommand`, `with_id`, and a
   non-empty `without_id`.
3. **Hooks.** Forge needs three capabilities, all mandatory:
   - a session-start event reporting the harness conversation id
     (restore resumes with it; without this, sessions cannot
     reconnect after a restart — use `session_attribution:
     "cwd_window"` only when the CLI additionally scrubs the hook
     environment, and say so in your report);
   - turn-start and turn-end events (queued peer replies deliver on
     the turn edges; without them sessions wedge after tool calls);
   - hook commands execute with the event JSON on stdin and can
     invoke `<forge-bin> hook-relay` — the absolute forge binary
     from `command -v forge` plus `hook-relay`, no shell
     metacharacters (some CLIs run hook commands without a shell).
   Find where the CLI reads hooks (`--help`, docs, settings files).
   For a `{hooks: {Event: [{matcher, hooks: [{command}]}]}}`
   document, add one `{matcher: "", hooks: [{type: "command",
   command: "<forge-bin> hook-relay"}]}` group per event, plus
   `"timeout": 10` inside the handler unless the CLI forbids
   per-hook timeouts. Then record the same path, events, and timeout
   in the entry's `hooks` block (`format: "claude-hooks"`) so
   future installer runs manage what you installed.
   **Any other mechanism — CLI-managed hooks, TOML hooks, anything
   needing a shell — is a hard stop.** Do not improvise. Omit the
   `hooks` block and report that `install-hooks` needs a new format
   in code; launching and resume still work, hooks do not.
4. **MCP.** Forge exposes one server: name `"forge"`, stdio
   transport, command = absolute forge binary, args `["mcp-serve"]`,
   requiring `FORGE_IPC_ENDPOINT` and `FORGE_RUN_ID` in its
   environment. Find the CLI's server registration (a settings JSON
   map — key it `"forge"` — or a CLI command like `mcp add`) and
   register exactly that. If the CLI scrubs MCP server environments
   (comms fail with no route after an otherwise good install), find
   its passthrough/allowlist mechanism and allowlist both variables.
5. **Skills.** If the CLI has a skills-directory convention (check
   its docs), copy `SKILL.md` from an already-installed harness
   (`~/.claude/skills/forge/SKILL.md` or
   `~/.codex/skills/forge/SKILL.md`, whichever exists) into the new
   CLI's equivalent path. If none exists and no convention is
   documented, skip skills and say so in your report.
6. **Write the entry and restart.** Following the template above. No
   approval bypasses anywhere (`yolo`, `dangerously`, and friends
   are rejected at parse). The create dialog lists agents in file
   order.
7. **Verify end to end.** Re-read every file you wrote and confirm
   exactly one forge entry per event/server, with foreign entries
   intact. Create a session with the new agent and confirm it binds
   (the harness id flows via SessionStart) and peer comms work over
   MCP. Quit, relaunch, confirm the restore picker offers it, and
   confirm resume reconnects. Remove your forge entries and re-add
   them to confirm a clean round-trip. (`forge uninstall-hooks` /
   `install-hooks` do this round-trip for the built-in agents, but
   they do not know about a CLI you installed by hand — redo your
   own edits.)
8. **Report.** What was added, the discovery evidence for each
   resume/hook/MCP claim (exact `--help` text or settings path),
   the verification performed, and anything left for code.

Renaming an agent orphans its saved snapshots (restore skips unknown
names with a reason instead of failing). Removing one is safe under
the same rule.

## Applying changes

Restart forge after editing either structured file. To verify a
change: relaunch, open the create dialog (`Ctrl-b c` by default) for
`agents.json` edits, or check the sidebar mode row for permission
edits. If forge refuses to start, read the startup error first — it
names the file, entry, and field.
