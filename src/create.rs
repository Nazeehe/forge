//! Create-session dialog: the New Session form — directory, name, model,
//! option rows (internet, CLI tool, connection, comm group), and a
//! Create/Cancel action row. Every visible element is a tui-realm
//! component (inputs, radios, selects, labels); the dialog itself only
//! routes keys and lays their views out. Enter submits a validated
//! [`SessionSpec`]; Escape cancels. Validation rejects empty/duplicate
//! names and non-directory folders without closing.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tui_realm_stdlib::components::{Input, Label, Radio, Select};
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

/// Validated dialog output: everything `create_session` needs, plus the
/// comm group to join (`None` keeps the session groupless).
#[derive(Clone, Debug)]
pub struct SessionSpec {
    pub kind: SessionKind,
    pub name: String,
    pub cwd: PathBuf,
    pub model: String,
    pub group: Option<String>,
}

/// Dialog result after one input: still open, cancelled, or submitted.
#[derive(Debug)]
pub enum DialogOutcome {
    Pending,
    Cancelled,
    Submitted(SessionSpec),
}

/// Focus rows in Tab order.
const FOCUS_DIRECTORY: usize = 0;
const FOCUS_NAME: usize = 1;
const FOCUS_MODEL: usize = 2;
const FOCUS_INTERNET: usize = 3;
const FOCUS_TOOL: usize = 4;
const FOCUS_CONNECTION: usize = 5;
const FOCUS_GROUP: usize = 6;
const FOCUS_ACTIONS: usize = 7;
const FIELD_COUNT: usize = 8;

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

/// New-session dialog: three text inputs, four option rows, and an action
/// row. Tab cycles rows, Left/Right changes the focused option, typing
/// edits the focused text, Enter submits, Esc cancels.
pub struct CreateDialog {
    directory: Input,
    name: Input,
    model: Input,
    internet: Radio,
    tool: Radio,
    connection: Radio,
    group: Select,
    actions: Radio,
    focus: usize,
    base_cwd: PathBuf,
    error: Option<String>,
}

