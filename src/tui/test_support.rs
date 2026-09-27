//! Shared TUI test fixtures: dialog geometry readers.

use ratatui::Terminal;

use crate::app::AppState;

/// Cell column of `needle` on the modal action row, read back
/// from a real paint so the click tests real geometry.
pub(super) fn telegram_button_cell(state: &mut AppState, needle: &str) -> (u16, u16) {
    use ratatui::backend::TestBackend;
    let area = crate::ui::dialogs::telegram::telegram_area(ratatui::layout::Rect::new(0, 0, 180, 40));
    let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
    if let Some(dialog) = state.telegram_dialog.as_mut() {
        terminal.draw(|f| dialog.view(f, area)).unwrap();
    }
    let buf = terminal.backend().buffer();
    let y = area.y + 2 + 7;
    let cells: Vec<String> = (area.x..area.x + area.width)
        .map(|x| buf[(x, y)].symbol().to_string())
        .collect();
    let start = (0..cells.len())
        .find(|&i| cells[i..].concat().starts_with(needle))
        .unwrap_or_else(|| panic!("{needle:?} visible: {:?}", cells.concat()));
    (area.x + start as u16 + (needle.chars().count() as u16) / 2, y)
}
