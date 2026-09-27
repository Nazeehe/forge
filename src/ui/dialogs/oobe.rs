//! First-run setup (OOBE): greet a fresh `~/.forge` owner, offer every
//! known agent CLI with checkboxes (all checked), and install hooks for
//! the picked set. Each CLI is a [`Cli`] plugin: adding one is a new
//! struct plus one entry in [`all_clis`].

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;

use crate::hooks::install::Outcome;

/// One known agent CLI: identity plus its install behavior. Thin
/// delegates over [`crate::hooks::install`]; the registry list below is what
/// the OOBE dialog and bulk installers drive.
pub trait Cli: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn setup(&self, home: &Path, forge_bin: &str) -> Outcome;
    fn uninstall(&self, home: &Path) -> Outcome;
}

pub struct ClaudeCli;
pub struct CodexCli;
pub struct MuseCli;
pub struct CopilotCli;
pub struct PiCli;

impl Cli for ClaudeCli {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn display_name(&self) -> &'static str {
        "claude"
    }
    fn setup(&self, home: &Path, forge_bin: &str) -> Outcome {
        crate::hooks::install::install_one_hooks(home, self.id(), forge_bin)
    }
    fn uninstall(&self, home: &Path) -> Outcome {
        crate::hooks::install::uninstall_one_hooks(home, self.id())
    }
}

impl Cli for CodexCli {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn display_name(&self) -> &'static str {
        "codex"
    }
    fn setup(&self, home: &Path, forge_bin: &str) -> Outcome {
        crate::hooks::install::install_one_hooks(home, self.id(), forge_bin)
    }
    fn uninstall(&self, home: &Path) -> Outcome {
        crate::hooks::install::uninstall_one_hooks(home, self.id())
    }
}

impl Cli for MuseCli {
    fn id(&self) -> &'static str {
        "muse"
    }
    fn display_name(&self) -> &'static str {
        "muse"
    }
    fn setup(&self, home: &Path, forge_bin: &str) -> Outcome {
        crate::hooks::install::install_one_hooks(home, self.id(), forge_bin)
    }
    fn uninstall(&self, home: &Path) -> Outcome {
        crate::hooks::install::uninstall_one_hooks(home, self.id())
    }
}

impl Cli for CopilotCli {
    fn id(&self) -> &'static str {
        "copilot"
    }
    fn display_name(&self) -> &'static str {
        "copilot"
    }
    fn setup(&self, home: &Path, forge_bin: &str) -> Outcome {
        crate::hooks::install::install_one_hooks(home, self.id(), forge_bin)
    }
    fn uninstall(&self, home: &Path) -> Outcome {
        crate::hooks::install::uninstall_one_hooks(home, self.id())
    }
}

impl Cli for PiCli {
    fn id(&self) -> &'static str {
        "pi"
    }
    fn display_name(&self) -> &'static str {
        "pi"
    }
    fn setup(&self, home: &Path, forge_bin: &str) -> Outcome {
        crate::hooks::install::install_one_hooks(home, self.id(), forge_bin)
    }
    fn uninstall(&self, home: &Path) -> Outcome {
        crate::hooks::install::uninstall_one_hooks(home, self.id())
    }
}

static CLAUDE: ClaudeCli = ClaudeCli;
static CODEX: CodexCli = CodexCli;
static MUSE: MuseCli = MuseCli;
static COPILOT: CopilotCli = CopilotCli;
static PI: PiCli = PiCli;

/// Every known CLI, in display order. Adding a CLI is a new [`Cli`]
/// struct plus one entry here.
pub fn all_clis() -> &'static [&'static dyn Cli] {
    // agy stays out: it is a supported session harness but documents
    // no hook surface, so setup could only ever report skipped.
    static ALL: &[&dyn Cli] = &[&CLAUDE, &CODEX, &MUSE, &COPILOT, &PI];
    ALL
}

/// First run means no `~/.forge` yet: the config dir is only created
/// by startup materialization, so its absence means Forge never ran.
pub fn is_first_run(home: &Path) -> bool {
    !crate::core::branding::config_dir(home).exists()
}

