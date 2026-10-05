    use super::super::test_support::*;
    use crate::tui::input::InputRouter;
    use crate::tui::keys::handle_key_at;
    use crossterm::event;

    #[test]
    fn one_char_mouse_selection_is_kept() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // Paint once so EdTUI learns its screen area.
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "bbb");
        // Down and drag on the same cell: one char stays selected.
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        super::super::handle_writer_mouse(
            &mut state,
            event::MouseEvent {
                kind: event::MouseEventKind::Drag(event::MouseButton::Left),
                column: x,
                row: y,
                modifiers: event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.writers.get(&id).unwrap().selection, Some(4..5));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_pills_fire_on_click() {
        let (mut state, id, dir) = writer_agent();
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "*New document");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert!(session.open_prompt.as_ref().is_some_and(|p| p.kind == crate::app::writer::PromptKind::New));
        // The prompt replaces Start while open; Esc backs out of it.
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Esc), now);
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Open…");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(state.writers.get(&id).unwrap().open_prompt.as_ref().is_some_and(|p| p.kind == crate::app::writer::PromptKind::Open));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn toolbar_assistant_toggles_the_panel_on_click() {
        let (mut state, id, dir) = writer_agent();
        state.term_size = (30, 120);
        state.open_writer_overlay();
        assert!(!state.writers.get(&id).unwrap().panel_visible, "hidden default");
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Assistant");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(state.writers.get(&id).unwrap().panel_visible, "click toggles on");
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Assistant");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(!state.writers.get(&id).unwrap().panel_visible, "click toggles off");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn disabled_preview_never_fires() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Preview");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert!(session.open_prompt.is_none(), "no prompt");
        assert!(!session.more_open, "no menu");
        assert!(!session.panel_visible, "panel untouched");
        assert!(session.doc.is_some(), "doc untouched");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn toolbar_save_as_and_close_fire_in_doc_state() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // Save-as opens its prompt; Esc backs out of it.
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Save as");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(state.writers.get(&id).unwrap().open_prompt.as_ref().is_some_and(
            |p| p.kind == crate::app::writer::PromptKind::SaveAs
        ));
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Esc), now);
        assert!(state.writers.get(&id).unwrap().open_prompt.is_none(), "esc cancels");
        // Close on a clean doc returns to the empty state at once.
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Close");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(state.writers.get(&id).unwrap().doc.is_none(), "clean closes");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recent_row_click_opens_that_file() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("alpha.md"), "A\n").unwrap();
        std::fs::write(dir.join("beta.md"), "B\n").unwrap();
        state.term_size = (30, 120);
        // Entering the tab scans the folder into the recent cache.
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "beta.md");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.doc.as_ref().unwrap().path_rel, "beta.md");
        assert_eq!(session.doc.as_ref().unwrap().text, "B\n");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn confirm_overwrite_fires_on_click() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("alpha.md"), "A\n").unwrap();
        std::fs::write(dir.join("beta.md"), "stale\n").unwrap();
        // The helper keeps existing bytes, so the doc holds "A\n".
        open_doc(&mut state, id, "alpha.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Save as");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        for c in "beta.md".chars() {
            handle_key_at(&mut state, &mut router, key(event::KeyCode::Char(c)), now);
        }
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Enter), now);
        assert!(state.writers.get(&id).unwrap().pending_confirm.is_some(), "overwrite offered");
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Overwrite");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert_eq!(std::fs::read_to_string(dir.join("beta.md")).unwrap(), "A\n");
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().path_rel,
            "beta.md"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn more_menu_fires_in_narrow_terminals() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 80);
        state.open_writer_overlay();
        // The short toolbar hides Save-as behind More.
        let buf = paint_full(&mut state, id);
        assert_eq!(find_all_text(&buf, "Save as").len(), 0, "no room for save-as");
        let (x, y) = find_text(&buf, "More");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(state.writers.get(&id).unwrap().more_open, "menu opens");
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Save as");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert!(!session.more_open, "firing closes the menu");
        assert!(session.open_prompt.as_ref().is_some_and(
            |p| p.kind == crate::app::writer::PromptKind::SaveAs
        ));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn modal_swallows_writer_clicks() {
        use crate::tui::mouse::forward_mouse;
        let (mut state, id, dir) = writer_agent();
        state.term_size = (30, 120);
        state.open_writer_overlay();
        state.open_quit_confirm();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "*New document");
        forward_mouse(&mut state, click_at(x, y));
        assert!(state.writers.get(&id).unwrap().open_prompt.is_none(), "click died at the modal");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn action_pills_fire_on_click() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // Panel-era UI needs the assistant shown (hidden default).
        state.writer_toggle_assistant(id);
        // Save pill writes the typed text.
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('!')), now);
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Save");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert_eq!(std::fs::read_to_string(dir.join("d.md")).unwrap(), "!aaa bbb");
        // Rephrase pill queues over the whole paragraph (the last
        // "Rephrase" on screen: the chat hint above names it too).
        let buf = paint_full(&mut state, id);
        let (x, y) = *find_all_text(&buf, "Rephrase")
            .last()
            .expect("rephrase pill painted");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert!(session.queue.back().expect("queued").contains("action=rephrase"));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn thread_entry_click_selects_and_accept_pill_fires() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        let doc = state.writers.get(&id).unwrap().doc.clone().unwrap();
        state
            .writers
            .get_mut(&id)
            .unwrap()
            .proposals
            .propose(&doc, None, 0..3, "AAA".to_string(), None)
            .unwrap();
        state.term_size = (30, 120);
        state.open_writer_overlay();
        state.writer_toggle_assistant(id);
        // Click the diff row: selects the proposal.
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "- aaa");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(state.writers.get(&id).unwrap().selected_proposal.is_some());
        // Click Accept: the range is replaced with one undo step.
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Accept");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.doc.as_ref().unwrap().text, "AAA bbb");
        assert_eq!(session.proposals.pending_count(), 0);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn editor_click_moves_cursor_and_drag_selects() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // Paint once so EdTUI learns its screen area.
        let buf = paint_full(&mut state, id);
        let (mut x, y) = find_text(&buf, "bbb");
        // Click the third char: cursor lands on it.
        x += 2;
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert_eq!(
            state.writers.get(&id).unwrap().editor.as_ref().unwrap().cursor,
            edtui::Index2::new(0, 6)
        );
        // Drag back to the first char: selects exactly "bbb".
        super::super::handle_writer_mouse(
            &mut state,
            event::MouseEvent {
                kind: event::MouseEventKind::Drag(event::MouseButton::Left),
                column: x.saturating_sub(2),
                row: y,
                modifiers: event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.writers.get(&id).unwrap().selection, Some(4..7));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wheel_scrolls_the_editor() {
        let (mut state, id, dir) = writer_agent();
        let text: String = (0..60).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.join("long.md"), &text).unwrap();
        open_doc(&mut state, id, "long.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "line 0");
        for _ in 0..5 {
            super::super::handle_writer_mouse(
                &mut state,
                event::MouseEvent {
                    kind: event::MouseEventKind::ScrollDown,
                    column: x,
                    row: y,
                    modifiers: event::KeyModifiers::NONE,
                },
            );
        }
        let after = paint_full(&mut state, id);
        let editor = editor_area(&state, id);
        let first: String = (editor.x..editor.x + 8)
            .map(|cx| after[(cx, editor.y)].symbol())
            .collect();
        assert!(!first.starts_with("line 0"), "scrolled: {first:?}");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detach_chip_click_clears_the_selection() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        state.writer_toggle_assistant(id);
        shift_select(&mut state, id, 3);
        assert!(state.writers.get(&id).unwrap().selection.is_some());
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "(✕)");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert_eq!(state.writers.get(&id).unwrap().selection, None);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }



    #[test]
    fn topbar_click_select_paints_the_empty_state() {
        // The blank-tab bug: entries were only created by Ctrl-b d,
        // so any other selection path painted nothing. Topbar clicks
        // funnel through select_top_tab (tui/mouse.rs), and the real
        // draw closure (tui/mod.rs) paints only with an entry.
        let (mut state, id, dir) = writer_agent();
        state.term_size = (30, 120);
        let slot = state.writer_slot(id).unwrap();
        assert!(state.select_top_tab(slot));
        assert!(
            state.writers.contains_key(&id),
            "entry exists however the tab is selected"
        );
        assert!(state.writer_keys_active());
        let buf = paint_full(&mut state, id);
        find_text(&buf, "*New document");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

