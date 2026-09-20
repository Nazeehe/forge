# Forge themes

External themes live under `~/.forge/themes/`. Each theme is one JSON
file — either a `*.json` file directly inside that directory, a
`theme.json` at its top level, or a `*/theme.json` one level down:

```text
~/.forge/themes/
  square.json
  round.json
  tokyo-night.json
  my-theme/theme.json
```

Press `Ctrl-b e` in forge to open the picker. It lists the builtin
`default` (today's look) plus every valid file, previews each theme's
button shape inline, and applies the selection at runtime with `Enter`.
The choice persists in `~/.forge/config.toml` (`theme = "…"`) and is
restored on the next launch. A broken file never hides the rest: it is
skipped, and an unknown saved name warns and keeps `default`.

## File format

All sections are optional; a minimal theme is one `colors` object (the
name falls back to the file stem when absent):

```json
{
  "name": "my-theme",
  "colors": {
    "brand": "#bb9af7",
    "tab_active": {
      "fg": "black",
      "bg": "#bb9af7",
      "modifiers": ["bold"]
    }
  },
  "buttons": {
    "left": "[",
    "right": "]"
  },
  "borders": "rounded"
}
```

- `name`: non-empty string. Shown in the picker and stored in the
  config. Omit it to use the file name.
- `colors`: maps a role to a color string or a full style object.
  Roles: `text`, `muted`, `success`, `running`, `starting`, `exited`,
  `warning`, `danger`, `info`, `brand`, `command`, `focus`,
  `tab_active`, `tab_inactive`, `border_focused`, `border_unfocused`,
  `border_modal`, `key_hint`, `key_desc`. Dashes read as underscores
  (`tab-active` works). Missing roles keep the builtin look.
  - Shorthand: `"brand": "yellow"` sets the foreground only,
    keeping the builtin background and modifiers.
  - Full: `{"fg": "…", "bg": "…", "modifiers": […]}`. `fg`/`bg` are
    optional; `modifiers` is an optional list of `bold`, `italic`,
    `underlined`, `reversed`, `dim`, `blink`, `crossedout`, `hidden`.
- `buttons`: the button bookends — the two glyphs wrapping every pill
  button (`left`/`right`, each exactly one character). Square:
  `"["`/`"]"`. Round: `"("`/`)`. Flat bar: `"│"`/`"│"`. Omit for the
  builtin Nerd Font half circles.
- `borders`: modal/chrome border style — `plain`, `rounded`, `double`,
  or `thick`. Omit for `plain` (today's look).
- `highlight`: how a selected button shows it — `full` fills the whole
  button with the accent container (today's pill look, the default);
  `left` keeps the button in its rest color and lights only the left
  bookend. Shaped themes (triangles, wedges) usually want `left`, so
  the fill never drowns the shape.

Colors are named ANSI (`black`, `red`, `green`, `yellow`, `blue`,
`magenta`, `cyan`, `gray`/`grey`, `darkgray`, `lightred`,
`lightgreen`, `lightyellow`, `lightblue`, `lightmagenta`,
`lightcyan`, `white` — case-insensitive) or `#rrggbb` / `#rgb` hex
(which renders as truecolor `Rgb`). Unknown fields, roles, colors, or
multi-character caps are errors, so typos fail loudly when the file is
loaded instead of silently doing nothing.

## What other TUIs do (and what we took)

- **lazygit**: per-role colors (`activeBorderColor`,
  `inactiveBorderColor`, `selectedLineBgColor`, `optionsTextColor`)
  plus a `border` style (`single`/`double`/`rounded`/`hidden`).
  Taken: role-keyed colors plus a global border style.
- **helix** (`theme.toml`): a `[palette]` of named hex colors, then
  `ui.*` scopes (`ui.text`, `ui.statusline`, `ui.selection`,
  `ui.cursor`, gutter/menu/popup) each with `fg`/`bg`/`modifiers`.
  Taken: palette-by-name thinking (we accept hex inline) and the
  fg/bg/modifiers triple per role.
- **k9s skins / bat / btop / bottom**: per-view fg/bg, statusline
  variants, selection-row highlight, light/dark awareness, `NO_COLOR`
  respect. Taken: selection/focus stays a full style (not hue-alone),
  and the OS light-theme override still wins for absolute roles so
  light terminals stay readable.
- Common carriers: TOML/YAML/JSON config, hot reload or runtime
  switching, strict validation. Taken: JSON under `~/.forge/themes/`,
  runtime apply from the picker, strict unknown-field rejection.

Deliberately out of scope: syntax highlighting scopes (forge chrome is
themed; pane contents pass through raw), background images, fonts, and
per-syntax palettes.

## Examples in this directory

- `square.json` — ASCII-safe `[ ]` buttons, near-default colors.
  For terminals without a Nerd Font.
- `round.json` — `( )` buttons, rounded borders, cyan accents.
- `tokyo-night.json` — colors-only showcase: a full hex palette on the
  builtin pill caps, proving minimal and maximal themes compose.
- `triangle.json` — asymmetric bookends: full block `█` left, Powerline
  triangle `` right, warm amber accents, `left` highlight so selected
  buttons keep their color and only the block lights up.
- `wedge.json` — corner wedges: upper-right `◥` left, lower-left `◣`
  right, green accents.