/// Install hooks for the picked CLI ids, in registry order. Unknown
/// ids are ignored; the install CLI subcommands stay the bulk path.
pub fn install_selected(
    home: &Path,
    ids: &[String],
    forge_bin: &str,
) -> Vec<Outcome> {
    all_clis()
        .iter()
        .filter(|cli| ids.iter().any(|id| id == cli.id()))
        .map(|cli| cli.setup(home, forge_bin))
        .collect()
}

/// Outcome of one key or click inside the OOBE dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OobeOutcome {
    Pending,
    Submitted(Vec<String>),
    Dismissed,
}

/// First-run dialog: cursor over the CLI rows, one checkbox per CLI
/// (all checked), and a centered `Setup` action. Enter submits the
/// checked set; Esc skips setup. After Setup, the dialog optionally
/// reopens in results mode listing one status row per installed CLI
/// with a centered `Done` action.
pub struct OobeDialog {
    cursor: usize,
    checked: Vec<bool>,
    pills: bool,
    results: Option<Vec<Outcome>>,
}

/// Rows above the CLI list: top pad (1) + greeting (2) + blank (1).
const LIST_TOP: u16 = 4;

fn inner_rect(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

fn cli_row_y(area: Rect, index: usize) -> u16 {
    inner_rect(area).y + LIST_TOP + index as u16
}

fn setup_y(area: Rect) -> u16 {
    cli_row_y(area, all_clis().len()) + 1
}

/// One outcome's row status: short text plus a semantic role. Text
/// always differs, so color never carries meaning alone.
fn outcome_status(outcome: &Outcome) -> (&'static str, crate::ui::theme::Role) {
    use crate::ui::theme::Role;
    if outcome.error.is_some() {
        ("error", Role::Danger)
    } else if outcome.installed {
        ("installed", Role::Success)
    } else if outcome.skipped {
        ("skipped", Role::Warning)
    } else {
        ("already set up", Role::Muted)
    }
}

/// The exact spans the view paints for the modal action: accent-filled
/// default carrying `*` with the `>` chosen-marker, like `create.rs`.
fn action_spans(pills: bool, label: &str) -> Vec<ratatui::text::Span<'static>> {
    use ratatui::text::Span;
    use crate::ui::theme::{Role, focus_row, style};
    if pills {
        use ratatui::style::{Color, Style};
        let (fill, left_cap, right_cap) = crate::ui::theme::button_chrome(
            true,
            style(Role::TabActive),
            style(Role::TabInactive),
            Color::Yellow,
        );
        vec![
            Span::styled(">", focus_row()),
            Span::styled(
                crate::ui::theme::pill_left().to_string(),
                Style::default().fg(left_cap),
            ),
            Span::styled(" ".to_string(), fill),
            Span::styled(label.to_string(), fill),
            Span::styled(" ".to_string(), fill),
            Span::styled(
                crate::ui::theme::pill_right().to_string(),
                Style::default().fg(right_cap),
            ),
        ]
    } else {
        vec![Span::styled(format!(">[{label}]"), focus_row())]
    }
}

/// Centered rect of the painted modal action: the same spans the view
/// centers, so clicks never desync from the paint.
fn action_button_rect(area: Rect, pills: bool, label: &str) -> Rect {
    use ratatui::text::Line;
    let inner = inner_rect(area);
    let width: usize = Line::from(action_spans(pills, label))
        .width()
        .min(inner.width as usize);
    Rect::new(
        inner.x + inner.width.saturating_sub(width as u16) / 2,
        setup_y(area),
        width as u16,
        1,
    )
}

impl OobeDialog {
    /// All CLIs checked: Setup installs everywhere unless told otherwise.
    pub fn new(pills: bool) -> Self {
        OobeDialog {
            cursor: 0,
            checked: vec![true; all_clis().len()],
            pills,
            results: None,
        }
    }

