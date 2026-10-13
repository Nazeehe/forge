//! Shared text-field key routing. Card-edit, create, and telegram
//! each hand-rolled the same block: Left/Right moves the cursor,
//! Backspace deletes, bare or Shift chars type. One owner means the
//! Shift guard can never drift between dialogs again.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use tui_realm_stdlib::components::{Input, Radio, Select};

use crate::ui::theme::{Role, focus_row, style};
use tuirealm::command::{Cmd, Direction};
use tuirealm::component::Component;
use tuirealm::state::{State, StateValue};

/// Route one key into a text field. Returns true only when the key
/// may have changed the text (delete/type), so callers clear their
/// own error/test slots on real edits; cursor moves are silent.
pub fn field_key(field: &mut Input, key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Left => {
            field.perform(Cmd::Move(Direction::Left));
            false
        }
        KeyCode::Right => {
            field.perform(Cmd::Move(Direction::Right));
            false
        }
        KeyCode::Backspace => {
            field.perform(Cmd::Delete);
            true
        }
        KeyCode::Char(ch)
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
        {
            field.perform(Cmd::Type(ch));
            true
        }
        _ => false,
    }
}

/// Cycle a closed select with wraparound; empty choice lists are a no-op.
/// Card-edit, create, and telegram carried identical copies.
pub fn cycle_select(select: &mut Select, dir: i32) {
    let len = select.states.choices.len() as i32;
    if len == 0 {
        return;
    }
    let next = (select.states.selected as i32 + dir).rem_euclid(len) as usize;
    select.states.select(next);
}

/// Read an input's current text; non-string states read as empty.
/// Card-edit, create, and telegram each owned a copy (`text_of`,
/// create spelling it via `input_state().unwrap_or_default()`).
pub fn input_text(input: &Input) -> String {
    match Component::state(input) {
        State::Single(StateValue::String(text)) => text,
        _ => String::new(),
    }
}

/// One labeled form row: `>` marker plus padded label, then the
/// caller's control spans. Card-edit and telegram painted this
/// inline with identical closures.
pub fn form_line(label: &str, focused: bool, control: Vec<Span<'static>>) -> Line<'static> {
    let text = style(Role::Text);
    let row_style = if focused { focus_row() } else { text };
    Line::from(
        std::iter::once(if focused {
            Span::styled("> ", focus_row())
        } else {
            Span::raw("  ")
        })
        .chain(std::iter::once(Span::styled(
            format!("{label:<11}"),
            row_style,
        )))
        .chain(control)
        .collect::<Vec<_>>(),
    )
}

/// `[value]` box, padded to fill; empty shows the dim placeholder.
/// `content_w` is the full content width the dialog computed.
pub fn form_field(
    value: &str,
    focused: bool,
    placeholder: &str,
    content_w: usize,
) -> Vec<Span<'static>> {
    let text = style(Role::Text);
    let room = content_w.saturating_sub(2);
    let shown = if value.is_empty() { placeholder } else { value };
    let cut = crate::ui::text::cut_chars(shown, room);
    let pad = room.saturating_sub(cut.chars().count());
    if focused {
        vec![Span::styled(format!("[{cut}{}]", " ".repeat(pad)), focus_row())]
    } else if value.is_empty() {
        vec![
            Span::styled("[", text),
            Span::styled(cut, style(Role::Muted)),
            Span::styled(format!("{}]", " ".repeat(pad)), text),
        ]
    } else {
        vec![Span::styled(format!("[{cut}{}]", " ".repeat(pad)), text)]
    }
}

/// `< value >` cycler row.
pub fn form_select(value: &str, focused: bool, content_w: usize) -> Vec<Span<'static>> {
    let text = style(Role::Text);
    let cut = crate::ui::text::cut_chars(value, content_w.saturating_sub(4));
    let chev = style(Role::Muted);
    vec![
        Span::styled("< ", if focused { focus_row() } else { chev }),
        Span::styled(cut, if focused { focus_row() } else { text }),
        Span::styled(" >", if focused { focus_row() } else { chev }),
    ]
}

/// Type a whole string (e.g. bracketed paste) one char at a time,
/// like typing. Card-edit and telegram duplicated this loop.
pub fn type_text(field: &mut Input, text: &str) {
    for ch in text.chars() {
        field.perform(Cmd::Type(ch));
    }
}

