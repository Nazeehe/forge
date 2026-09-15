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

1. Confirm the CLI's resume form with `--help`: flag style
   (`--resume <id>`, fallback flag) or subcommand style
   (`resume [<id>]`, fallback `resume --last`).
2. Append one entry following the template above.
3. Restart forge. The create dialog lists agents in file order.
4. Verify: create a session with the new agent, quit, relaunch, and
   confirm the restore picker offers it and resume reconnects.

Renaming an agent orphans its saved snapshots (restore skips unknown
names with a reason instead of failing). Removing one is safe under
the same rule. Hook/MCP/skills installers are still keyed by name in
code, so a brand-new agent gets launching and resume but not
`install-*` support yet.

## Applying changes

Restart forge after editing either structured file. To verify a
change: relaunch, open the create dialog (`Ctrl-b c` by default) for
`agents.json` edits, or check the sidebar mode row for permission
edits. If forge refuses to start, read the startup error first — it
names the file, entry, and field.