    /// Results mode after Setup: one status row per outcome.
    pub fn results(outcomes: Vec<Outcome>, pills: bool) -> Self {
        OobeDialog {
            cursor: 0,
            checked: Vec::new(),
            pills,
            results: Some(outcomes),
        }
    }

    /// True while showing per-CLI results instead of checkboxes.
    pub fn done(&self) -> bool {
        self.results.is_some()
    }

    /// Action label with its default mark: Setup picks, Done closes.
    fn action_label(&self) -> &'static str {
        if self.done() {
            "Done*"
        } else {
            "Setup*"
        }
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_checked(&self, index: usize) -> bool {
        self.checked.get(index).copied().unwrap_or(false)
    }

    /// Checked CLI ids, in registry order.
    pub fn selected_ids(&self) -> Vec<String> {
        all_clis()
            .iter()
            .enumerate()
            .filter(|(i, _)| self.is_checked(*i))
            .map(|(_, cli)| cli.id().to_string())
            .collect()
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = all_clis().len();
        if len == 0 {
            return;
        }
        self.cursor = (self.cursor as isize + delta).rem_euclid(len as isize) as usize;
    }

    fn toggle_cursor(&mut self) {
        if let Some(slot) = self.checked.get_mut(self.cursor) {
            *slot = !*slot;
        }
    }