impl CreateDialog {
    /// `groups` are the live broker group names; the select offers them
    /// after a leading `None` (groupless).
    pub fn new(suggested_name: &str, cwd: &Path, groups: &[String]) -> Self {
        let directory = Input::default()
            .title("Directory")
            .value(cwd.to_string_lossy().into_owned());
        let name = Input::default().title("Name").value(suggested_name);
        let model = Input::default()
            .title("Model (empty = CLI default)")
            .placeholder("<cli default>");
        let internet = Radio::default()
            .choices(["OFF", "ON"])
            .value(0)
            .rewind(true);
        // Agent CLIs only: forge never creates a bare shell session.
        let tool = Radio::default()
            .choices(["claude", "codex", "muse"])
            .value(0)
            .rewind(true);
        let connection = Radio::default().choices(["Local"]).value(0);
        let mut group_choices = vec!["None".to_string()];
        group_choices.extend(groups.iter().cloned());
        let group = Select::default()
            .choices(group_choices)
            .value(0)
            .rewind(true);
        let actions = Radio::default()
            .choices(["Create", "Cancel"])
            .value(0)
            .rewind(true);
        CreateDialog {
            directory,
            name,
            model,
            internet,
            tool,
            connection,
            group,
            actions,
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

    pub fn tool_choice(&self) -> SessionKind {
        match self.tool.states.choice {
            1 => SessionKind::Agent(Harness::Codex),
            2 => SessionKind::Agent(Harness::Muse),
            _ => SessionKind::Agent(Harness::Claude),
        }
    }

    /// Comm group choice: the leading `None` entry keeps the session
    /// groupless, anything else names a live broker group.
    pub fn group_choice(&self) -> Option<String> {
        match self.group.states.selected {
            0 => None,
            i => self.group.states.choices.get(i).and_then(|choice| {
                (choice != "None").then(|| choice.clone())
            }),
        }
    }

    fn text_of(input: &Input) -> String {
        input_state(input).unwrap_or_default()
    }

    fn directory_text(&self) -> String {
        Self::text_of(&self.directory)
    }

    fn name_text(&self) -> String {
        Self::text_of(&self.name)
    }

    fn model_text(&self) -> String {
        Self::text_of(&self.model).trim().to_string()
    }

    /// Unchecked spec: `key` validates before submitting.
    pub fn spec(&self) -> SessionSpec {
        SessionSpec {
            kind: self.tool_choice(),
            name: self.name_text(),
            cwd: expand_folder(&self.directory_text(), &self.base_cwd),
            model: self.model_text(),
            group: self.group_choice(),
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
        let cwd = expand_folder(&self.directory_text(), &self.base_cwd);
        if !cwd.is_dir() {
            self.error = Some(format!("{} is not a directory", cwd.display()));
            return DialogOutcome::Pending;
        }
        self.error = None;
        let mut spec = self.spec();
        spec.name = name;
        spec.cwd = cwd;
        DialogOutcome::Submitted(spec)
    }

    /// One key: Tab/BackTab cycle rows, Left/Right changes the focused
    /// option row (or moves the text cursor), typing edits the focused
    /// text, Enter submits (or activates the focused action), Esc cancels.
    pub fn key(&mut self, key: &KeyEvent, live_names: &[String]) -> DialogOutcome {
        match key.code {
            KeyCode::Esc => return DialogOutcome::Cancelled,
            KeyCode::Tab => {
                self.focus = (self.focus + 1) % FIELD_COUNT;
                return DialogOutcome::Pending;
            }
            KeyCode::BackTab => {
                self.focus = (self.focus + FIELD_COUNT - 1) % FIELD_COUNT;
                return DialogOutcome::Pending;
            }
            KeyCode::Enter => {
                if self.focus == FOCUS_ACTIONS {
                    return match self.actions.states.choice {
                        0 => self.submit(live_names),
                        _ => DialogOutcome::Cancelled,
                    };
                }
                return self.submit(live_names);
            }
            KeyCode::Up => {
                self.focus = (self.focus + FIELD_COUNT - 1) % FIELD_COUNT;
                return DialogOutcome::Pending;
            }
            KeyCode::Down => {
                self.focus = (self.focus + 1) % FIELD_COUNT;
                return DialogOutcome::Pending;
            }
            _ => {}
        }
        match self.focus {
            FOCUS_INTERNET => {
                option_key(&mut self.internet, key);
            }
            FOCUS_TOOL => {
                option_key(&mut self.tool, key);
            }
            FOCUS_CONNECTION => {
                option_key(&mut self.connection, key);
            }
            // The select's popup stays closed (one-row cycler look), so
            // cycle its public selection index directly.
            FOCUS_GROUP => match key.code {
                KeyCode::Left => {
                    cycle_group(&mut self.group, -1);
                }
                KeyCode::Right => {
                    cycle_group(&mut self.group, 1);
                }
                _ => {}
            },
            FOCUS_ACTIONS => {
                option_key(&mut self.actions, key);
            }
            _ => {
                let field = match self.focus {
                    FOCUS_DIRECTORY => &mut self.directory,
                    FOCUS_NAME => &mut self.name,
                    _ => &mut self.model,
                };
                match key.code {
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
            }
        }
        DialogOutcome::Pending
    }
}

/// Cycle the group select with wraparound; empty choice lists are a no-op.
fn cycle_group(select: &mut Select, dir: i32) {
    let len = select.states.choices.len() as i32;
    if len == 0 {
        return;
    }
    let next = (select.states.selected as i32 + dir).rem_euclid(len) as usize;
    select.states.select(next);
}

/// Left/Right drives a radio row; anything else is ignored there.
fn option_key(radio: &mut Radio, key: &KeyEvent) {
    match key.code {
        KeyCode::Left => {
            radio.perform(Cmd::Move(Direction::Left));
        }
        KeyCode::Right => {
            radio.perform(Cmd::Move(Direction::Right));
        }
        _ => {}
    }
}

impl CreateDialog {
    /// Render the New Session form: a focus marker plus label per row,
    /// each control drawn through its own tui-realm view.
    pub fn view(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        use ratatui::style::Color;
        use ratatui::widgets::{Block, Borders};
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" New Session ")
            .style(crate::theme::style(crate::theme::Role::BorderFocused));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height < 12 || inner.width < 44 {
            return;
        }
        let end = inner.y + inner.height;
        let mut row = inner.y;
        // Marker column plus label column; controls start after both.
        let marker_w = 2u16;
        let label_w = 11u16;
        let field_x = inner.x + marker_w + label_w;
        let field_w = inner.width.saturating_sub(marker_w + label_w);
        let labels = [
            "Directory",
            "Name",
            "Model",
            "Internet",
            "CLI Tool",
            "Connection",
            "Comm Group",
            "",
        ];
        for index in 0..FIELD_COUNT {
            if row >= end {
                return;
            }
            let focused = index == self.focus;
            let mark = if focused { "▸" } else { " " };
            Label::default()
                .text(format!("{mark} "))
                .view(frame, Rect::new(inner.x, row, marker_w, 1));
            Label::default()
                .text(labels[index].to_string())
                .view(frame, Rect::new(inner.x + marker_w, row, label_w, 1));
            match index {
                FOCUS_DIRECTORY => self.directory.view(frame, Rect::new(field_x, row, field_w, 1)),
                FOCUS_NAME => self.name.view(frame, Rect::new(field_x, row, field_w, 1)),
                FOCUS_MODEL => self.model.view(frame, Rect::new(field_x, row, field_w, 1)),
                FOCUS_INTERNET => self.internet.view(frame, Rect::new(field_x, row, field_w, 1)),
                FOCUS_TOOL => self.tool.view(frame, Rect::new(field_x, row, field_w, 1)),
                FOCUS_CONNECTION => {
                    self.connection.view(frame, Rect::new(field_x, row, field_w, 1));
                }
                FOCUS_GROUP => {
                    // Cycler markers hug the selected text: size the select
                    // to its content so ▶ never floats at the area edge.
                    let selected_len = self
                        .group
                        .states
                        .choices
                        .get(self.group.states.selected)
                        .map(|choice| choice.len() as u16)
                        .unwrap_or(4)
                        .max(4);
                    Label::default()
                        .text("◀ ")
                        .view(frame, Rect::new(field_x, row, 2, 1));
                    self.group.view(
                        frame,
                        Rect::new(field_x + 2, row, selected_len + 1, 1),
                    );
                    Label::default().text("▶").view(
                        frame,
                        Rect::new(field_x + 3 + selected_len, row, 1, 1),
                    );
                }
                _ => self.actions.view(frame, Rect::new(field_x, row, field_w, 1)),
            }
            row += 1;
            // Breathing room: after the text block and after the options.
            if index == FOCUS_MODEL || index == FOCUS_GROUP {
                row += 1;
            }
        }
        if let Some(err) = self.error.clone() {
            if row < end {
                Label::default()
                    .text(err)
                    .foreground(Color::Red)
                    .view(frame, Rect::new(inner.x, row, inner.width, 1));
                row += 1;
            }
        }
        if row < end {
            Label::default()
                .text("Tab move • Left/Right change • Enter create • Esc cancel".to_string())
                .view(frame, Rect::new(inner.x, row, inner.width, 1));
        }
    }
}

fn input_state(input: &Input) -> Option<String> {
    match Component::state(input) {
        State::Single(StateValue::String(s)) => Some(s),
        _ => None,
    }
}

/// Centered dialog box, wider and taller than the old form for the
/// option rows, clamped into tiny terminals.
pub fn create_area(term: Rect) -> Rect {
    let (w, h) = (78.min(term.width), 15.min(term.height));
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

    fn tab(dialog: &mut CreateDialog, names: &[String], n: usize) {
        for _ in 0..n {
            assert!(matches!(dialog.key(&key(KeyCode::Tab), names), DialogOutcome::Pending));
        }
    }

    fn fresh() -> (CreateDialog, Vec<String>) {
        let cwd = std::env::temp_dir();
        let groups = vec!["team".to_string(), "other".to_string()];
        (CreateDialog::new("claude-1", &cwd, &groups), Vec::new())
    }

    #[test]
    fn defaults_prefill_claude_and_none_group() {
        let (d, _) = fresh();
        assert_eq!(d.tool_choice(), SessionKind::Agent(crate::harness::Harness::Claude));
        assert_eq!(d.spec().name, "claude-1");
        assert_eq!(d.spec().cwd, std::env::temp_dir());
        assert_eq!(d.spec().model, "");
        assert_eq!(d.group_choice(), None);
        assert_eq!(d.error(), None);
        assert_eq!(d.focus(), 0);
    }

    #[test]
    fn tab_cycles_all_rows_and_wraps() {
        let (mut d, names) = fresh();
        for expect in [1, 2, 3, 4, 5, 6, 7, 0, 1] {
            tab(&mut d, &names, 1);
            assert_eq!(d.focus(), expect);
        }
        assert!(matches!(d.key(&key(KeyCode::BackTab), &names), DialogOutcome::Pending));
        assert_eq!(d.focus(), 0);
    }

    #[test]
    fn arrows_move_focus_everywhere() {
        let (mut d, names) = fresh();
        assert!(matches!(d.key(&key(KeyCode::Down), &names), DialogOutcome::Pending));
        assert_eq!(d.focus(), 1);
        assert!(matches!(d.key(&key(KeyCode::Up), &names), DialogOutcome::Pending));
        assert_eq!(d.focus(), 0);
        // Up from the first row wraps to actions.
        assert!(matches!(d.key(&key(KeyCode::Up), &names), DialogOutcome::Pending));
        assert_eq!(d.focus(), 7);
    }

    #[test]
    fn left_right_drives_tool_radio_and_rewinds() {
        let (mut d, names) = fresh();
        tab(&mut d, &names, 4);
        assert_eq!(d.focus(), 4);
        assert!(matches!(d.key(&key(KeyCode::Right), &names), DialogOutcome::Pending));
        assert_eq!(d.tool_choice(), SessionKind::Agent(crate::harness::Harness::Codex));
        assert!(matches!(d.key(&key(KeyCode::Right), &names), DialogOutcome::Pending));
        assert_eq!(d.tool_choice(), SessionKind::Agent(crate::harness::Harness::Muse));
        // Rewind wraps back to claude.
        assert!(matches!(d.key(&key(KeyCode::Right), &names), DialogOutcome::Pending));
        assert_eq!(d.tool_choice(), SessionKind::Agent(crate::harness::Harness::Claude));
        assert!(matches!(d.key(&key(KeyCode::Left), &names), DialogOutcome::Pending));
        assert_eq!(d.tool_choice(), SessionKind::Agent(crate::harness::Harness::Muse));
    }

    #[test]
    fn internet_toggles_visually_but_connection_is_fixed() {
        let (mut d, names) = fresh();
        tab(&mut d, &names, 3);
        assert!(matches!(d.key(&key(KeyCode::Right), &names), DialogOutcome::Pending));
        assert_eq!(d.internet.states.choice, 1, "ON shows");
        assert!(matches!(d.key(&key(KeyCode::Left), &names), DialogOutcome::Pending));
        assert_eq!(d.internet.states.choice, 0, "back to OFF");
        tab(&mut d, &names, 2);
        assert_eq!(d.focus(), 5);
        assert!(matches!(d.key(&key(KeyCode::Right), &names), DialogOutcome::Pending));
        assert_eq!(d.connection.states.choice, 0, "Local is the only choice");
    }

    #[test]
    fn group_select_cycles_none_and_groups() {
        let (mut d, names) = fresh();
        tab(&mut d, &names, 6);
        assert_eq!(d.group_choice(), None);
        assert!(matches!(d.key(&key(KeyCode::Right), &names), DialogOutcome::Pending));
        assert_eq!(d.group_choice(), Some("team".to_string()));
        assert!(matches!(d.key(&key(KeyCode::Right), &names), DialogOutcome::Pending));
        assert_eq!(d.group_choice(), Some("other".to_string()));
        // Rewind wraps to None.
        assert!(matches!(d.key(&key(KeyCode::Right), &names), DialogOutcome::Pending));
        assert_eq!(d.group_choice(), None);
        assert!(matches!(d.key(&key(KeyCode::Left), &names), DialogOutcome::Pending));
        assert_eq!(d.group_choice(), Some("other".to_string()));
    }

    #[test]
    fn actions_create_submits_with_group() {
        let (mut d, names) = fresh();
        tab(&mut d, &names, 6);
        assert!(matches!(d.key(&key(KeyCode::Right), &names), DialogOutcome::Pending));
        tab(&mut d, &names, 1);
        assert_eq!(d.focus(), 7);
        match d.key(&key(KeyCode::Enter), &names) {
            DialogOutcome::Submitted(spec) => {
                assert_eq!(spec.name, "claude-1");
                assert!(matches!(spec.kind, SessionKind::Agent(_)));
                assert_eq!(spec.group, Some("team".to_string()));
            }
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn actions_cancel_cancels_from_button_row() {
        let (mut d, names) = fresh();
        tab(&mut d, &names, 7);
        assert!(matches!(d.key(&key(KeyCode::Right), &names), DialogOutcome::Pending));
        assert!(matches!(d.key(&key(KeyCode::Enter), &names), DialogOutcome::Cancelled));
    }

    #[test]
    fn enter_submits_from_text_rows() {
        let (mut d, names) = fresh();
        match d.key(&key(KeyCode::Enter), &names) {
            DialogOutcome::Submitted(spec) => {
                assert_eq!(spec.name, "claude-1");
                assert_eq!(spec.group, None);
            }
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn typing_edits_name_and_backspace_deletes() {
        let (mut d, names) = fresh();
        tab(&mut d, &names, 1);
        for _ in 0.."claude-1".len() {
            assert!(matches!(
                d.key(&key(KeyCode::Backspace), &names),
                DialogOutcome::Pending
            ));
        }
        typed(&mut d, "agent-9", &names);
        assert_eq!(d.spec().name, "agent-9");
    }

    #[test]
    fn enter_with_bad_name_stays_open_with_error() {
        let (mut d, _) = fresh();
        // Duplicate name.
        let names = vec!["claude-1".to_string()];
        assert!(matches!(d.key(&key(KeyCode::Enter), &names), DialogOutcome::Pending));
        assert!(d.error().is_some_and(|e| e.contains("taken")), "err: {:?}", d.error());
        // Empty name.
        tab(&mut d, &names, 1);
        for _ in 0.."claude-1".len() {
            let _ = d.key(&key(KeyCode::Backspace), &names);
        }
        assert!(matches!(d.key(&key(KeyCode::Enter), &names), DialogOutcome::Pending));
        assert!(d.error().is_some_and(|e| e.contains("empty")), "err: {:?}", d.error());
    }

    #[test]
    fn enter_with_bad_directory_stays_open_with_error() {
        let (mut d, names) = fresh();
        for _ in 0..std::env::temp_dir().to_string_lossy().len() {
            let _ = d.key(&key(KeyCode::Backspace), &names);
        }
        typed(&mut d, "/no/such/dir-anywhere", &names);
        assert!(matches!(d.key(&key(KeyCode::Enter), &names), DialogOutcome::Pending));
        assert!(d.error().is_some_and(|e| e.contains("directory")), "err: {:?}", d.error());
    }

    #[test]
    fn escape_cancels_from_any_row() {
        let (mut d, names) = fresh();
        tab(&mut d, &names, 5);
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
