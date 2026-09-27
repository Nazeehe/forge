//! TUI visual tab: viewport math, terminal sync, viewport keys.

#[cfg(feature = "visual")]
use std::io;

#[cfg(feature = "visual")]
use crossterm::event;

#[cfg(feature = "visual")]
use crate::app::AppState;
#[cfg(feature = "visual")]
use crate::ui;

#[cfg(feature = "visual")]
use super::StdoutTerminal;

/// Kitty image sync, after the ratatui draw flushed: hide first (a
/// replacement emits both sides), then position the cursor at the
/// overlay origin and transmit `c` cell columns wide. Best effort —
/// a dead terminal means we are quitting anyway.
/// Focused Visual tab viewport: session, tab image region, and
/// text-cell size. The region is the same chrome the fallback art
/// and the button hit test use, so all three backends agree. The
/// cell size comes from the terminal pixel report, falling back to
/// the 8x16 the half-block art draws with exactly.
#[cfg(feature = "visual")]
pub(super) fn visual_viewport(
    state: &AppState,
) -> Option<(crate::session::SessionId, crate::ui::visual::VisualChrome, (f64, f64))> {
    let (id, _) = state.visual_focused_frame()?;
    let (rows, cols) = state.term_size;
    let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, cols, rows));
    let content = ui::layout::pane_content_area(&areas);
    let chrome = ui::visual::visual_chrome(
        content,
        state.pill_tabs,
        state.visual_footer_rows(id, content.height),
    );
    let cell = match crossterm::terminal::window_size() {
        Ok(ws) => crate::visual::cell_px(cols, rows, ws.width as u32, ws.height as u32),
        Err(_) => crate::visual::FALLBACK_CELL_PX,
    };
    Some((id, chrome, cell))
}

#[cfg(feature = "visual")]
pub(super) fn sync_visual_terminal(
    state: &mut AppState,
    _terminal: &mut StdoutTerminal,
) -> io::Result<()> {
    use std::io::Write as _;
    if let Some(image_id) = state.visual_take_hide() {
        write!(io::stdout(), "{}", crate::visual::kitty_delete(image_id))?;
    }
    if !crate::visual::kitty_supported_env() {
        return Ok(());
    }
    // Peek before bytes: an unchanged frame costs comparisons, not
    // a clone or crop+encode the gate would drop. Bytes still come
    // before the claim, so the gate records only paints the terminal
    // actually receives and a failed encode retries next frame.
    if let Some((id, chrome, (cell_w, cell_h))) = visual_viewport(state) {
        let (_, generation) = state.visual_focused_frame().expect("viewport implies frame");
        if let Some(paint) = state.visual_paint(id, chrome.image, cell_w, cell_h) {
            if state.visual_current_paint() != Some(paint) {
                if let Some(png) = state.visual_frame_png(id, generation, paint) {
                    if let Some(spec) = state.visual_take_show(true, paint) {
                        // No delete before a same-id re-display: the
                        // fixed placement id swaps it in place, while
                        // delete-then-show flashes the bare pane.
                        let placed = spec.paint;
                        if placed.out_cols > 0 && placed.out_rows > 0 {
                            crossterm::execute!(
                                io::stdout(),
                                crossterm::cursor::MoveTo(placed.cursor_x, placed.cursor_y)
                            )?;
                            write!(
                                io::stdout(),
                                "{}",
                                crate::visual::kitty_transmit(
                                    &png,
                                    spec.image_id,
                                    placed.out_cols,
                                    placed.out_rows
                                )
                            )?;
                        }
                    }
                }
            }
        }
    }
    io::stdout().flush()?;
    Ok(())
}

/// Visual tab viewport keys: arrows pan, `+`/`-` zoom, Esc clears
/// the selection first and leaves the tab second. Returns true when
/// the key belonged to the viewport.
/// Plain keys only: chords with Ctrl/Alt still reach the router, so
/// prefixes keep working with a diagram open.
#[cfg(feature = "visual")]
pub(super) fn handle_visual_key(state: &mut AppState, key: event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    let Some((id, chrome, (cell_w, cell_h))) = visual_viewport(state) else {
        return true;
    };
    let (area_cols, area_rows) = (chrome.image.width, chrome.image.height);
    let mods = key.modifiers;
    // Ctrl/Alt chords still reach the router, so prefixes keep
    // working with a diagram open.
    if mods.contains(
        KeyModifiers::CONTROL
            | KeyModifiers::ALT
            | KeyModifiers::SUPER
            | KeyModifiers::HYPER
            | KeyModifiers::META,
    ) {
        return false;
    }
    // Input mode owns the editing keys so `+`/`-` type instead of
    // zooming; browse mode keeps every viewport key. Either way the
    // tab swallows the key and the agent pane never sees it.
    if state.visual_slots.get(&id).is_some_and(|s| s.input_active) {
        match key.code {
            KeyCode::Enter if mods.is_empty() => {
                let ready = state
                    .visual_slots
                    .get(&id)
                    .is_some_and(|s| s.draft.as_deref().is_some_and(|d| !d.trim().is_empty()));
                if ready {
                    state.submit_visual_question(id);
                } else if let Some(slot) = state.visual_slots.get_mut(&id) {
                    slot.input_active = false;
                    state.dirty = true;
                }
            }
            KeyCode::Esc if mods.is_empty() => {
                if let Some(slot) = state.visual_slots.get_mut(&id) {
                    slot.input_active = false;
                    state.dirty = true;
                }
            }
            KeyCode::Backspace if mods.is_empty() => {
                if let Some(slot) = state.visual_slots.get_mut(&id) {
                    if let Some(draft) = slot.draft.as_mut() {
                        draft.pop();
                    }
                    state.dirty = true;
                }
            }
            KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                if let Some(slot) = state.visual_slots.get_mut(&id) {
                    let draft = slot.draft.get_or_insert_with(String::new);
                    if draft.chars().count() < crate::walkthrough::MAX_INPUT_CHARS {
                        draft.push(c);
                    }
                    state.dirty = true;
                }
            }
            _ if mods.is_empty() => {}
            // Shifted non-text keys (and any other modifier) keep the
            // old passthrough behavior so nothing else is swallowed.
            _ => return false,
        }
        return true;
    }
    if mods != KeyModifiers::NONE {
        return false;
    }
    match key.code {
        KeyCode::Left => {
            state.visual_scroll(id, -1, 0, area_cols, area_rows, cell_w, cell_h);
        }
        KeyCode::Right => {
            state.visual_scroll(id, 1, 0, area_cols, area_rows, cell_w, cell_h);
        }
        KeyCode::Up => {
            state.visual_scroll(id, 0, -1, area_cols, area_rows, cell_w, cell_h);
        }
        KeyCode::Down => {
            state.visual_scroll(id, 0, 1, area_cols, area_rows, cell_w, cell_h);
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomIn, area_cols, area_rows, cell_w, cell_h);
        }
        KeyCode::Char('-') | KeyCode::Char('_') => {
            state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomOut, area_cols, area_rows, cell_w, cell_h);
        }
        KeyCode::Char('c') => {
            state.visual_toggle_chat(id);
        }
        KeyCode::Enter => {
            // Enter focuses the ask row under a selection; without
            // one it stays dead so it never reaches the pane.
            if let Some(slot) = state.visual_slots.get_mut(&id) {
                if slot.selected.is_some() {
                    slot.input_active = true;
                    state.dirty = true;
                }
            }
        }
        KeyCode::Esc => {
            // Draft goes first, then the selection: Esc clears the
            // ask text and stays, a second Esc clears the highlight,
            // a third leaves the tab. (Input mode Esc above exits
            // input first and keeps the draft.)
            if let Some(slot) = state.visual_slots.get_mut(&id) {
                if slot.draft.take().is_some() {
                    state.dirty = true;
                    return true;
                }
                if slot.selected.take().is_some() {
                    state.dirty = true;
                    return true;
                }
            }
            state.overlay_view = None;
            state.dirty = true;
        }
        _ => {}
    }
    true
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::event::AppEvent;
    use crate::infra::ids::RunId;
    use crate::tui::input::InputRouter;
    use crate::tui::keys::handle_key;
    use crate::tui::mouse::forward_mouse;

    #[cfg(feature = "visual")]
    fn open_visual_overlay(state: &mut AppState) -> crate::session::SessionId {
        let id = state.manager.spawn_agent(
            "agent", &std::env::temp_dir(), "exec cat",
            RunId::generate(), "codex",
        ).unwrap();
        state.visual_slots.insert(id, crate::app::VisualSlot {
            generation: 1,
            png: vec![1, 2, 3],
            rgba: Vec::new(),
            width: 1000,
            height: 1000,
            title: String::new(),
            alt: String::new(),
            zoom: 1.0,
            scroll_x: 0,
            scroll_y: 0,
            shapes: Vec::new(),
            vb: [0.0, 0.0, 1000.0, 1000.0],
            selected: None,
            input_active: false,
            chat_open: false,
            chat_scroll: 0,
            draft: None,
            questions: Vec::new(),
        });
        // Agent tabs: CLI, terminal, SCM, then Visual.
        assert!(state.select_top_tab(3));
        assert!(state.visual_tab_focused());
        id
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_arrows_pan_plus_minus_zoom_esc_leaves() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = open_visual_overlay(&mut state);
        let mut router = InputRouter::new();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        // Contained at zoom 1, nothing to pan: keys are swallowed and
        // the viewport does not move.
        handle_key(&mut state, &mut router, key(KeyCode::Right));
        handle_key(&mut state, &mut router, key(KeyCode::Down));
        let slot = state.visual_slots.get(&id).unwrap();
        assert_eq!((slot.scroll_x, slot.scroll_y), (0, 0));
        assert!(state.visual_tab_focused(), "arrows stay in the tab");
        // Zoom in, then pan down and back up.
        handle_key(&mut state, &mut router, key(KeyCode::Char('+')));
        assert!(state.visual_slots.get(&id).unwrap().zoom > 1.0);
        handle_key(&mut state, &mut router, key(KeyCode::Down));
        assert_eq!(state.visual_slots.get(&id).unwrap().scroll_y, 1);
        handle_key(&mut state, &mut router, key(KeyCode::Up));
        assert_eq!(state.visual_slots.get(&id).unwrap().scroll_y, 0);
        handle_key(&mut state, &mut router, key(KeyCode::Char('-')));
        assert_eq!(state.visual_slots.get(&id).unwrap().zoom, 1.0);
        // Esc leaves the tab; other keys never reach the pane.
        handle_key(&mut state, &mut router, key(KeyCode::Char('x')));
        assert!(state.visual_tab_focused(), "plain keys are swallowed");
        handle_key(&mut state, &mut router, key(KeyCode::Esc));
        assert!(!state.visual_tab_focused());
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_wheel_pans_horizontally_like_vertically() {
        use crossterm::event::{MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = open_visual_overlay(&mut state);
        let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let content = ui::layout::pane_content_area(&areas);
        let chrome = ui::visual::visual_chrome(
            content,
            state.pill_tabs,
            state.visual_footer_rows(id, content.height),
        );
        let (area_cols, area_rows) = (chrome.image.width, chrome.image.height);
        // Zoom to the max: the square test image only overflows the
        // wide tab horizontally at high zoom (at low zoom the height
        // is the limiting axis, so x clamps to zero).
        while state.visual_zoom(id, ui::visual::VisualButton::ZoomIn, area_cols, area_rows, 8.0, 16.0) {}
        let wheel = |kind| MouseEvent {
            kind, column: chrome.image.x + 2, row: chrome.image.y + 2,
            modifiers: KeyModifiers::NONE,
        };
        forward_mouse(&mut state, wheel(MouseEventKind::ScrollRight));
        assert_eq!(state.visual_slots.get(&id).unwrap().scroll_x, 3);
        forward_mouse(&mut state, wheel(MouseEventKind::ScrollLeft));
        assert_eq!(state.visual_slots.get(&id).unwrap().scroll_x, 0);
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_image_click_selects_shape_and_toggles() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = open_visual_overlay(&mut state);
        let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let content = ui::layout::pane_content_area(&areas);
        let chrome = ui::visual::visual_chrome(
            content,
            state.pill_tabs,
            state.visual_footer_rows(id, content.height),
        );
        // Box the clicked image cell plus margin, in SVG units: the
        // 1000x1000 test slot uses identical SVG dims. Display size
        // matches production (same fit inputs as the click path).
        let (area_cols, area_rows) = (chrome.image.width, chrome.image.height);
        let (disp_cols, disp_rows) =
            crate::visual::fit_display(1000, 1000, 1.0, area_cols, area_rows, 8.0, 16.0);
        // The live click path centers on the Kitty backend and starts
        // at the corner on fallback: aim two diagram cells in under
        // whichever placement the environment paints with.
        let (off_x, off_y) = if crate::visual::kitty_supported_env() {
            (
                area_cols.saturating_sub(disp_cols.min(area_cols)) / 2,
                area_rows.saturating_sub(disp_rows.min(area_rows)) / 2,
            )
        } else {
            (0, 0)
        };
        let (px, py) = crate::visual::view_to_source(2, 2, 0, 0, disp_cols, disp_rows, 1000, 1000);
        {
            let slot = state.visual_slots.get_mut(&id).unwrap();
            slot.shapes = vec![crate::visual::ShapeBox {
                id: "n".to_string(),
                label: "Node".to_string(),
                x: (px.saturating_sub(30)) as f32,
                y: (py.saturating_sub(30)) as f32,
                width: 60.0,
                height: 60.0,
            }];
        }
        let click = |column, row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column, row,
            modifiers: KeyModifiers::NONE,
        };
        let (bx, by) = (chrome.image.x + off_x + 2, chrome.image.y + off_y + 2);
        forward_mouse(&mut state, click(bx, by));
        assert_eq!(state.visual_slots.get(&id).unwrap().selected, Some(0));
        forward_mouse(&mut state, click(bx, by));
        assert_eq!(state.visual_slots.get(&id).unwrap().selected, None, "toggle off");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_esc_clears_selection_before_leaving() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = open_visual_overlay(&mut state);
        state.visual_slots.get_mut(&id).unwrap().selected = Some(0);
        let mut router = InputRouter::new();
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        handle_key(&mut state, &mut router, esc);
        assert_eq!(state.visual_slots.get(&id).unwrap().selected, None);
        assert!(state.visual_tab_focused(), "first Esc only clears");
        handle_key(&mut state, &mut router, esc);
        assert!(!state.visual_tab_focused(), "second Esc leaves");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_enter_gates_typing_and_submits() {
        // Typing arms on selection, so live tabs never need Enter to
        // focus first; Enter still arms as a fallback (so `+`/`-`
        // keep zooming outside input), Backspace edits, Enter asks,
        // and asking leaves typing armed for the next question.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = open_visual_overlay(&mut state);
        let mut router = InputRouter::new();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        {
            let slot = state.visual_slots.get_mut(&id).unwrap();
            slot.shapes = vec![crate::visual::ShapeBox {
                id: "A".to_string(),
                label: "Start".to_string(),
                x: 0.0, y: 0.0, width: 1000.0, height: 1000.0,
            }];
            slot.selected = Some(0);
        }
        handle_key(&mut state, &mut router, key(KeyCode::Char('x')));
        assert_eq!(state.visual_slots.get(&id).unwrap().draft, None, "no input mode, no draft");
        handle_key(&mut state, &mut router, key(KeyCode::Enter));
        assert!(state.visual_slots.get(&id).unwrap().input_active, "Enter focuses");
        handle_key(&mut state, &mut router, key(KeyCode::Char('h')));
        handle_key(&mut state, &mut router, key(KeyCode::Char('i')));
        assert_eq!(
            state.visual_slots.get(&id).unwrap().draft.as_deref(),
            Some("hi")
        );
        handle_key(&mut state, &mut router, key(KeyCode::Backspace));
        handle_key(&mut state, &mut router, key(KeyCode::Char('i')));
        handle_key(&mut state, &mut router, key(KeyCode::Enter));
        let slot = state.visual_slots.get(&id).unwrap();
        assert_eq!(slot.questions.len(), 1);
        assert_eq!(slot.questions[0].question, "hi");
        assert_eq!(slot.questions[0].shape_id, "A");
        assert_eq!(slot.draft, None, "draft clears on submit");
        assert!(slot.input_active, "submit stays armed");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_esc_walks_input_draft_selection_leave() {
        // Esc peels one layer at a time: input mode, draft text,
        // highlight, then the tab itself. Draft text survives the
        // first Esc so an accidental tap never eats typing.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = open_visual_overlay(&mut state);
        let mut router = InputRouter::new();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        {
            let slot = state.visual_slots.get_mut(&id).unwrap();
            slot.selected = Some(0);
            slot.draft = Some("half".to_string());
            slot.input_active = true;
        }
        handle_key(&mut state, &mut router, key(KeyCode::Esc));
        let slot = state.visual_slots.get(&id).unwrap();
        assert!(!slot.input_active, "input exits first");
        assert_eq!(slot.draft.as_deref(), Some("half"), "draft survives");
        assert!(state.visual_tab_focused());
        handle_key(&mut state, &mut router, key(KeyCode::Esc));
        assert_eq!(state.visual_slots.get(&id).unwrap().draft, None, "draft clears next");
        handle_key(&mut state, &mut router, key(KeyCode::Esc));
        assert_eq!(state.visual_slots.get(&id).unwrap().selected, None, "then selection");
        assert!(state.visual_tab_focused());
        handle_key(&mut state, &mut router, key(KeyCode::Esc));
        assert!(!state.visual_tab_focused(), "last Esc leaves");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_c_key_toggles_chat_but_types_in_input() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = open_visual_overlay(&mut state);
        let mut router = InputRouter::new();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(!state.visual_slots.get(&id).unwrap().chat_open);
        handle_key(&mut state, &mut router, key(KeyCode::Char('c')));
        assert!(state.visual_slots.get(&id).unwrap().chat_open, "c opens");
        handle_key(&mut state, &mut router, key(KeyCode::Char('c')));
        assert!(!state.visual_slots.get(&id).unwrap().chat_open, "c closes");
        // In input mode `c` is text, not a toggle.
        {
            let slot = state.visual_slots.get_mut(&id).unwrap();
            slot.selected = Some(0);
            slot.input_active = true;
        }
        handle_key(&mut state, &mut router, key(KeyCode::Char('c')));
        assert_eq!(
            state.visual_slots.get(&id).unwrap().draft.as_deref(),
            Some("c")
        );
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_shift_char_types_uppercase_in_input() {
        // Shift+F arrives as Char('F') with SHIFT held: input mode
        // must type it, not drop it to the agent pane.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = open_visual_overlay(&mut state);
        let mut router = InputRouter::new();
        {
            let slot = state.visual_slots.get_mut(&id).unwrap();
            slot.selected = Some(0);
            slot.input_active = true;
            slot.draft = Some(String::new());
        }
        handle_key(
            &mut state,
            &mut router,
            KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT),
        );
        assert_eq!(
            state.visual_slots.get(&id).unwrap().draft.as_deref(),
            Some("F"),
            "Shift+F types F in chat input"
        );
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_chat_button_click_toggles() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = open_visual_overlay(&mut state);
        let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let content = ui::layout::pane_content_area(&areas);
        let chrome = ui::visual::visual_chrome(
            content,
            state.pill_tabs,
            state.visual_footer_rows(id, content.height),
        );
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: chrome.chat.x + 1,
            row: chrome.chat.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(!state.visual_slots.get(&id).unwrap().chat_open);
        forward_mouse(&mut state, click);
        assert!(state.visual_slots.get(&id).unwrap().chat_open, "button opens");
        forward_mouse(&mut state, click);
        assert!(!state.visual_slots.get(&id).unwrap().chat_open, "button closes");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_wheel_over_footer_scrolls_chat_not_diagram() {
        use crossterm::event::{MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = open_visual_overlay(&mut state);
        {
            let slot = state.visual_slots.get_mut(&id).unwrap();
            slot.chat_open = true;
            slot.questions = (0..10)
                .map(|i| crate::visual::VisualQuestion {
                    shape_id: "A".to_string(),
                    shape_label: "S".to_string(),
                    question: format!("q{i}"),
                    answer: Some(format!("a{i}")),
                })
                .collect();
        }
        let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let content = ui::layout::pane_content_area(&areas);
        let chrome = ui::visual::visual_chrome(
            content,
            state.pill_tabs,
            state.visual_footer_rows(id, content.height),
        );
        assert!(chrome.footer.height > 0, "footer reserves rows");
        let wheel = |kind, row| MouseEvent {
            kind,
            column: chrome.footer.x + 1,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Wheel up reads back toward older pairs; wheel down comes
        // forward to the tail. The diagram viewport never moves.
        forward_mouse(&mut state, wheel(MouseEventKind::ScrollUp, chrome.footer.y + 1));
        assert_eq!(
            state.visual_slots.get(&id).unwrap().chat_scroll,
            3,
            "footer wheel scrolls chat"
        );
        forward_mouse(&mut state, wheel(MouseEventKind::ScrollDown, chrome.footer.y + 1));
        assert_eq!(state.visual_slots.get(&id).unwrap().chat_scroll, 0, "back to tail");
        assert_eq!(state.visual_slots.get(&id).unwrap().scroll_x, 0);
        assert_eq!(state.visual_slots.get(&id).unwrap().scroll_y, 0, "diagram untouched");
        assert!(state.manager.remove(id));
    }

    #[cfg(feature = "visual")]
    #[test]
    fn visual_wheel_pans_and_button_clicks_zoom() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
        let mut state = AppState::new();
        state.apply(AppEvent::Resize(40, 180));
        let id = open_visual_overlay(&mut state);
        let areas = ui::layout::chrome_areas(ratatui::layout::Rect::new(0, 0, 180, 40));
        let content = ui::layout::pane_content_area(&areas);
        let chrome = ui::visual::visual_chrome(
            content,
            state.pill_tabs,
            state.visual_footer_rows(id, content.height),
        );
        let (area_cols, area_rows) = (chrome.image.width, chrome.image.height);
        for _ in 0..3 {
            assert!(state.visual_zoom(id, ui::visual::VisualButton::ZoomIn, area_cols, area_rows, 8.0, 16.0));
        }
        let zoomed = state.visual_slots.get(&id).unwrap().zoom;
        let wheel = |kind| MouseEvent {
            kind, column: chrome.image.x + 2, row: chrome.image.y + 2,
            modifiers: KeyModifiers::NONE,
        };
        forward_mouse(&mut state, wheel(MouseEventKind::ScrollDown));
        assert_eq!(state.visual_slots.get(&id).unwrap().scroll_y, 3);
        forward_mouse(&mut state, wheel(MouseEventKind::ScrollUp));
        assert_eq!(state.visual_slots.get(&id).unwrap().scroll_y, 0);
        // Zoom-in button click; a click on the image itself is dead.
        let click = |column, row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column, row,
            modifiers: KeyModifiers::NONE,
        };
        forward_mouse(&mut state, click(chrome.zoom_in.x, chrome.zoom_in.y));
        assert!(state.visual_slots.get(&id).unwrap().zoom > zoomed);
        let zoomed = state.visual_slots.get(&id).unwrap().zoom;
        forward_mouse(&mut state, click(chrome.image.x + 2, chrome.image.y + 2));
        assert_eq!(state.visual_slots.get(&id).unwrap().zoom, zoomed);
        forward_mouse(&mut state, click(chrome.zoom_out.x, chrome.zoom_out.y));
        assert!(state.visual_slots.get(&id).unwrap().zoom < zoomed);
        assert!(state.visual_tab_focused(), "clicks stay in the tab");
        assert!(state.manager.remove(id));
    }

}