    pub fn key(&mut self, key: &KeyEvent) -> OobeOutcome {
        // Results mode has no cursor: Enter and Esc both close it.
        if self.done() {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter => OobeOutcome::Dismissed,
                _ => OobeOutcome::Pending,
            };
        }
        match key.code {
            KeyCode::Esc => OobeOutcome::Dismissed,
            KeyCode::Enter => OobeOutcome::Submitted(self.selected_ids()),
            KeyCode::Up => {
                self.move_cursor(-1);
                OobeOutcome::Pending
            }
            KeyCode::Down => {
                self.move_cursor(1);
                OobeOutcome::Pending
            }
            KeyCode::Char('k') if key.modifiers.is_empty() => {
                self.move_cursor(-1);
                OobeOutcome::Pending
            }
            KeyCode::Char('j') if key.modifiers.is_empty() => {
                self.move_cursor(1);
                OobeOutcome::Pending
            }
            KeyCode::Char(' ') if key.modifiers.is_empty() => {
                self.toggle_cursor();
                OobeOutcome::Pending
            }
            _ => OobeOutcome::Pending,
        }
    }

    /// Left click: a CLI row moves the cursor and toggles, the Setup
    /// button submits. Same rows the view paints.
    pub fn click(&mut self, col: u16, row: u16, area: Rect) -> OobeOutcome {
        if col <= area.x
            || col >= area.right().saturating_sub(1)
            || row <= area.y
            || row >= area.bottom().saturating_sub(1)
        {
            return OobeOutcome::Pending;
        }
        let button = action_button_rect(area, self.pills, self.action_label());
        if row == button.y && col >= button.x && col < button.x + button.width {
            return if self.done() {
                OobeOutcome::Dismissed
            } else {
                OobeOutcome::Submitted(self.selected_ids())
            };
        }
        // Results rows are inert: only Done closes.
        if self.done() {
            return OobeOutcome::Pending;
        }
        let first = cli_row_y(area, 0);
        if row >= first {
            let index = (row - first) as usize;
            if index < all_clis().len() {
                self.cursor = index;
                self.toggle_cursor();
            }
        }
        OobeOutcome::Pending
    }

    /// Render the centered modal: opaque, greeting, checkbox rows with
    /// `>` plus reverse video on the cursor row, centered Setup pill,
    /// pinned hint. Content keeps border padding; rows are the same
    /// top-down order `click` hit-tests.
    pub fn view(&self, frame: &mut ratatui::Frame, area: Rect) {
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Clear, Paragraph};
        use crate::ui::theme::{Role, focus_row, style};
        // Opaque: the live grid must not show through the modal.
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(crate::ui::theme::border_type())
            .title(if self.done() {
                " Setup complete "
            } else {
                " Welcome to Forge "
            })
            .style(crate::ui::theme::modal_fill())
            .border_style(style(Role::BorderModal));
        let inner = inner_rect(area);
        frame.render_widget(block, area);
        let need: u16 = LIST_TOP + all_clis().len() as u16 + 4;
        if inner.height < need || inner.width < 30 {
            return;
        }
        let text = style(Role::Text);
        let pad_x = if inner.width >= 40 { 2 } else { 0 };
        let cx = inner.x + pad_x;
        let cw = inner.width.saturating_sub(pad_x * 2);
        let mut row = inner.y + 1;
        let header = if self.done() {
            ["Setup finished.", "Per-CLI results:"]
        } else {
            [
                "Welcome! Let's get your agent CLIs set up.",
                "Pick which CLIs install Forge hooks:",
            ]
        };
        for line in header {
            frame.render_widget(
                Paragraph::new(Line::from(vec![Span::styled(line, text)])),
                Rect::new(cx, row, cw, 1),
            );
            row += 1;
        }
        row += 1;
        if let Some(outcomes) = self.results.as_ref() {
            // Status rows sit on the same absolute rows `click`
            // hit-tests, even when fewer CLIs were picked.
            for (i, outcome) in outcomes.iter().enumerate() {
                let (status, role) = outcome_status(outcome);
                let line = Line::from(vec![
                    Span::raw("  "),
                    Span::styled(outcome.harness.clone(), text),
                    Span::styled(format!(": {status}"), style(role)),
                ]);
                frame.render_widget(
                    Paragraph::new(line),
                    Rect::new(cx, cli_row_y(area, i), cw, 1),
                );
            }
        } else {
            for (i, cli) in all_clis().iter().enumerate() {
                let checked = self.is_checked(i);
                let focused = i == self.cursor;
                let row_style = if focused { focus_row() } else { text };
                let (box_glyph, box_style) = if focused {
                    (if checked { "[x] " } else { "[ ] " }, focus_row())
                } else if checked {
                    ("[x] ", style(Role::Brand))
                } else {
                    ("[ ] ", style(Role::Muted))
                };
                let line = Line::from(vec![
                    if focused {
                        Span::styled("> ", focus_row())
                    } else {
                        Span::raw("  ")
                    },
                    Span::styled(box_glyph, box_style),
                    Span::styled(cli.display_name(), row_style),
                ]);
                frame.render_widget(Paragraph::new(line), Rect::new(cx, row, cw, 1));
                row += 1;
            }
        }
        // Absolute action row: subset results paint fewer rows, so the
        // walked `row` only matches in pick mode.
        row = setup_y(area);
        // Centered modal action on its own row, starting at the same
        // x `action_button_rect` centers, so click and paint agree.
        let button = action_button_rect(area, self.pills, self.action_label());
        let pad = button.x.saturating_sub(cx);
        let mut spans = vec![Span::raw(" ".repeat(pad as usize))];
        spans.extend(action_spans(self.pills, self.action_label()));
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(cx, row, cw, 1),
        );
        row += 2;
        let hint = if self.done() {
            "Enter Done • Esc"
        } else {
            "↑/↓ move • Space toggle • Enter Setup • Esc skip"
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(hint, style(Role::Muted))])),
            Rect::new(cx, row, cw, 1),
        );
    }
}

