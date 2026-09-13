//! Create-session dialog (Phase 5c): keyboard-first form for the minimum
//! set — harness picker, session name, folder path, model override. Enter
//! submits a validated [`SessionSpec`]; Escape cancels. Validation rejects
//! empty/duplicate names and non-directory folders without closing.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tui_realm_stdlib::components::{Input, List};
use tuirealm::command::{Cmd, Direction};
use tuirealm::component::Component;
use tuirealm::state::{State, StateValue};

use crate::harness::Harness;

/// Which agent (if any) a new session runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    Shell,
    Agent(Harness),
}

/// Validated dialog output: everything `create_session` needs.
#[derive(Clone, Debug)]
pub struct SessionSpec {
    pub kind: SessionKind,
    pub name: String,
    pub cwd: PathBuf,
    pub model: String,
}

/// Dialog result after one input: still open, cancelled, or submitted.
#[derive(Debug)]
pub enum DialogOutcome {
    Pending,
    Cancelled,
    Submitted(SessionSpec),
}

const FIELD_COUNT: usize = 4;
const FOCUS_HARNESS: usize = 0;

/// Expand `~` against HOME; resolve relatives against the dialog's base.
pub fn expand_folder(raw: &str, base: &Path) -> PathBuf {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix('~') {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let mut out = PathBuf::from(home);
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        if !rest.is_empty() {
            out.push(rest);
        }
        return out;
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

/// Quote argv for `sh -c`: bare safe words pass through, everything else
/// gets single quotes with embedded quotes escaped.
pub fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if !a.is_empty()
                && a.bytes().all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/' | b':' | b'@' | b'+' | b',' | b'=' | b'%'))
            {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// New-session dialog: a harness List plus name/folder/model Inputs.
/// Tab cycles fields, arrows drive the harness row, Enter submits.
pub struct CreateDialog {
    harness: List,
    name: Input,
    folder: Input,
    model: Input,
    focus: usize,
    base_cwd: PathBuf,
    error: Option<String>,
}

impl CreateDialog {
    pub fn new(suggested_name: &str, cwd: &Path) -> Self {
        let harness = List::default()
            .rows(["shell", "claude", "codex", "muse"])
            .rewind(true)
            .scroll(true)
            .selected_line(0)
            .always_active()
            .highlight_str("▸");
        let name = Input::default().title("Name").value(suggested_name);
        let folder = Input::default()
            .title("Folder")
            .value(cwd.to_string_lossy().into_owned());
        let model = Input::default()
            .title("Model (empty = CLI default)")
            .placeholder("<cli default>");
        CreateDialog {
            harness,
            name,
            folder,
            model,
            focus: 0,
            base_cwd: cwd.to_path_buf(),
            error: None,
        }
    }

    pub fn focus(&self) -> usize {
        self.focus
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn harness_choice(&self) -> SessionKind {
        match self.harness.states.list_index {
            1 => SessionKind::Agent(Harness::Claude),
            2 => SessionKind::Agent(Harness::Codex),
            3 => SessionKind::Agent(Harness::Muse),
            _ => SessionKind::Shell,
        }
    }

    fn text_of(input: &Input) -> String {
        input_state(input).unwrap_or_default()
    }

    fn name_text(&self) -> String {
        Self::text_of(&self.name)
    }

    fn folder_text(&self) -> String {
        Self::text_of(&self.folder)
    }

    fn model_text(&self) -> String {
        Self::text_of(&self.model).trim().to_string()
    }

    /// Unchecked spec: `key` validates before submitting.
    pub fn spec(&self) -> SessionSpec {
        SessionSpec {
            kind: self.harness_choice(),
            name: self.name_text(),
            cwd: expand_folder(&self.folder_text(), &self.base_cwd),
            model: self.model_text(),
        }
    }

    fn submit(&mut self, live_names: &[String]) -> DialogOutcome {
        let name = self.name_text().trim().to_string();
        if name.is_empty() {
            self.error = Some("name must not be empty".to_string());
            return DialogOutcome::Pending;
        }
        if live_names.iter().any(|n| n == &name) {
            self.error = Some(format!("name {name:?} is already taken"));
            return DialogOutcome::Pending;
        }
        let cwd = expand_folder(&self.folder_text(), &self.base_cwd);
        if !cwd.is_dir() {
            self.error = Some(format!("{} is not a directory", cwd.display()));
            return DialogOutcome::Pending;
        }
        self.error = None;
        DialogOutcome::Submitted(SessionSpec {
            kind: self.harness_choice(),
            name,
            cwd,
            model: self.model_text(),
        })
    }

    /// One key: Tab/BackTab cycle, arrows drive the harness row or move
    /// focus, typing edits the focused text, Enter submits, Esc cancels.
    pub fn key(&mut self, key: &KeyEvent, live_names: &[String]) -> DialogOutcome {
        match key.code {
            KeyCode::Esc => return DialogOutcome::Cancelled,
            KeyCode::Enter => return self.submit(live_names),
            KeyCode::Tab => {
                self.focus = (self.focus + 1) % FIELD_COUNT;
                return DialogOutcome::Pending;
            }
            KeyCode::BackTab => {
                self.focus = (self.focus + FIELD_COUNT - 1) % FIELD_COUNT;
                return DialogOutcome::Pending;
            }
            _ => {}
        }
        if self.focus == FOCUS_HARNESS {
            match key.code {
                KeyCode::Up | KeyCode::Left => {
                    self.harness.perform(Cmd::Move(Direction::Up));
                }
                KeyCode::Down | KeyCode::Right => {
                    self.harness.perform(Cmd::Move(Direction::Down));
                }
                _ => {}
            }
            return DialogOutcome::Pending;
        }
        let field = match self.focus {
            1 => &mut self.name,
            2 => &mut self.folder,
            _ => &mut self.model,
        };
        match key.code {
            KeyCode::Up => {
                self.focus -= 1;
            }
            KeyCode::Down => {
                self.focus = (self.focus + 1) % FIELD_COUNT;
            }
            KeyCode::Left => {
                field.perform(Cmd::Move(Direction::Left));
            }
            KeyCode::Right => {
                field.perform(Cmd::Move(Direction::Right));
            }
            KeyCode::Backspace => {
                field.perform(Cmd::Delete);
                self.error = None;
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                field.perform(Cmd::Type(ch));
                self.error = None;
            }
            _ => {}
        }
        DialogOutcome::Pending
    }

    /// Render the centered dialog: labels plus each field's own view.
    pub fn view(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        use ratatui::widgets::{Block, Borders, Paragraph};
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" new session (Tab move • Enter create • Esc cancel) ")
            .style(crate::theme::style(crate::theme::Role::BorderFocused));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height < 11 || inner.width < 30 {
            return;
        }
        let label_w = 9u16;
        let field_x = inner.x + label_w;
        let field_w = inner.width.saturating_sub(label_w);
        let mut row = inner.y;
        label(frame, inner.x, row, label_w, " Harness");
        self.harness.view(
            frame,
            Rect::new(field_x, row, field_w, 4),
        );
        row += 4;
        for (text, field) in [
            (" Name", &mut self.name),
            (" Folder", &mut self.folder),
            (" Model", &mut self.model),
        ] {
            label(frame, inner.x, row, label_w, text);
            field.view(frame, Rect::new(field_x, row, field_w, 1));
            row += 1;
        }
        if let Some(err) = self.error.clone() {
            frame.render_widget(
                Paragraph::new(err).style(crate::theme::style(crate::theme::Role::Danger)),
                Rect::new(inner.x, row, inner.width, 1),
            );
            row += 1;
        }
        let _ = row;
    }
}

fn input_state(input: &Input) -> Option<String> {
    match Component::state(input) {
        State::Single(StateValue::String(s)) => Some(s),
        _ => None,
    }
}

fn label(frame: &mut ratatui::Frame, x: u16, row: u16, w: u16, text: &str) {
    use ratatui::widgets::Paragraph;
    frame.render_widget(Paragraph::new(text), Rect::new(x, row, w, 1));
}

/// Centered dialog box, clamped into tiny terminals.
pub fn create_area(term: Rect) -> Rect {
    let (w, h) = (64.min(term.width), 14.min(term.height));
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(dialog: &mut CreateDialog, text: &str, names: &[String]) {
        for ch in text.chars() {
            let outcome = dialog.key(&key(KeyCode::Char(ch)), names);
            assert!(matches!(outcome, DialogOutcome::Pending), "typing {ch:?}");
        }
    }

    fn fresh() -> (CreateDialog, Vec<String>) {
        let cwd = std::env::temp_dir();
        (CreateDialog::new("shell-1", &cwd), Vec::new())
    }

    #[test]
    fn defaults_select_shell_with_prefill() {
        let (d, _) = fresh();
        assert_eq!(d.harness_choice(), SessionKind::Shell);
        assert_eq!(d.spec().name, "shell-1");
        assert_eq!(d.spec().cwd, std::env::temp_dir());
        assert_eq!(d.spec().model, "");
        assert_eq!(d.error(), None);
    }

    #[test]
    fn tab_cycles_all_fields_and_wraps() {
        let (mut d, names) = fresh();
        assert_eq!(d.focus(), 0);
        for expect in [1, 2, 3, 0, 1] {
            assert!(matches!(d.key(&key(KeyCode::Tab), &names), DialogOutcome::Pending));
            assert_eq!(d.focus(), expect);
        }
        assert!(matches!(d.key(&key(KeyCode::BackTab), &names), DialogOutcome::Pending));
        assert_eq!(d.focus(), 0);
    }

    #[test]
    fn arrows_change_harness_on_its_row_and_move_focus_elsewhere() {
        let (mut d, names) = fresh();
        assert!(matches!(d.key(&key(KeyCode::Right), &names), DialogOutcome::Pending));
        assert_eq!(d.harness_choice(), SessionKind::Agent(crate::harness::Harness::Claude));
        assert!(matches!(d.key(&key(KeyCode::Left), &names), DialogOutcome::Pending));
        assert_eq!(d.harness_choice(), SessionKind::Shell);
        // Down off the harness row moves focus instead.
        assert!(matches!(d.key(&key(KeyCode::Tab), &names), DialogOutcome::Pending));
        assert!(matches!(d.key(&key(KeyCode::Down), &names), DialogOutcome::Pending));
        assert_eq!(d.focus(), 2);
        assert!(matches!(d.key(&key(KeyCode::Up), &names), DialogOutcome::Pending));
        assert_eq!(d.focus(), 1);
    }

    #[test]
    fn typing_edits_focused_text_and_backspace_deletes() {
        let (mut d, names) = fresh();
        assert!(matches!(d.key(&key(KeyCode::Tab), &names), DialogOutcome::Pending));
        // Clear the prefilled name first.
        for _ in 0.."shell-1".len() {
            assert!(matches!(
                d.key(&key(KeyCode::Backspace), &names),
                DialogOutcome::Pending
            ));
        }
        typed(&mut d, "agent-9", &names);
        assert_eq!(d.spec().name, "agent-9");
    }

    #[test]
    fn enter_submits_valid_spec() {
        let (mut d, names) = fresh();
        match d.key(&key(KeyCode::Enter), &names) {
            DialogOutcome::Submitted(spec) => {
                assert_eq!(spec.name, "shell-1");
                assert!(matches!(spec.kind, SessionKind::Shell));
            }
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn enter_with_bad_name_stays_open_with_error() {
        let (mut d, _) = fresh();
        // Duplicate name.
        let names = vec!["shell-1".to_string()];
        assert!(matches!(d.key(&key(KeyCode::Enter), &names), DialogOutcome::Pending));
        assert!(d.error().is_some_and(|e| e.contains("taken")), "err: {:?}", d.error());
        // Empty name.
        assert!(matches!(d.key(&key(KeyCode::Tab), &names), DialogOutcome::Pending));
        for _ in 0.."shell-1".len() {
            let _ = d.key(&key(KeyCode::Backspace), &names);
        }
        assert!(matches!(d.key(&key(KeyCode::Enter), &names), DialogOutcome::Pending));
        assert!(d.error().is_some_and(|e| e.contains("empty")), "err: {:?}", d.error());
    }

    #[test]
    fn enter_with_bad_folder_stays_open_with_error() {
        let (mut d, names) = fresh();
        assert!(matches!(d.key(&key(KeyCode::Tab), &names), DialogOutcome::Pending));
        assert!(matches!(d.key(&key(KeyCode::Tab), &names), DialogOutcome::Pending));
        assert_eq!(d.focus(), 2);
        for _ in 0..std::env::temp_dir().to_string_lossy().len() {
            let _ = d.key(&key(KeyCode::Backspace), &names);
        }
        typed(&mut d, "/no/such/dir-anywhere", &names);
        assert!(matches!(d.key(&key(KeyCode::Enter), &names), DialogOutcome::Pending));
        assert!(d.error().is_some_and(|e| e.contains("directory")), "err: {:?}", d.error());
    }

    #[test]
    fn escape_cancels_from_any_field() {
        let (mut d, names) = fresh();
        assert!(matches!(d.key(&key(KeyCode::Tab), &names), DialogOutcome::Pending));
        assert!(matches!(d.key(&key(KeyCode::Esc), &names), DialogOutcome::Cancelled));
    }

    #[test]
    fn folder_expansion_covers_tilde_and_relative() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let base = std::path::Path::new("/work");
        assert_eq!(
            expand_folder("~", base),
            std::path::PathBuf::from(&home),
        );
        assert_eq!(
            expand_folder("~/proj", base),
            std::path::PathBuf::from(&home).join("proj"),
        );
        assert_eq!(expand_folder("/abs/x", base), std::path::PathBuf::from("/abs/x"));
        assert_eq!(expand_folder("rel/y", base), base.join("rel/y"));
    }

    #[test]
    fn shell_join_quotes_argv_safely() {
        assert_eq!(shell_join(&["codex".to_string()]), "codex");
        assert_eq!(
            shell_join(&["/path/with space/codex".to_string(), "--model".to_string(), "gpt-5".to_string()]),
            "'/path/with space/codex' --model gpt-5"
        );
        assert_eq!(
            shell_join(&["a'b".to_string()]),
            "'a'\\''b'"
        );
    }
}