/// The selected choice's label, or `fallback` when the list is
/// empty. Card-edit owned this (`selected`); create and telegram
/// inlined it with per-site fallbacks.
pub fn selected_label(select: &Select, fallback: &str) -> String {
    select
        .states
        .choices
        .get(select.states.selected)
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

/// Left/Right drives a radio row; anything else is ignored there.
/// Card-edit and telegram inlined this; create named it `option_key`.
pub fn option_key(radio: &mut Radio, key: &KeyEvent) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn text_of(input: &Input) -> String {
        input_text(input)
    }

    #[test]
    fn typing_appends_and_reports_consumed() {
        let mut field = Input::default();
        assert!(field_key(&mut field, &key(KeyCode::Char('h'))));
        assert!(field_key(&mut field, &key(KeyCode::Char('i'))));
        assert_eq!(text_of(&field), "hi");
    }

    #[test]
    fn backspace_deletes_and_arrows_move_silently() {
        let mut field = Input::default().value("ab".to_string());
        assert!(field_key(&mut field, &key(KeyCode::Backspace)));
        assert_eq!(text_of(&field), "a");
        // Arrows report "not an edit" but still move: Left then
        // Backspace eats the char before the cursor.
        let mut field = Input::default().value("ab".to_string());
        assert!(!field_key(&mut field, &key(KeyCode::Left)));
        assert!(field_key(&mut field, &key(KeyCode::Backspace)));
        assert_eq!(text_of(&field), "b");
    }

    #[test]
    fn select_cycles_with_wraparound() {
        let mut select =
            Select::default().choices(["a", "b", "c"]).value(0).rewind(true);
        cycle_select(&mut select, 1);
        assert_eq!(select.states.selected, 1);
        cycle_select(&mut select, -1);
        assert_eq!(select.states.selected, 0);
        cycle_select(&mut select, -1);
        assert_eq!(select.states.selected, 2);
    }

    #[test]
    fn select_cycle_empty_is_noop() {
        let mut select = Select::default();
        cycle_select(&mut select, 1);
        assert_eq!(select.states.selected, 0);
    }

    #[test]
    fn radio_arrows_move_choice_others_ignored() {
        let mut radio =
            Radio::default().choices(["Save", "Cancel"]).value(0).rewind(true);
        option_key(&mut radio, &key(KeyCode::Right));
        assert_eq!(radio.states.choice, 1);
        option_key(&mut radio, &key(KeyCode::Left));
        assert_eq!(radio.states.choice, 0);
        option_key(&mut radio, &key(KeyCode::Enter));
        assert_eq!(radio.states.choice, 0);
    }

    #[test]
    fn input_text_reads_field_value() {
        let field = Input::default().value("hello".to_string());
        assert_eq!(input_text(&field), "hello");
        assert_eq!(input_text(&Input::default()), "");
    }

    #[test]
    fn type_text_inserts_whole_string() {
        let mut field = Input::default().value("ab".to_string());
        type_text(&mut field, "cd");
        assert_eq!(text_of(&field), "abcd");
    }

    #[test]
    fn form_field_focused_wraps_value_in_brackets() {
        let spans = form_field("ab", true, "ph", 10);
        assert_eq!(spans.len(), 1);
        assert!(spans[0].content.starts_with("[ab"));
        assert_eq!(spans[0].style, crate::ui::theme::focus_row());
    }

    #[test]
    fn form_field_empty_unfocused_shows_muted_placeholder() {
        use crate::ui::theme::{Role, style};
        let spans = form_field("", false, "ph", 10);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].content, "ph");
        assert_eq!(spans[1].style, style(Role::Muted));
    }

    #[test]
    fn form_select_focused_chevrons_and_value() {
        let spans = form_select("On", true, 10);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "< ");
        assert_eq!(spans[1].content, "On");
        assert_eq!(spans[0].style, crate::ui::theme::focus_row());
    }

    #[test]
    fn form_line_focused_marks_row_and_pads_label() {
        let line = form_line("Title", true, vec![]);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with("> Title"));
    }

    #[test]
    fn selected_label_reads_choice_or_falls_back() {
        let select = Select::default().choices(["a", "b"]).value(1).rewind(true);
        assert_eq!(selected_label(&select, "?"), "b");
        assert_eq!(selected_label(&Select::default(), "?"), "?");
    }

    #[test]
    fn unhandled_keys_report_not_consumed() {
        let mut field = Input::default();
        assert!(!field_key(&mut field, &key(KeyCode::Enter)));
        assert!(!field_key(&mut field, &key(KeyCode::Esc)));
        let ctrl = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(!field_key(&mut field, &ctrl));
    }
}
