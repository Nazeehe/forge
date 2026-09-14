# Forge TUI guidance

This document proposes a consistent visual language for Forge's TUI chrome. It is based on the Forge screenshot in `../screens/forge.png`. Keep the PTY grid and visual pane as raw custom views; apply these rules to tabs, settings, dialogs, the switcher, status, and other controls.

## Core rules

- **Focus** means where keyboard input will go. **Selection** means the current, committed value. Show them with different marks even when they coincide.
- Use shape and text to communicate state; color reinforces meaning. Every state must remain readable in monochrome and with color vision differences.
- Keep one cell of inner padding for controls, one-cell gaps between adjacent controls, stable alignment, and a visible border around each panel. Use rounded borders where terminal glyph support allows them.
- Use semantic theme APIs in `theme.rs`; do not place RGB values in views. Suggested roles: `surface`, `surface_raised`, `border`, `text`, `muted`, `accent`, `focus`, `success`, `warning`, and `danger`.
- Reserve the existing yellow accent for the current item and primary action, and cyan for keyboard focus and chrome. Never rely on hue alone. Ordinary body text should retain full readable contrast; dim only disabled controls and secondary metadata.
- Use `>` plus reverse video for keyboard focus. Use `*` for an active tab or default action, `(o)` for a selected option, `~` for activity, and `!` for attention or destructive action. These marks have meaning within their control type; include a legend in help.
- Mouse hover may add a subtle underline or border cue, but must not look like keyboard focus or a committed selection.

## Buttons and settings

| State | Example | Rule |
| --- | --- | --- |
| Ordinary | `[Cancel]` | Neutral foreground and border. |
| Primary/default | `[Save*]` | Accent and bold; `*` marks the default action. |
| Keyboard focused | `>[Save*]` | Leading `>` and reverse video, regardless of button role. |
| Destructive | `![Delete]` | Danger color and `!`; require an explicit, named consequence. |
| Disabled | `[Export]` (dim) | Explain why nearby, such as `Export unavailable: no data`. |

Do not style a setting's value as if it were a button. For example:

```text
Autopilot: (o) Off  >( ) YOLO
```

Here Off is selected while keyboard focus is on YOLO. Enter or Space commits the focused option. The distinction addresses the ambiguous `[off] [Yolo]` highlight in the current screenshot. Successful and failed outcomes should also include text or a `+`/`!` mark, not only green or red.

## Session tabs

```text
Sessions: [1* codex] >[2  muse~] [3  claude!] [+2]
```

| Mark | Meaning |
| --- | --- |
| `*` | Active session; retain the accent underline or border. |
| `>` plus reverse video | Keyboard focus; a focused inactive tab is not yet active. |
| `~` | Busy or producing activity. |
| `!` | Needs attention or has an error; takes priority over `~`. |
| `[+N]` | Overflow; opens the session switcher. |

Enter or Space activates a focused tab; a mouse click activates it directly. Keep the active and focused tabs visible as width shrinks. Shorten names before moving tabs into overflow, but never truncate a status mark. The switcher should show full session names and explain any attention state. Do not equate a session's name with its authorization identity; run IDs remain the authority for requests.

## Modals

Use a consistent order: bordered title and optional severity mark, short task or consequence, fields or plain-language error details, action row, then a one-line key hint. Center the modal over a dimmed background. Target a width of `min(content width, 64, terminal width - 4)` cells, with a practical minimum near 34 cells. Fit height to content up to roughly 70% of the terminal; scroll longer bodies and show a `v more` cue. At very small terminal sizes, preserve readable actions and a visible focus target before decorative spacing.

Keep exactly one initial focus target visible. Tab and Shift-Tab cycle inside the modal; Esc cancels or closes it. Enter activates the focused control. The `*` default acts only when focus is not on another action. Restore prior focus when the modal closes. Focus the first editable field or primary action in a normal form. For a destructive confirmation, focus and default to Cancel, name the exact target, and explain the consequence. For an error, state the cause and next action; focus Retry only when retry is useful and safe, otherwise Close. Put raw diagnostics behind `[Details]`. Do not add permission modals: harness permissions follow the project's existing policy flow.

```text
+-- New session --------------------------------------+
| Name: [muse                 ]                        |
| Harness: (o) Codex  ( ) Claude  ( ) Muse            |
|                                                    |
| [Cancel]                         >[Create*]         |
| Tab next   Esc cancel   Enter choose                |
+----------------------------------------------------+

+-- Delete session ! ---------------------------------+
| Delete `muse` and its saved state?                  |
| This cannot be undone.                             |
|                                                    |
| >[Cancel*]                         ![Delete]         |
| Esc cancel   Tab move   Enter choose                |
+----------------------------------------------------+

+-- Message failed ! ---------------------------------+
| Only the target session can acknowledge a message. |
| No acknowledgement was recorded.                   |
|                                                    |
| >[Close*]  [Details]                                |
+----------------------------------------------------+
```

Use ASCII fallbacks for decorative glyphs that may occupy ambiguous cell widths. Do not signal activity or urgency through animation alone; if reduced motion is preferred, keep `~` static. Every control must be reachable by keyboard and have a full readable label in the switcher or help.

## Screen-level priorities

- Let the transcript use available width. The current status pane takes substantial space while leaving most of its center empty; make it narrower or collapsible, especially on small terminals.
- Summarize tool calls in the transcript by action, target, and outcome. Make raw JSON, long IDs, and repeated reply instructions expandable rather than showing them at the same visual weight as conversation.
- Label failed calls clearly with a reason and recovery action. A small red bullet alone is easy to miss.
- Label sidebar counters by scope and time window, remove repeated identity labels, and make the current session state most prominent.
- Keep only a few contextual shortcuts in the footer and expose `Ctrl-b ?` for the full help overlay.
- Check layouts at 80×24 and 120×40. Preserve the input, active/focused tabs, and essential actions before secondary status or shortcut text.

These are design proposals based on one screenshot. Verify interaction behavior and smaller-terminal layouts in the running TUI before treating observed concerns as confirmed defects.