/// Centered modal area, tall enough for the greeting plus every CLI row.
pub fn oobe_area(term: Rect) -> Rect {
    let need: u16 = LIST_TOP + all_clis().len() as u16 + 4 + 2;
    let (w, h) = (64.min(term.width), need.min(term.height));
    Rect::new(
        term.x + term.width.saturating_sub(w) / 2,
        term.y + term.height.saturating_sub(h) / 2,
        w,
        h,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn scratch_home(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-oobe-test-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn first_run_when_forge_dir_missing() {
        let home = scratch_home("missing");
        assert!(is_first_run(&home), "no ~/.forge yet means first run");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn not_first_run_once_forge_dir_exists() {
        let home = scratch_home("exists");
        std::fs::create_dir_all(crate::core::branding::config_dir(&home)).unwrap();
        assert!(!is_first_run(&home));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn known_clis_listed_in_registry_order() {
        let ids: Vec<_> = all_clis().iter().map(|c| c.id()).collect();
        assert_eq!(
            ids,
            vec!["claude", "codex", "muse", "copilot", "pi"]
        );
    }

    #[test]
    fn dialog_selects_all_by_default() {
        let dialog = OobeDialog::new(true);
        assert_eq!(
            dialog.selected_ids(),
            vec!["claude", "codex", "muse", "copilot", "pi"]
        );
    }

    #[test]
    fn space_toggles_the_cursor_row() {
        let mut dialog = OobeDialog::new(true);
        dialog.key(&key(KeyCode::Char(' ')));
        assert!(!dialog.is_checked(0), "first row toggled off");
        assert!(dialog.is_checked(1), "other rows stay checked");
        assert_eq!(
            dialog.selected_ids(),
            vec!["codex", "muse", "copilot", "pi"]
        );
    }

    #[test]
    fn arrows_move_the_cursor() {
        let mut dialog = OobeDialog::new(true);
        dialog.key(&key(KeyCode::Down));
        assert_eq!(dialog.cursor(), 1);
        dialog.key(&key(KeyCode::Up));
        assert_eq!(dialog.cursor(), 0);
    }

    #[test]
    fn enter_submits_checked_ids() {
        let mut dialog = OobeDialog::new(true);
        dialog.key(&key(KeyCode::Down));
        dialog.key(&key(KeyCode::Char(' ')));
        assert_eq!(
            dialog.key(&key(KeyCode::Enter)),
            OobeOutcome::Submitted(vec![
                "claude".to_string(),
                "muse".to_string(),
                "copilot".to_string(),
                "pi".to_string(),
            ])
        );
    }

    #[test]
    fn esc_skips_setup() {
        let mut dialog = OobeDialog::new(true);
        assert_eq!(dialog.key(&key(KeyCode::Esc)), OobeOutcome::Dismissed);
    }

    #[test]
    fn install_selected_only_touches_checked() {
        let home = scratch_home("subset");
        let outs = install_selected(&home, &["claude".to_string()], "/tmp/forge-under-test");
        assert_eq!(outs.len(), 1, "one outcome per picked CLI: {outs:?}");
        assert!(outs[0].installed, "out: {:?}", outs[0]);
        let claude = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
        assert!(claude.contains("hook-relay"), "picked CLI installed: {claude}");
        assert!(
            !home.join(".codex/hooks.json").exists(),
            "unchecked CLI untouched"
        );
        assert!(
            !home.join(".config/muse/settings.json").exists(),
            "unchecked CLI untouched"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn results_paint_shows_status_per_cli() {
        use ratatui::{backend::TestBackend, Terminal};
        let home = scratch_home("results-paint");
        let outs = install_selected(
            &home,
            &["claude".to_string(), "pi".to_string()],
            "/tmp/forge-under-test",
        );
        assert_eq!(outs.len(), 2);
        let dialog = OobeDialog::results(outs, true);
        assert!(dialog.done(), "results mode");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| dialog.view(f, oobe_area(f.area())))
            .unwrap();
        let buf = terminal.backend().buffer();
        let text: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        assert!(text.contains("claude"), "row: {text}");
        assert!(text.contains("installed"), "status: {text}");
        assert!(text.contains("pi"), "row: {text}");
        assert!(text.contains("skipped"), "status: {text}");
        assert!(text.contains("Done"), "action: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn results_enter_and_esc_close_other_keys_stay() {
        let home = scratch_home("results-keys");
        let outs = install_selected(&home, &["pi".to_string()], "/tmp/forge-under-test");
        let mut dialog = OobeDialog::results(outs, true);
        assert_eq!(dialog.key(&key(KeyCode::Char(' '))), OobeOutcome::Pending);
        assert_eq!(dialog.cursor(), 0, "no cursor in results");
        assert_eq!(dialog.key(&key(KeyCode::Enter)), OobeOutcome::Dismissed);
        let outs = install_selected(&home, &["pi".to_string()], "/tmp/forge-under-test");
        let mut dialog = OobeDialog::results(outs, true);
        assert_eq!(dialog.key(&key(KeyCode::Esc)), OobeOutcome::Dismissed);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn results_click_done_closes() {
        use ratatui::{backend::TestBackend, Terminal};
        let home = scratch_home("results-click");
        let outs = install_selected(&home, &["pi".to_string()], "/tmp/forge-under-test");
        let mut dialog = OobeDialog::results(outs, true);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| dialog.view(f, oobe_area(f.area())))
            .unwrap();
        let area = oobe_area(terminal.backend().buffer().area);
        let buf = terminal.backend().buffer().clone();
        let (sx, sy) = find_cell(&buf, "Done").expect("Done painted");
        assert_eq!(dialog.click(sx, sy, area), OobeOutcome::Dismissed);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn pi_setup_skips_without_touching_home() {
        // Pi (pi-mono) has no native hook or MCP surface: setup reports
        // skipped instead of erroring, and writes nothing.
        let home = scratch_home("pi-skip");
        let outs = install_selected(&home, &["pi".to_string()], "/tmp/forge-under-test");
        assert_eq!(outs.len(), 1, "one outcome: {outs:?}");
        assert!(outs[0].skipped, "out: {:?}", outs[0]);
        assert!(outs[0].error.is_none(), "skip is not failure: {:?}", outs[0]);
        assert!(!home.join(".pi").exists(), "nothing written");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn modal_paints_greeting_cli_rows_and_setup() {
        use ratatui::{backend::TestBackend, Terminal};
        let dialog = OobeDialog::new(true);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| dialog.view(f, oobe_area(f.area())))
            .unwrap();
        let buf = terminal.backend().buffer();
        let text: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        assert!(text.contains("Welcome"), "greets: {text}");
        for name in ["claude", "codex", "muse", "copilot", "pi"] {
            assert!(text.contains(name), "lists {name}");
        }
        assert!(text.contains("Setup"), "action button");
    }

    #[test]
    fn click_row_toggles_and_click_setup_submits() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut dialog = OobeDialog::new(true);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| dialog.view(f, oobe_area(f.area())))
            .unwrap();
        let area = oobe_area(terminal.backend().buffer().area);
        // Find the painted `codex` cell and click it: no hand-computed coords.
        let buf = terminal.backend().buffer().clone();
        let (cx, cy) = find_cell(&buf, "codex").expect("codex painted");
        assert_eq!(dialog.click(cx, cy, area), OobeOutcome::Pending);
        assert!(!dialog.is_checked(1), "click toggled codex off");
        // Find the painted `Setup` cell and click it.
        let buf2 = {
            let mut t2 = Terminal::new(TestBackend::new(80, 24)).unwrap();
            t2.draw(|f| dialog.view(f, oobe_area(f.area()))).unwrap();
            t2.backend().buffer().clone()
        };
        let (sx, sy) = find_cell(&buf2, "Setup").expect("Setup painted");
        assert_eq!(
            dialog.click(sx, sy, area),
            OobeOutcome::Submitted(vec![
                "claude".to_string(),
                "muse".to_string(),
                "copilot".to_string(),
                "pi".to_string(),
            ])
        );
        let _ = buf;
    }

    fn find_cell(
        buf: &ratatui::buffer::Buffer,
        needle: &str,
    ) -> Option<(u16, u16)> {
        for y in buf.area.top()..buf.area.bottom() {
            let mut row_text = String::new();
            for x in buf.area.left()..buf.area.right() {
                row_text.push_str(buf[(x, y)].symbol());
            }
            if let Some(byte) = row_text.find(needle) {
                let col = row_text[..byte].chars().count() as u16 + buf.area.x;
                return Some((col, y));
            }
        }
        None
    }
}
