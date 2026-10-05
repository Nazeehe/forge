    use super::super::test_support::*;
    use crate::app::AppState;
    use crate::tui::input::InputRouter;
    use crate::tui::keys::handle_key_at;
    use crossterm::event;

    #[test]
    fn arrows_cross_line_boundaries_through_real_keys() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "ab\ncd").unwrap();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        assert_eq!(cursor_offset(&state, id), 0);
        for _ in 0..6 {
            handle_key_at(&mut state, &mut router, key(event::KeyCode::Right), now);
        }
        assert_eq!(cursor_offset(&state, id), 5, "right crosses \\n to doc end");
        for _ in 0..4 {
            handle_key_at(&mut state, &mut router, key(event::KeyCode::Left), now);
        }
        assert_eq!(cursor_offset(&state, id), 1, "left crosses \\n back");
        assert_eq!(state.writers.get(&id).unwrap().selection, None);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn click_then_typing_still_inserts() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // Paint once so EdTUI learns its screen area, then click.
        let buf = paint_full(&mut state, id);
        let (mut x, y) = find_text(&buf, "bbb");
        x += 2;
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert_eq!(cursor_offset(&state, id), 6);
        // The click must not strand the editor in a modal state:
        // typing inserts at the click point.
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('X')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "aaa bbXb"
        );
        assert_eq!(
            state.writers.get(&id).unwrap().editor.as_ref().unwrap().mode,
            edtui::EditorMode::Insert
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn move_keys_work_through_real_keys() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "l1\nl2\nl3\nl4").unwrap();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::End), now);
        assert_eq!(cursor_offset(&state, id), 2);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Home), now);
        assert_eq!(cursor_offset(&state, id), 0);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Down), now);
        assert_eq!(cursor_offset(&state, id), 3);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Right), now);
        assert_eq!(cursor_offset(&state, id), 4);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Up), now);
        assert_eq!(cursor_offset(&state, id), 1);
        handle_key_at(&mut state, &mut router, ctrl_key(event::KeyCode::End), now);
        assert_eq!(cursor_offset(&state, id), 11);
        handle_key_at(&mut state, &mut router, ctrl_key(event::KeyCode::Home), now);
        assert_eq!(cursor_offset(&state, id), 0);
        handle_key_at(&mut state, &mut router, ctrl_key(event::KeyCode::Right), now);
        assert_eq!(cursor_offset(&state, id), 3, "next word start");
        handle_key_at(&mut state, &mut router, ctrl_key(event::KeyCode::Right), now);
        assert_eq!(cursor_offset(&state, id), 6);
        handle_key_at(&mut state, &mut router, ctrl_key(event::KeyCode::Left), now);
        assert_eq!(cursor_offset(&state, id), 3);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn page_keys_move_by_visible_height() {
        let (mut state, id, dir) = writer_agent();
        let text: String = (0..60).map(|n| format!("line {n:02}")).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.join("long.md"), &text).unwrap();
        open_doc(&mut state, id, "long.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // Paint once so the editor learns its screen area. Page keys
        // move by the painted height (below the toolbar rule), which
        // the paint reports back.
        paint_full(&mut state, id);
        let page = state.writers.get(&id).unwrap().editor_rows as usize;
        assert!(page > 0, "paint reports a visible height");
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::PageDown), now);
        assert_eq!(cursor_offset(&state, id), page * "line 00\n".len());
        handle_key_at(&mut state, &mut router, key(event::KeyCode::PageUp), now);
        assert_eq!(cursor_offset(&state, id), 0);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn shift_move_keys_select_through_real_keys() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "aaa bbb\nccc").unwrap();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, shift(event::KeyCode::Right), now);
        handle_key_at(&mut state, &mut router, shift(event::KeyCode::Right), now);
        assert_eq!(state.writers.get(&id).unwrap().selection, Some(0..2));
        // Plain arrows collapse the gesture.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Right), now);
        assert_eq!(state.writers.get(&id).unwrap().selection, None);
        // Shift+End/Home select to the line bounds.
        handle_key_at(&mut state, &mut router, shift(event::KeyCode::End), now);
        assert_eq!(state.writers.get(&id).unwrap().selection, Some(3..7));
        handle_key_at(&mut state, &mut router, shift(event::KeyCode::Home), now);
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            Some(0..3),
            "anchor stays, cursor to line start"
        );
        // Shift+Ctrl+Home/End reach the document bounds.
        handle_key_at(
            &mut state,
            &mut router,
            event::KeyEvent::new(
                event::KeyCode::End,
                event::KeyModifiers::SHIFT | event::KeyModifiers::CONTROL,
            ),
            now,
        );
        assert_eq!(state.writers.get(&id).unwrap().selection, Some(3..11));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn viewport_follows_cursor_to_doc_end() {
        let (mut state, id, dir) = writer_agent();
        let text: String = (0..60).map(|n| format!("line {n:02}")).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.join("long.md"), &text).unwrap();
        open_doc(&mut state, id, "long.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        paint_full(&mut state, id);
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, ctrl_key(event::KeyCode::End), now);
        let buf = paint_full(&mut state, id);
        let editor = editor_area(&state, id);
        let tail: String = (editor.x..editor.x + 8)
            .map(|cx| buf[(cx, editor.y + editor.height - 1)].symbol())
            .collect();
        assert!(tail.starts_with("line 59"), "last line visible: {tail:?}");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gutter_marks_every_wrapped_row_of_proposal() {
        use crate::ui::theme::{style, Role};
        let (mut state, id, dir) = writer_agent();
        let text = format!("aaa\n{}\nzzz", "x".repeat(300));
        std::fs::write(dir.join("wrap.md"), &text).unwrap();
        open_doc(&mut state, id, "wrap.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // Proposal over the middle of the long wrapped line.
        let doc = state.writers.get(&id).unwrap().doc.clone().unwrap();
        let pid = state
            .writers
            .get_mut(&id)
            .unwrap()
            .proposals
            .propose(&doc, None, 100..200, "y".repeat(100), None)
            .unwrap();
        state.writer_select_proposal(id, pid);
        // Cursor to the doc end so wrapped rows sit above it.
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, ctrl_key(event::KeyCode::End), now);
        let buf = paint_full(&mut state, id);
        let area = crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, 120, 30));
        let visible = state.writers.get(&id).is_some_and(|s| s.panel_visible);
        let layout = crate::ui::writer::writer_layout(area, visible);
        let marks: Vec<u16> = (layout.editor.y..layout.editor.y + layout.editor.height)
            .filter(|y| buf[(layout.gutter.x, *y)].symbol() == "▌")
            .collect();
        // EdTUI patches the highlight over the base style (base
        // wins ties), so only the background is an exact oracle.
        let want_bg = style(Role::TabActive).bg;
        let lit: Vec<u16> = (layout.editor.y..layout.editor.y + layout.editor.height)
            .filter(|y| {
                (layout.editor.x..layout.editor.x + layout.editor.width)
                    .any(|x| buf[(x, *y)].style().bg == want_bg)
            })
            .collect();
        assert!(!lit.is_empty(), "highlight paints");
        assert_eq!(marks, lit, "every highlighted screen row is marked");
        // The cursor sits at the doc end past the wrapped block: the
        // paint must keep it inside the editor (viewport follows,
        // including wrapped lines).
        let cursor = {
            use ratatui::{backend::TestBackend, Terminal};
            let area =
                crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, 120, 30));
            let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
            let activity = state.manager.get(id).map(|rec| rec.activity).unwrap();
            let mut pos = None;
            terminal
                .draw(|f| {
                    pos = crate::ui::writer::paint(
                        f,
                        area,
                        state.writers.get_mut(&id).unwrap(),
                        activity,
                        120,
                    );
                })
                .unwrap();
            pos
        };
        let cursor = cursor.expect("cursor paints");
        let editor = editor_area(&state, id);
        assert!(
            cursor.x >= editor.x
                && cursor.x < editor.x + editor.width
                && cursor.y >= editor.y
                && cursor.y < editor.y + editor.height,
            "cursor visible in the editor: {cursor:?}"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn click_after_selection_then_typing_inserts() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // Shift-select (Visual), then click in the editor: EdTUI
        // parks Down in vim Normal while Visual is live.
        handle_key_at(&mut state, &mut router, shift(event::KeyCode::Right), now);
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "bbb");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('X')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "aaa Xbbb",
            "click recovers Insert, typing inserts at the click"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn esc_clears_selection_and_restores_insert() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, shift(event::KeyCode::Right), now);
        assert!(state.writers.get(&id).unwrap().selection.is_some());
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Esc), now);
        assert_eq!(state.writers.get(&id).unwrap().selection, None);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('Y')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "aYaa bbb",
            "cursor stays at the active end"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn typing_over_a_live_selection_replaces_it() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, shift(event::KeyCode::Right), now);
        handle_key_at(&mut state, &mut router, shift(event::KeyCode::Right), now);
        handle_key_at(&mut state, &mut router, shift(event::KeyCode::Right), now);
        assert_eq!(state.writers.get(&id).unwrap().selection, Some(0..3));
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('X')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "X bbb"
        );
        assert_eq!(state.writers.get(&id).unwrap().selection, None);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn right_n(state: &mut AppState, n: usize) {
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        for _ in 0..n {
            handle_key_at(state, &mut router, key(event::KeyCode::Right), now);
        }
    }

    fn down(state: &mut AppState) {
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(state, &mut router, key(event::KeyCode::Down), now);
    }

    #[test]
    fn goal_column_survives_short_lines() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "0123456789AB\nabc\n0123456789CD").unwrap();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        right_n(&mut state, 10);
        assert_eq!(cursor_offset(&state, id), 10);
        down(&mut state);
        assert_eq!(cursor_offset(&state, id), 16, "short line clamps");
        down(&mut state);
        assert_eq!(cursor_offset(&state, id), 27, "goal column returns");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn vertical_moves_follow_wrapped_screen_rows() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "seed").unwrap();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let w = editor_area(&state, id).width as usize;
        assert!(w > 10, "sane editor width: {w}");
        let text = format!("{}\nzzz", "y".repeat(2 * w + 5));
        std::fs::write(dir.join("wrap.md"), &text).unwrap();
        open_doc(&mut state, id, "wrap.md");
        state.open_writer_overlay();
        // Paint once: the real loop always paints before input, and
        // the paint reports the editor width vertical moves wrap by.
        paint_full(&mut state, id);
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Down), now);
        assert_eq!(cursor_offset(&state, id), w, "second wrapped row");
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Down), now);
        assert_eq!(cursor_offset(&state, id), 2 * w, "third wrapped row");
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Down), now);
        assert_eq!(cursor_offset(&state, id), 2 * w + 6, "next line");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn goal_resets_on_horizontal_move() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(
            dir.join("d.md"),
            "0123456789ABCDEFGHIJ\n0123456789ABCDEFGHIJ\n0123456789ABCDEFGHIJ",
        )
        .unwrap();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        right_n(&mut state, 10);
        down(&mut state);
        assert_eq!(cursor_offset(&state, id), 31);
        // One step left: the goal is gone, Down keeps col 9.
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Left), now);
        assert_eq!(cursor_offset(&state, id), 30);
        down(&mut state);
        assert_eq!(cursor_offset(&state, id), 51, "col 9, not the old 10");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }


