//! TUI mouse: modal-swallow dispatch, chrome hit-testing, pane forwarding.

use crossterm::event;

use crate::app::AppState;
use crate::ui;

use super::fit_active_pane;
use super::input;
#[cfg(feature = "visual")]
use super::visual::visual_viewport;

/// Route an outer mouse event: session-bar clicks switch sessions, the
/// main area forwards to the active pane when it wants mouse reporting,
/// and everything else (sidebar, status bar, borders) is chrome-owned.
pub(super) fn forward_mouse(state: &mut AppState, mev: event::MouseEvent) {
    // Modals own mouse input. Only a left press inside the group dialog
    // reaches its List/Checkbox rows; clicks behind it do nothing.
    // The restore picker is keyboard-only: every click dies here.
    // The first-run dialog owns its mouse via handle_oobe_mouse (like
    // Telegram settings); anything reaching here dies.
    if state.oobe_dialog.is_some() {
        return;
    }
    if state.restore_picker.is_some() {
        return;
    }
    if state.group_dialog.is_some() {
        if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left)) {
            let (rows, cols) = state.term_size;
            let area = crate::ui::dialogs::groups::group_area(ratatui::layout::Rect::new(0, 0, cols, rows));
            if mev.column >= area.x && mev.column < area.right()
                && mev.row >= area.y && mev.row < area.bottom()
            {
                let ctx = state.group_ctx();
                if let Some(dialog) = state.group_dialog.as_mut() {
                    if dialog.click(mev.column, mev.row, area, &ctx) {
                        state.dirty = true;
                    }
                }
            }
        }
        return;
    }
    if state.create_dialog.is_some() {
        return;
    }
    // The theme picker owns its mouse via handle_theme_mouse (like
    // Telegram settings); anything reaching here dies.
    if state.theme_dialog.is_some() {
        return;
    }
    if state.confirm.is_some() {
        return;
    }
    let (rows, cols) = state.term_size;
    let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
    // The open card editor swallows ALL mouse input: rows take focus
    // and Save/Cancel fire, clicks behind it move nothing.
    if state.card_edit.is_some() {
        if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left)) {
            let area = crate::ui::dialogs::card_edit::card_edit_area(ratatui::layout::Rect::new(
                0, 0, cols, rows,
            ));
            state.board_card_edit_click(mev.column, mev.row, area);
        }
        return;
    }
    // The open board owns main-area clicks: column/card focus follows
    // the painted cells, footer and border stay dead, and nothing
    // falls through to the panes behind the board.
    if state.board_open
        && matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left))
        && mev.column >= areas.main.x
        && mev.column < areas.main.x + areas.main.width
        && mev.row >= areas.main.y
        && mev.row < areas.main.y + areas.main.height
    {
        let view = state.board_view();
        if let Some((ci, card)) = ui::board::board_cell_at(areas.main, &view, mev.column, mev.row) {
            state.board_focus.column = ci;
            if let Some(k) = card {
                state.board_focus.card = k;
            }
            state.ensure_board_focus();
            state.dirty = true;
        }
        return;
    }
    // Tab strip: click-to-activate like the sessions bar, hover ignored.
    // Grid mode draws no tab strip, so row 0 belongs to the tiles there.
    if !state.grid_mode
        && areas.topbar.height > 0
        && mev.row >= areas.topbar.y
        && mev.row < areas.topbar.y + areas.topbar.height
    {
        let topbar = state.topbar();
        let buttons = ui::topbar::layout_topbar(areas.topbar, &topbar.tabs, state.pill_tabs);
        if let Some(index) = ui::topbar::topbar_at(&buttons, mev.column) {
            let button = &buttons[index];
            let area = ratatui::layout::Rect::new(button.start, areas.topbar.y, button.end - button.start, 1);
            if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left))
                && ui::topbar::ChromeButton::new("", ratatui::style::Style::default())
                    .click(mev.column, mev.row, area)
            {
                if state.manager.active().is_some() {
                    // Refit whenever a live pane tab wins: lazy tabs are
                    // born 24x80 and would otherwise render cornered.
                    if state.select_top_tab(index) && !state.overlay_active() {
                        fit_active_pane(state);
                    }
                    state.dirty = true;
                }
            }
        }
        return;
    }
    // Sidebar settings row: click-to-switch Off/Yolo like the bars; hover
    // and drags must never flip the live permission mode. Grid mode hides
    // the sidebar, so this whole region belongs to the tiles instead.
    if !state.grid_mode
        && areas.sidebar.width > 0
        && areas.sidebar.height > 0
        && mev.column >= areas.sidebar.x
        && mev.column < areas.sidebar.x + areas.sidebar.width
        && mev.row >= areas.sidebar.y
        && mev.row < areas.sidebar.y + areas.sidebar.height
    {
        // Fleet router: wheel scrolls the slot, left-click switches to
        // the row. Same rects and window the render paints, recomputed
        // live, so a repaint can never desync them.
        let rich = ui::sidebar::sidebar_is_rich(areas.sidebar);
        match mev.kind {
            event::MouseEventKind::ScrollUp | event::MouseEventKind::ScrollDown => {
                // Wheel scrolls the fleet only over the list region;
                // footer rows (settings and below) stay inert. While
                // Tetris owns the list region the wheel does nothing.
                if state.tetris_open {
                    return;
                }
                let info = state.sidebar_info();
                let layout =
                    ui::sidebar::sidebar_layout(areas.sidebar, ui::sidebar::sidebar_footer_height(&info, rich));
                let in_list = mev.column >= layout.list.x
                    && mev.column < layout.list.x + layout.list.width
                    && mev.row >= layout.list.y
                    && mev.row < layout.list.y + layout.list.height;
                if !in_list {
                    return;
                }
                let len = info.sessions.len();
                let max_start = len.saturating_sub(1);
                if matches!(mev.kind, event::MouseEventKind::ScrollUp) {
                    state.fleet_scroll = state.fleet_scroll.saturating_sub(1);
                } else {
                    state.fleet_scroll = (state.fleet_scroll + 1).min(max_start);
                }
                state.dirty = true;
                return;
            }
            _ => {}
        }
        // While Tetris owns the list region, fleet clicks sleep: the
        // game paint carries no session rows to hit.
        if !state.tetris_open
            && matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left))
        {
            let info = state.sidebar_info();
            for (id, area) in ui::sidebar::sidebar_session_rects(areas.sidebar, &info, rich) {
                if ui::topbar::ChromeButton::new("[fleet]", ratatui::style::Style::default())
                    .click(mev.column, mev.row, area)
                {
                    if state.focus_session(id) {
                        fit_active_pane(state);
                    }
                    return;
                }
            }
        }
        // Armed-timer Cancel buttons: same rects the render paints,
        // recomputed live, so a repaint can never desync them. They
        // sleep while Tetris owns the list region, like fleet clicks.
        if !state.tetris_open
            && matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left))
        {
            let info = state.sidebar_info();
            for (timer_id, area) in ui::sidebar::timer_cancel_rects(areas.sidebar, &info, state.pill_tabs) {
                if ui::topbar::ChromeButton::new("[Cancel]", ratatui::style::Style::default())
                    .click(mev.column, mev.row, area)
                {
                    state.cancel_timer(&timer_id);
                    state.dirty = true;
                    return;
                }
            }
        }
        if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left)) {
            // Pinned footer row, same builder the render uses.
            let buttons = ui::sidebar::footer_mode_buttons(
                areas.sidebar,
                &state.sidebar_info(),
                rich,
                state.pill_tabs,
            );
            match ui::sidebar::mode_at(&buttons, mev.column, mev.row) {
                Some("yolo") => {
                    if ui::topbar::ChromeButton::new("[Yolo]", ratatui::style::Style::default())
                        .click(mev.column, mev.row, buttons.yolo) {
                        state.set_permission_mode(crate::infra::config::PermissionMode::Yolo);
                    }
                }
                Some(_) => {
                    if ui::topbar::ChromeButton::new("[Off]", ratatui::style::Style::default())
                        .click(mev.column, mev.row, buttons.off) {
                        state.set_permission_mode(crate::infra::config::PermissionMode::Off);
                    }
                }
                None => {}
            }
        }
        // Kanban button, gap-separated below the autopilot buttons:
        // same builder the render uses, so clicks track the paint exactly.
        if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left)) {
            let info = state.sidebar_info();
            let board = ui::sidebar::board_button_area(
                areas.sidebar,
                &info,
                ui::sidebar::sidebar_is_rich(areas.sidebar),
                state.pill_tabs,
            );
            if ui::board::board_at(&board, mev.column, mev.row)
                && ui::topbar::ChromeButton::new("[Kanban]", ratatui::style::Style::default())
                    .click(mev.column, mev.row, board)
            {
                state.toggle_board();
            }
        }
        // Tetris button, gap-separated below Kanban: same builder the
        // render uses, so clicks track the paint exactly.
        if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left)) {
            let info = state.sidebar_info();
            let game = ui::sidebar::tetris_button_area(
                areas.sidebar,
                &info,
                ui::sidebar::sidebar_is_rich(areas.sidebar),
                state.pill_tabs,
            );
            if ui::board::board_at(&game, mev.column, mev.row)
                && ui::topbar::ChromeButton::new("[Tetris]", ratatui::style::Style::default())
                    .click(mev.column, mev.row, game)
            {
                state.toggle_tetris();
            }
        }
        return;
    }
    if mev.row >= areas.session_bar.y
        && mev.row < areas.session_bar.y + areas.session_bar.height
        && areas.session_bar.height > 0
    {
        let segments = ui::session_bar::session_bar_segments_for_area(&state.tabs(), areas.session_bar, state.pill_tabs);
        let buttons = ui::session_bar::layout_session_bar(areas.session_bar, &segments);
        // Click-to-activate only: hover (Moved) and drags must never steal
        // the session; pane mouse protocols still get every event below.
        if let Some(index) = ui::session_bar::session_at(&buttons, mev.column) {
            let button = buttons.iter().find(|b| b.index == Some(index)).unwrap();
            let area = ratatui::layout::Rect::new(button.start, areas.session_bar.y, button.end - button.start, 1);
            if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left))
                && ui::topbar::ChromeButton::new(&button.label, ratatui::style::Style::default())
                    .click(mev.column, mev.row, area)
            {
                if state.select_session(index) {
                    fit_active_pane(state);
                }
                state.dirty = true;
            }
        }
        return;
    }
    // An open tour owns the main area: the wheel scrolls its code
    // window and every other main-area event dies here, so clicks
    // never reach the agent pane hiding behind the tour. Chrome above
    // (tabs, sidebar, session bar) already returned, so those clicks
    // keep working.
    if state.walkthrough_overlay_active() {
        match mev.kind {
            event::MouseEventKind::ScrollUp => {
                if let Some(tour) = state.walkthrough_overlay_mut() {
                    tour.scroll_code(crate::walkthrough::WHEEL_SCROLL_LINES);
                    state.dirty = true;
                }
            }
            event::MouseEventKind::ScrollDown => {
                if let Some(tour) = state.walkthrough_overlay_mut() {
                    tour.scroll_code(-crate::walkthrough::WHEEL_SCROLL_LINES);
                    state.dirty = true;
                }
            }
            _ => {}
        }
        return;
    }
    // A focused Writer tab owns main-area clicks the same way: pills
    // fire, thread rows select, the editor takes cursor/drag/wheel,
    // and nothing falls through to the agent pane behind the overlay.
    // Chrome above (tabs, sidebar, session bar) already returned, so
    // those clicks keep working. The tour arm above wins when both.
    if state.writer_keys_active() {
        super::writer::handle_writer_mouse(state, mev);
        return;
    }
    // Grid mode owns the main area: a left-click focuses the clicked
    // tile and the wheel scrolls the focused pane's scrollback. App
    // mouse protocols stay quiet here: tile coordinates don't map onto
    // full-size panes, so forwarding them would mis-deliver.
    if state.grid_mode {
        let full = ratatui::layout::Rect::new(0, 0, cols, rows);
        let grid = ui::layout::grid_area(full);
        let in_grid = mev.column >= grid.x
            && mev.column < grid.right()
            && mev.row >= grid.y
            && mev.row < grid.bottom();
        if matches!(mev.kind, event::MouseEventKind::Down(event::MouseButton::Left)) {
            let order = state.manager.order().to_vec();
            let cells = ui::layout::grid_cells(grid, order.len());
            if let Some(index) = ui::layout::grid_cell_at(&cells, mev.column, mev.row) {
                if let Some(&id) = order.get(index) {
                    state.manager.switch(id);
                    state.dirty = true;
                }
            }
        } else if in_grid {
            match mev.kind {
                event::MouseEventKind::ScrollUp | event::MouseEventKind::ScrollDown => {
                    if let Some(active) = state.manager.active() {
                        let step = crate::session::pty::PtyPane::SCROLL_LINES_PER_NOTCH;
                        let delta = if matches!(mev.kind, event::MouseEventKind::ScrollUp) {
                            step
                        } else {
                            -step
                        };
                        state.manager.scroll_view(active, delta);
                        state.dirty = true;
                    }
                }
                _ => {}
            }
        }
        return;
    }
    let Some(active) = state.manager.active() else {
        return;
    };
    // A focused Visual tab owns the wheel (pan) and its zoom
    // buttons; every other main-area event dies here so clicks
    // never reach the agent pane behind the diagram.
    #[cfg(feature = "visual")]
    if state.visual_tab_focused() {
        if let Some((id, chrome, (cell_w, cell_h))) = visual_viewport(state) {
            let (area_cols, area_rows) = (chrome.image.width, chrome.image.height);
            let over_footer = chrome.footer.height > 0
                && mev.column >= chrome.footer.x
                && mev.column < chrome.footer.x.saturating_add(chrome.footer.width)
                && mev.row >= chrome.footer.y
                && mev.row < chrome.footer.y.saturating_add(chrome.footer.height);
            match mev.kind {
                event::MouseEventKind::ScrollUp => {
                    // Wheel over the chat footer reads back through
                    // history; over the image it pans the diagram.
                    // Visible history is the box interior rows.
                    let visible = crate::ui::visual::visual_chat_history_rows(chrome.footer.height);
                    if over_footer {
                        state.visual_chat_scroll(id, 3, visible);
                    } else {
                        state.visual_scroll(id, 0, -3, area_cols, area_rows, cell_w, cell_h);
                    }
                }
                event::MouseEventKind::ScrollDown => {
                    let visible = crate::ui::visual::visual_chat_history_rows(chrome.footer.height);
                    if over_footer {
                        state.visual_chat_scroll(id, -3, visible);
                    } else {
                        state.visual_scroll(id, 0, 3, area_cols, area_rows, cell_w, cell_h);
                    }
                }
                event::MouseEventKind::ScrollLeft => {
                    state.visual_scroll(id, -3, 0, area_cols, area_rows, cell_w, cell_h);
                }
                event::MouseEventKind::ScrollRight => {
                    state.visual_scroll(id, 3, 0, area_cols, area_rows, cell_w, cell_h);
                }
                event::MouseEventKind::Down(event::MouseButton::Left) => {
                    // Buttons keep priority over the image, so a click
                    // never does both. Centering follows the backend
                    // that painted, like the pick math must.
                    match ui::visual::visual_button_at(&chrome, mev.column, mev.row) {
                        Some(ui::visual::VisualButton::Chat) => {
                            state.visual_toggle_chat(id);
                        }
                        Some(button) => {
                            state.visual_zoom(id, button, area_cols, area_rows, cell_w, cell_h);
                        }
                        None
                            if mev.column >= chrome.image.x
                                && mev.row >= chrome.image.y
                                && mev.column
                                    < chrome.image.x.saturating_add(chrome.image.width)
                                && mev.row
                                    < chrome.image.y.saturating_add(chrome.image.height) =>
                        {
                            state.visual_select_at(
                                id,
                                mev.column - chrome.image.x,
                                mev.row - chrome.image.y,
                                area_cols,
                                area_rows,
                                cell_w,
                                cell_h,
                                crate::visual::kitty_supported_env(),
                            );
                        }
                        None => {}
                    }
                }
                _ => {}
            }
        }
        return;
    }
    if state.overlay_active() { return; }
    let mode = state.manager.mouse_mode(active);
    if mode == vt100::MouseProtocolMode::None {
        // No mouse protocol: the wheel scrolls this pane instead of dying
        // silently, so every session scrolls whether or not its app
        // reports mouse. Normal screen moves the scrollback viewport;
        // the alternate screen has none, so scroll_view sends Up/Down
        // arrows for fullscreen apps that never take the mouse (codex,
        // claude). Other buttons still do nothing.
        if ui::layout::translate_mouse(ui::layout::pane_grid_area(&areas), mev.column, mev.row).is_some() {
            let step = crate::session::pty::PtyPane::SCROLL_LINES_PER_NOTCH;
            match mev.kind {
                event::MouseEventKind::ScrollUp => {
                    state.manager.scroll_view(active, step);
                    state.dirty = true;
                }
                event::MouseEventKind::ScrollDown => {
                    state.manager.scroll_view(active, -step);
                    state.dirty = true;
                }
                _ => {}
            }
        }
        return;
    }
    let Some((col, row)) = ui::layout::translate_mouse(ui::layout::pane_grid_area(&areas), mev.column, mev.row) else {
        return;
    };
    let pev = event::MouseEvent {
        column: col,
        row,
        ..mev
    };
    if let Some(bytes) = input::encode_mouse(&pev, mode, state.manager.mouse_encoding(active)) {
        let _ = state.manager.pane_write(active, &bytes);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::event::AppEvent;
    use crate::infra::ids::RunId;
    use crate::tui::keys::spawn_shell_cmd;

    #[test]
    fn session_bar_click_switches_sessions() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        // Session bar owns the last row; the one-cell inset plus caps
        // and centering pads push the second button to column 16, so
        // this click lands on its left cap.
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 16,
            row: 23,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        forward_mouse(&mut state, click);
        let order = state.manager.order().to_vec();
        assert_eq!(state.manager.active(), Some(order[1]));
        assert_eq!(state.manager.pane_size(order[1]), Some((20, 62)));
        assert!(state.manager.remove(order[0]));
        assert!(state.manager.remove(order[1]));
    }

    #[test]
    fn fleet_click_switches_to_row_session() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let order = state.manager.order().to_vec();
        assert_eq!(state.manager.active(), Some(order[0]));
        // Blocks breathe: name plus reason rows, then a dead gap row.
        // Drive the click from the painted rect, never hand-computed
        // geometry.
        let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let info = state.sidebar_info();
        let rects = ui::sidebar::sidebar_session_rects(areas.sidebar, &info, true);
        assert_eq!(rects.len(), 2);
        assert_eq!(
            rects[0].1.y + rects[0].1.height + 1,
            rects[1].1.y,
            "gap between blocks"
        );
        let (_, area) = &rects[1];
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x + 1,
                row: area.y,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.manager.active(), Some(order[1]), "row click focuses");
        assert!(state.manager.remove(order[0]));
        assert!(state.manager.remove(order[1]));
    }

    #[test]
    fn fleet_wheel_scrolls_past_the_cap() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        for _ in 0..6 {
            spawn_shell_cmd(&mut state, "exec sleep 30");
        }
        // Compact sidebar caps the slot at two blocks of six.
        assert_eq!(state.fleet_scroll, 0);
        let wheel = |down: bool| MouseEvent {
            kind: if down {
                MouseEventKind::ScrollDown
            } else {
                MouseEventKind::ScrollUp
            },
            column: 70,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        forward_mouse(&mut state, wheel(true));
        assert_eq!(state.fleet_scroll, 1, "wheel down scrolls");
        for _ in 0..9 {
            forward_mouse(&mut state, wheel(true));
        }
        assert_eq!(state.fleet_scroll, 5, "scroll saturates at the tail");
        forward_mouse(&mut state, wheel(false));
        assert_eq!(state.fleet_scroll, 4, "wheel up scrolls back");
        for id in state.manager.order().to_vec() {
            assert!(state.manager.remove(id));
        }
    }

    #[test]
    fn fleet_wheel_over_footer_is_inert() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        for _ in 0..6 {
            spawn_shell_cmd(&mut state, "exec sleep 30");
        }
        // Scroll once from the list, then wheel the pinned footer row:
        // settings must never scroll the fleet.
        let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 80, 24));
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: areas.sidebar.x + 2,
                row: areas.sidebar.y + 2,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.fleet_scroll, 1, "list wheel scrolls");
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: areas.sidebar.x + 2,
                row: areas.sidebar.bottom() - 1,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.fleet_scroll, 1, "footer wheel is inert");
        for id in state.manager.order().to_vec() {
            assert!(state.manager.remove(id));
        }
    }

    #[test]
    fn topbar_click_selects_terminal_tab() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        let id = state
            .manager
            .spawn_agent(
                "a",
                &std::env::temp_dir(),
                "exec cat",
                crate::infra::ids::RunId::generate(),
                "codex",
            )
            .unwrap();
        // Top strip is row 0: "[Codex]" then "[Terminal]" from column 10.
        let bar = state.topbar();
        assert_eq!(bar.tabs.len(), 3);
        assert!(bar.tabs[0].active);
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 12,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        let bar = state.topbar();
        assert!(bar.tabs[1].active);
        // Hover on the strip never switches back.
        state.dirty = false;
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: 3,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert!(state.topbar().tabs[1].active);
        assert!(!state.dirty);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn topbar_click_opens_visual_and_returns_to_agent() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            RunId::generate(), "codex",
        ).unwrap();
        let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let buttons = ui::topbar::layout_topbar(areas.topbar, &state.topbar().tabs, state.pill_tabs);
        let click = |column| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column, row: areas.topbar.y,
            modifiers: KeyModifiers::NONE,
        };
        // Index 2 is the live SCM tab; Visual overlay sits at 3.
        forward_mouse(&mut state, click(buttons[3].start));
        assert!(state.overlay_active());
        assert!(state.topbar().tabs[3].active);
        forward_mouse(&mut state, click(buttons[0].start));
        assert!(!state.overlay_active());
        assert!(state.topbar().tabs[0].active);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn wheel_scrolls_tour_code_while_tabs_stay_clickable() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            RunId::generate(), "codex",
        ).unwrap();
        let content = (1..=40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let tour = crate::walkthrough::Walkthrough::start(
            "Tour".to_string(),
            "f.rs".to_string(),
            &content,
            vec![crate::walkthrough::Step {
                start: 1,
                end: 3,
                explanation: "first".to_string(),
            }],
        ).unwrap();
        state.walkthroughs.insert(id, tour);
        assert!(state.select_top_tab(4));
        let wheel = |kind| MouseEvent {
            kind, column: 90, row: 20, modifiers: KeyModifiers::NONE,
        };
        forward_mouse(&mut state, wheel(MouseEventKind::ScrollDown));
        assert_eq!(state.walkthrough_overlay().unwrap().code_scroll, 3);
        forward_mouse(&mut state, wheel(MouseEventKind::ScrollUp));
        assert_eq!(state.walkthrough_overlay().unwrap().code_scroll, 0);
        // Clicks in the tour pane never reach the agent, but the tab
        // strip above it still switches back to the CLI tab.
        let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let buttons = ui::topbar::layout_topbar(areas.topbar, &state.topbar().tabs, state.pill_tabs);
        forward_mouse(&mut state, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: buttons[0].start, row: areas.topbar.y, modifiers: KeyModifiers::NONE,
        });
        assert!(!state.walkthrough_overlay_active());
        assert!(state.topbar().tabs[0].active);
        assert!(state.manager.remove(id));
    }

    #[test]
    fn sidebar_cancel_click_drops_armed_timer() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        std::env::set_var("CODEX_BIN", "cat");
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            RunId::generate(), "codex",
        ).unwrap();
        std::env::remove_var("CODEX_BIN");
        let run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        let now = std::time::Instant::now();
        state.broker.call(&state.manager, &run, "schedule_prompt",
            r#"{"prompt":"later","delay_seconds":600}"#, now).expect("arms");
        let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let rects = ui::sidebar::timer_cancel_rects(areas.sidebar, &state.sidebar_info(), state.pill_tabs);
        assert_eq!(rects.len(), 1, "one armed timer, one button");
        let (_, area) = &rects[0];
        forward_mouse(&mut state, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 1, row: area.y, modifiers: KeyModifiers::NONE,
        });
        assert!(state.sidebar_info().session.unwrap().timers.is_empty(), "click cancels");
        assert!(state.manager.remove(id));
    }

    #[test]
    fn topbar_click_fits_lazy_scm_pane_to_main() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            RunId::generate(), "codex",
        ).unwrap();
        let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let buttons = ui::topbar::layout_topbar(areas.topbar, &state.topbar().tabs, state.pill_tabs);
        // Click the SCM tab: the pane is born 24x80 and must take the
        // main area at once (180x40 less top strip, session bar, and
        // main-pane borders), not render cornered.
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: buttons[2].start,
                row: areas.topbar.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(state.manager.active_tab_kind(id), Some(crate::session::TabKind::Scm));
        // Sidebar clamps at 36, so the main pane keeps the remainder.
        assert_eq!(state.manager.pane_size(id), Some((36, 142)));
        assert!(state.manager.remove(id));
    }

    #[test]
    fn session_bar_hover_never_switches_sessions() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let order = state.manager.order().to_vec();
        assert_eq!(state.manager.active(), Some(order[0]));
        state.dirty = false;
        // Same coordinates as the click test, but a hover and a drag.
        for kind in [
            MouseEventKind::Moved,
            MouseEventKind::Drag(MouseButton::Left),
        ] {
            forward_mouse(
                &mut state,
                MouseEvent {
                    kind,
                    column: 12,
                    row: 23,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                },
            );
        }
        assert_eq!(state.manager.active(), Some(order[0]));
        assert!(!state.dirty, "hover leaves no work");
        assert!(state.manager.remove(order[0]));
        assert!(state.manager.remove(order[1]));
    }

    #[test]
    fn wheel_scrolls_pane_scrollback_when_mouse_is_off() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(
            &mut state,
            "for i in $(seq 1 40); do echo line-$i; done; exec cat",
        );
        let id = state.manager.active().unwrap();
        assert_eq!(
            state.manager.mouse_mode(id),
            vt100::MouseProtocolMode::None
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let tail = state
                .manager
                .styled_rows(id)
                .last()
                .map(|r| r.iter().map(|c| c.text.clone()).collect::<String>())
                .unwrap_or_default();
            if tail.contains("line-40") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "history never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let areas = crate::ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 80, 24));
        let grid = crate::ui::layout::pane_grid_area(&areas);
        let at = |kind| MouseEvent {
            kind,
            column: grid.x + grid.width / 2,
            row: grid.y + grid.height / 2,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let screen_text = |state: &AppState| {
            state
                .manager
                .styled_rows(id)
                .iter()
                .flat_map(|r| r.iter().map(|c| c.text.clone()))
                .collect::<String>()
        };
        state.dirty = false;
        forward_mouse(&mut state, at(MouseEventKind::ScrollUp));
        assert!(state.dirty, "wheel marks redraw");
        assert!(
            !screen_text(&state).contains("line-40"),
            "wheel up leaves the live tail"
        );
        forward_mouse(&mut state, at(MouseEventKind::ScrollDown));
        assert!(
            screen_text(&state).contains("line-40"),
            "wheel down returns to live"
        );
        assert!(state.manager.remove(id));
    }

    #[test]
    fn wheel_on_alt_screen_without_mouse_sends_arrows() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        // cat never takes the mouse; the escapes put its screen on the
        // alternate buffer like codex/claude, cursor parked at (9, 19).
        // Raw mode first: every real fullscreen app takes it, and without
        // it the line discipline echoes ESC back as `^[`, which the parser
        // prints instead of driving the cursor.
        spawn_shell_cmd(
            &mut state,
            "stty raw -echo; printf '\\033[?1049h\\033[10;20H'; exec cat",
        );
        let id = state.manager.active().unwrap();
        assert_eq!(
            state.manager.mouse_mode(id),
            vt100::MouseProtocolMode::None
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if state.manager.cursor(id) == Some((9, 19)) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "alt screen never engaged");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(state.manager.alternate_screen(id));
        let areas = crate::ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 80, 24));
        let grid = crate::ui::layout::pane_grid_area(&areas);
        let at = |kind| MouseEvent {
            kind,
            column: grid.x + grid.width / 2,
            row: grid.y + grid.height / 2,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        // Wheel up arrives as 3 Up arrows echoed back through cat and
        // lifts the cursor 3 rows; wheel down returns it.
        forward_mouse(&mut state, at(MouseEventKind::ScrollUp));
        loop {
            if state.manager.cursor(id) == Some((6, 19)) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "wheel up never moved the cursor");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        forward_mouse(&mut state, at(MouseEventKind::ScrollDown));
        loop {
            if state.manager.cursor(id) == Some((9, 19)) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "wheel down never moved the cursor");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(state.manager.remove(id));
    }

    #[test]
    fn tetris_sidebar_click_toggles_game() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        // Compact footer: mode buttons at y=17, a blank gap at y=18,
        // Kanban at y=19, another gap at y=20, Tetris at y=21.
        assert!(!state.tetris_open);
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 67,
                row: 21,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert!(state.tetris_open, "tetris button opens the game");
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 67,
                row: 21,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert!(!state.tetris_open, "same button switches back");
    }

    #[test]
    fn sidebar_click_switches_mode_hover_ignored() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        // Sidebar is x=64..80; with nothing pending the footer
        // shrinks and buttons sit at y=17, Off at x=66..71, a blank
        // gap at y=18, the Kanban button at y=19, Tetris at y=21.
        assert_eq!(state.permission_mode, crate::infra::config::PermissionMode::Yolo);
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 67,
                row: 17,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.permission_mode, crate::infra::config::PermissionMode::Off);
        // The Kanban row toggles the board, never the permission mode.
        assert!(!state.board_open);
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 67,
                row: 19,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert!(state.board_open);
        assert_eq!(state.permission_mode, crate::infra::config::PermissionMode::Off);
        // Hover over Yolo (x=72..78) must not flip it back.
        state.dirty = false;
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: 73,
                row: 17,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.permission_mode, crate::infra::config::PermissionMode::Off);
        assert!(!state.dirty, "hover leaves no work");
        // Click Yolo to return (pill starts at x=74, footer row 17).
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 75,
                row: 17,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.permission_mode, crate::infra::config::PermissionMode::Yolo);
    }

    #[test]
    fn board_click_focuses_painted_column() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(30, 160));
        state.boards.board_create("team", None).unwrap();
        state.board_open = true;
        state.ensure_board_focus();
        assert_eq!(state.board_focus.column, 0);
        // Four default columns across the wide main area: the header
        // paints at row 3 and the second column starts at x=31.
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 40,
                row: 3,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.board_focus.column, 1);
    }

    #[test]
    fn grid_click_focuses_cell() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let none = KeyModifiers::NONE;
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(24, 80));
        spawn_shell_cmd(&mut state, "exec sleep 30");
        spawn_shell_cmd(&mut state, "exec sleep 30");
        let order = state.manager.order().to_vec();
        assert_eq!(order.len(), 2);
        state.toggle_grid();
        // Click the middle of the first tile (cells tile 40x23 from y 0).
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 20,
                row: 12,
                modifiers: none,
            },
        );
        assert_eq!(state.manager.active(), Some(order[0]));
        // Click the second tile.
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 60,
                row: 12,
                modifiers: none,
            },
        );
        assert_eq!(state.manager.active(), Some(order[1]));
        assert!(state.manager.remove(order[0]));
        assert!(state.manager.remove(order[1]));
    }

}
