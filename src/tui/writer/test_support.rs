//! Writer TUI test fixtures shared across the topic test modules.

use crate::app::AppState;
use crossterm::event;

pub(super) fn key(code: event::KeyCode) -> event::KeyEvent {
    event::KeyEvent::new(code, event::KeyModifiers::NONE)
}

pub(super) fn ctrl(code: event::KeyCode) -> event::KeyEvent {
    event::KeyEvent::new(code, event::KeyModifiers::CONTROL)
}

pub(super) fn tab() -> event::KeyEvent {
    key(event::KeyCode::Tab)
}

pub(super) fn ctrl_key(code: event::KeyCode) -> event::KeyEvent {
    event::KeyEvent::new(code, event::KeyModifiers::CONTROL)
}

pub(super) fn shift(code: event::KeyCode) -> event::KeyEvent {
    event::KeyEvent::new(code, event::KeyModifiers::SHIFT)
}

pub(super) fn editor_area(state: &AppState, id: crate::session::SessionId) -> ratatui::layout::Rect {
    let (rows, cols) = state.term_size;
    let area = crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, cols, rows));
    let visible = state.writers.get(&id).is_some_and(|s| s.panel_visible);
    crate::ui::writer::writer_layout(area, visible).editor
}

pub(super) fn panel_text(
    buf: &ratatui::buffer::Buffer,
    layout: crate::ui::writer::WriterLayout,
) -> String {
    let mut s = String::new();
    for y in layout.panel.y..layout.panel.y + layout.panel.height {
        for x in layout.panel.x..layout.panel.x + layout.panel.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

pub(super) fn doc_text(state: &AppState, id: crate::session::SessionId) -> String {
    state.writers.get(&id).unwrap().doc.as_ref().unwrap().text.clone()
}

pub(super) fn cursor_offset(state: &AppState, id: crate::session::SessionId) -> usize {
    let session = state.writers.get(&id).unwrap();
    crate::app::writer::adapter::editor_cursor_offset(session.editor.as_ref().unwrap())
}

pub(super) fn shift_select(state: &mut AppState, id: crate::session::SessionId, count: usize) {
    let shift = event::KeyModifiers::SHIFT;
    for _ in 0..count {
        state.writer_feed_key(
            id,
            event::KeyEvent::new(event::KeyCode::Right, shift),
        );
    }
}

static TUI_WRITER_TMP_SEQ: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub(super) fn tui_writer_tmp_dir() -> std::path::PathBuf {
    // Same burst-collision guard as app::writer::test_support:
    // time-only names can repeat across parallel spawns.
    std::env::temp_dir().join(format!(
        "forge-tui-writer-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0),
        TUI_WRITER_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    ))
}

#[test]
fn tui_tmp_dirs_stay_unique_under_bursts() {
    let mut dirs = std::collections::HashSet::new();
    for _ in 0..500 {
        assert!(dirs.insert(tui_writer_tmp_dir()), "scratch dir repeated");
    }
}

pub(super) fn writer_agent() -> (AppState, crate::session::SessionId, std::path::PathBuf) {
    let dir = tui_writer_tmp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut state = AppState::new();
    let id = state
        .manager
        .spawn_agent(
            "agent",
            &dir,
            "exec cat",
            crate::infra::ids::RunId::generate(),
            "codex",
        )
        .unwrap();
    (state, id, dir)
}

pub(super) fn open_doc(state: &mut AppState, id: crate::session::SessionId, name: &str) {
    // Default text only when the test did not pre-write the file.
    let path = state.manager.get(id).unwrap().cwd.join(name);
    if !path.exists() {
        std::fs::write(&path, "aaa bbb").unwrap();
    }
    let doc =
        crate::writer::document::Document::open(&state.manager.get(id).unwrap().cwd, name)
            .unwrap();
    let session = state.writers.entry(id).or_default();
    session.doc = Some(doc);
    state.writer_open_editor(id);
}

pub(super) fn click_at(x: u16, y: u16) -> event::MouseEvent {
    event::MouseEvent {
        kind: event::MouseEventKind::Down(event::MouseButton::Left),
        column: x,
        row: y,
        modifiers: event::KeyModifiers::NONE,
    }
}

pub(super) fn find_text(buf: &ratatui::buffer::Buffer, needle: &str) -> (u16, u16) {
    find_all_text(buf, needle)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("needle {needle:?} not painted"))
}

pub(super) fn find_all_text(buf: &ratatui::buffer::Buffer, needle: &str) -> Vec<(u16, u16)> {
    let mut hits = Vec::new();
    for y in 0..buf.area.height {
        let row: String = (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect();
        let mut rest = row.as_str();
        let mut offset = 0usize;
        while let Some(byte) = rest.find(needle) {
            let x = (offset + row[offset..offset + byte].chars().count()) as u16;
            hits.push((x, y));
            let step = byte + needle.len();
            offset += step;
            rest = &rest[step..];
        }
    }
    hits
}

/// Paint the overlay exactly like the draw closure, so clicks read
/// off the buffer land where the handler looks.
pub(super) fn paint_full(
    state: &mut AppState,
    id: crate::session::SessionId,
) -> ratatui::buffer::Buffer {
    use ratatui::{backend::TestBackend, Terminal};
    let (rows, cols) = state.term_size;
    let area = crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, cols, rows));
    let activity = state.manager.get(id).map(|rec| rec.activity).unwrap();
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
    terminal
        .draw(|f| {
            crate::ui::writer::paint(f, area, state.writers.get_mut(&id).unwrap(), activity, cols);
        })
        .unwrap();
    terminal.backend().buffer().clone()
}
