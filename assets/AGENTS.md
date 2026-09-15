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

Back up `agents.json` first, then work top to bottom. You are
expected to run shell commands for discovery (`--help`, `--version`,
inspecting settings files) but never to bypass validation: an
invalid file is a startup error naming the field — fix it and retry.

1. **Launch.** From `<bin> --help`: the model flag (usually
   `--model`) and any env-var binary override convention. If the CLI
   has no model flag, leave both `model_flag` and `default_model`
   out — an empty `default_model` means the CLI's own default, and a
   set one is passed with `model_flag` on every fresh launch.
2. **Resume.** Find the continue form: flag style (`--resume <id>`
   with a no-id fallback flag) or subcommand style (`resume [<id>]`
   with `resume --last`). Fill `subcommand`, `with_id`, and a
   non-empty `without_id`.
3. **Hooks — discover, then decide.** Forge needs three capabilities
   from the hook system, and all three are mandatory:
   - a session-start event reporting the harness conversation id
     (restore resumes with it; without this, sessions cannot
     reconnect after a restart — use `session_attribution:
     "cwd_window"` only when the CLI additionally scrubs the hook
     environment, and say so in your report);
   - turn-start and turn-end events (queued peer replies deliver on
     the turn edges; without them sessions wedge after tool calls);
   - hook commands execute with the event JSON on stdin and can
     invoke `<forge-bin> hook-relay` (find the forge binary with
     `command -v forge`; the command itself takes no shell
     metacharacters).
   - If the settings document is
     `{hooks: {Event: [{matcher, hooks: [{command}]}]}}`, write the
     `hooks` block (`format: "claude-hooks"`) with the discovered
     file path (relative to `~`, or `{env, default}` based when the
     CLI honors a home override), the exact event names, and
     `timeout: 10` unless the CLI forbids per-hook timeouts.
     `matcher` stays `""`.
   - **Any other mechanism — CLI-managed hooks (e.g. a `hooks`
     subcommand), TOML hooks, anything needing a shell — is a hard
     stop.** Do not hand-write settings files and do not improvise
     installer behavior. Report that `install-hooks` needs a new
     format in code; launching and resume still work, hooks do not.
4. **Write the entry** following the template above. No approval
   bypasses anywhere (`yolo`, `dangerously`, and friends are
   rejected at parse). Restart forge; the create dialog lists agents
   in file order.
5. **Install hooks.** Run `forge install-hooks` (the all-agents
   path, not a per-harness alias) and confirm the new agent's
   outcome line reads `installed` or `unchanged` — never `error` or
   `skipped`. `skipped` means the block did not take; recheck step 3.
6. **Verify end to end.** Create a session with the new agent and
   confirm it binds (the harness id flows via SessionStart).
   Quit, relaunch, confirm the restore picker offers it, and confirm
   resume reconnects. Then `forge uninstall-hooks` followed by
   `forge install-hooks` to confirm a clean round-trip.
7. **Report.** What was added, the discovery evidence for each
   resume/hook claim (exact `--help` text or settings path), the
   verification performed, and anything left for code (e.g. a new
   hook format, a per-harness install alias).

Renaming an agent orphans its saved snapshots (restore skips unknown
names with a reason instead of failing). Removing one is safe under
the same rule. MCP/skills installers are still keyed by name in
code, so a brand-new agent gets launching, resume, and hooks but not
`install-mcp` / `install-skills` support yet.

## Applying changes

Restart forge after editing either structured file. To verify a
change: relaunch, open the create dialog (`Ctrl-b c` by default) for
`agents.json` edits, or check the sidebar mode row for permission
edits. If forge refuses to start, read the startup error first — it
names the file, entry, and field.
