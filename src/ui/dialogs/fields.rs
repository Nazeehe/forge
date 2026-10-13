//! Shared text-field key routing. Card-edit, create, and telegram
//! each hand-rolled the same block: Left/Right moves the cursor,
//! Backspace deletes, bare or Shift chars type. One owner means the
//! Shift guard can never drift between dialogs again.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_realm_stdlib::components::Input;
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
    fn unhandled_keys_report_not_consumed() {
        let mut field = Input::default();
        assert!(!field_key(&mut field, &key(KeyCode::Enter)));
        assert!(!field_key(&mut field, &key(KeyCode::Esc)));
        let ctrl = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(!field_key(&mut field, &ctrl));
    }
}
