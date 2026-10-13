//! Shared text-field key routing. Card-edit, create, and telegram
//! each hand-rolled the same block: Left/Right moves the cursor,
//! Backspace deletes, bare or Shift chars type. One owner means the
//! Shift guard can never drift between dialogs again.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_realm_stdlib::components::{Input, Radio, Select};
use tuirealm::command::{Cmd, Direction};
use tuirealm::component::Component;

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

/// Type a whole string (e.g. bracketed paste) one char at a time,
/// like typing. Card-edit and telegram duplicated this loop.
pub fn type_text(field: &mut Input, text: &str) {
    for ch in text.chars() {
        field.perform(Cmd::Type(ch));
    }
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
    use tuirealm::state::{State, StateValue};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn text_of(input: &Input) -> String {
        match Component::state(input) {
            State::Single(StateValue::String(text)) => text,
            _ => String::new(),
        }
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
    fn type_text_inserts_whole_string() {
        let mut field = Input::default().value("ab".to_string());
        type_text(&mut field, "cd");
        assert_eq!(text_of(&field), "abcd");
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
