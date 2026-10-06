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
    fn preview_stays_disabled_without_a_doc() {
        let (mut state, id, dir) = writer_agent();
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Preview");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        // E8 enables Preview only with a doc: without one the dimmed
        // pill dies silently.
        let session = state.writers.get(&id).unwrap();
        assert!(!session.preview, "no preview without a doc");
        assert!(session.open_prompt.is_none(), "no prompt");
        assert!(session.doc.is_none(), "no doc opened");
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
    fn hover_motion_never_selects() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // Paint once so EdTUI learns its screen area.
        let before = paint_full(&mut state, id);
        let (x, y) = find_text(&before, "bbb");
        let cursor = state.writers.get(&id).unwrap().editor.as_ref().unwrap().cursor;
        // Plain motion, then the Ghostty form: motion arriving as a
        // button-less Drag(Left). Neither may select or repaint.
        for kind in [
            event::MouseEventKind::Moved,
            event::MouseEventKind::Drag(event::MouseButton::Left),
        ] {
            super::super::handle_writer_mouse(
                &mut state,
                event::MouseEvent {
                    kind,
                    column: x,
                    row: y,
                    modifiers: event::KeyModifiers::NONE,
                },
            );
        }
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.selection, None, "hover leaves no selection");
        assert_eq!(
            session.editor.as_ref().unwrap().cursor,
            cursor,
            "hover never moves the cursor"
        );
        assert_eq!(
            session.editor.as_ref().unwrap().mode,
            edtui::EditorMode::Insert,
            "hover never leaves Insert"
        );
        assert_eq!(paint_full(&mut state, id), before, "hover repaints nothing");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn down_drag_up_selects_and_later_motion_changes_nothing() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "bbb");
        let at = |kind| event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: event::KeyModifiers::NONE,
        };
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        super::super::handle_writer_mouse(
            &mut state,
            at(event::MouseEventKind::Drag(event::MouseButton::Left)),
        );
        assert_eq!(state.writers.get(&id).unwrap().selection, Some(4..5));
        super::super::handle_writer_mouse(
            &mut state,
            at(event::MouseEventKind::Up(event::MouseButton::Left)),
        );
        let selected = state.writers.get(&id).unwrap().selection.clone();
        // The gesture ended at Up: motion here and a stray drag three
        // cells over keep whatever the gesture left behind.
        super::super::handle_writer_mouse(&mut state, at(event::MouseEventKind::Moved));
        super::super::handle_writer_mouse(
            &mut state,
            event::MouseEvent {
                kind: event::MouseEventKind::Drag(event::MouseButton::Left),
                column: x.saturating_add(3),
                row: y,
                modifiers: event::KeyModifiers::NONE,
            },
        );
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            selected,
            "post-gesture motion changes nothing"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn drag_starting_off_the_editor_never_selects() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        // Press the gutter cell left of the editor, then drag into
        // the text: the gesture started off-editor, so nothing selects.
        let gutter = editor_area(&state, id);
        super::super::handle_writer_mouse(
            &mut state,
            click_at(gutter.x.saturating_sub(1), gutter.y),
        );
        let (x, y) = find_text(&buf, "bbb");
        super::super::handle_writer_mouse(
            &mut state,
            event::MouseEvent {
                kind: event::MouseEventKind::Drag(event::MouseButton::Left),
                column: x,
                row: y,
                modifiers: event::KeyModifiers::NONE,
            },
        );
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            None,
            "off-editor press plus drag selects nothing"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn double_click_selects_the_word_and_typing_replaces_it() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "foo bar\n").unwrap();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "bar");
        let press = |kind| event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: event::KeyModifiers::NONE,
        };
        use event::MouseButton::Left;
        use event::MouseEventKind::{Down, Up};
        // Two fast presses on the word: the second selects it.
        super::super::handle_writer_mouse(&mut state, press(Down(Left)));
        super::super::handle_writer_mouse(&mut state, press(Up(Left)));
        super::super::handle_writer_mouse(&mut state, press(Down(Left)));
        super::super::handle_writer_mouse(&mut state, press(Up(Left)));
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            Some(4..7),
            "double-click selects the word"
        );
        // Typing replaces through the real key path.
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('X')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "foo X\n",
            "typing replaces the word"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn triple_click_selects_the_line() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "first\nsecond\n").unwrap();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "second");
        let press = |kind| event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: event::KeyModifiers::NONE,
        };
        use event::MouseButton::Left;
        use event::MouseEventKind::{Down, Up};
        for _ in 0..3 {
            super::super::handle_writer_mouse(&mut state, press(Down(Left)));
            super::super::handle_writer_mouse(&mut state, press(Up(Left)));
        }
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            Some(6..12),
            "triple-click selects the line without its newline"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn drag_past_the_bottom_edge_autoscrolls() {
        let (mut state, id, dir) = writer_agent();
        let text: String = (0..60).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.join("long.md"), &text).unwrap();
        open_doc(&mut state, id, "long.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let area = editor_area(&state, id);
        let (x, y) = find_text(&buf, "line 0");
        // A live H1 press gesture first: stray drags never scroll.
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let before = state
            .writers
            .get(&id)
            .unwrap()
            .editor
            .as_ref()
            .unwrap()
            .viewport_offset()
            .1;
        // Drag onto the first row below the editor (still inside
        // the content area, so dispatch sees it): the viewport
        // moves and the selection grows.
        super::super::handle_writer_mouse(
            &mut state,
            event::MouseEvent {
                kind: event::MouseEventKind::Drag(event::MouseButton::Left),
                column: x,
                row: area.y.saturating_add(area.height),
                modifiers: event::KeyModifiers::NONE,
            },
        );
        let session = state.writers.get(&id).unwrap();
        let after = session.editor.as_ref().unwrap().viewport_offset().1;
        assert!(after > before, "drag past the edge scrolls: {before} -> {after}");
        assert!(
            session.selection.is_some(),
            "the gesture selection grows while scrolling"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preview_pill_fires_from_painted_cells() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // The Preview pill paints in the view group past the │ rule.
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Preview");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(state.writers.get(&id).unwrap().preview, "pill opens preview");
        let buf = paint_full(&mut state, id);
        assert!(
            find_all_text(&buf, "✓Preview").len() == 1,
            "pressed pill carries its marker"
        );
        let (x, y) = find_text(&buf, "Preview");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(!state.writers.get(&id).unwrap().preview, "pill closes preview");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preview_wheel_scrolls_and_clicks_rest() {
        let (mut state, id, dir) = writer_agent();
        let text: String = (0..60).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.join("long.md"), &text).unwrap();
        open_doc(&mut state, id, "long.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        state.writer_toggle_preview(id);
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "line 0");
        // Wheel over the preview scrolls three rows a notch.
        super::super::handle_writer_mouse(
            &mut state,
            event::MouseEvent {
                kind: event::MouseEventKind::ScrollDown,
                column: x,
                row: y,
                modifiers: event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.writers.get(&id).unwrap().preview_scroll, 3);
        // Clicks in the preview move nothing and select nothing.
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.selection, None, "no selection in preview");
        assert_eq!(
            session.editor.as_ref().unwrap().cursor,
            edtui::Index2::new(0, 0),
            "cursor rests while previewing"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn more_menu_toggles_line_numbers() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 80);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "More");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Line numbers");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert!(!session.more_open, "firing closes the menu");
        assert!(session.line_numbers, "numbers toggle on");
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
    fn close_pill_fires_from_every_painted_cell_including_caps() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let content =
            crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, 120, 30));
        let layout = crate::ui::writer::writer_layout(content, false);
        let (rect, _) = crate::ui::writer::toolbar_pill_rects(layout.title, false, state.writers.get(&id).unwrap())
            .into_iter()
            .find(|(_, b)| *b == crate::ui::writer::ToolbarButton::Close)
            .expect("close rect");
        assert!(rect.width > 0, "close paints");
        for x in rect.x..rect.x.saturating_add(rect.width) {
            assert!(
                state.writers.get(&id).unwrap().doc.is_some(),
                "doc open before click at {x}"
            );
            super::super::handle_writer_mouse(&mut state, click_at(x, rect.y));
            assert!(
                state.writers.get(&id).unwrap().doc.is_none(),
                "clean Close fires from painted cell {x}"
            );
            open_doc(&mut state, id, "d.md");
        }
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn toolbar_pill_rects_cover_the_painted_caps_in_every_state() {
        let left = crate::ui::theme::pill_left();
        let right = crate::ui::theme::pill_right();
        for (doc, panel, cols) in
            [(false, false, 120), (false, true, 120), (true, false, 120), (true, true, 120), (true, false, 80)]
        {
            let (mut state, id, dir) = writer_agent();
            if doc {
                open_doc(&mut state, id, "d.md");
            }
            state.term_size = (30, cols);
            state.open_writer_overlay();
            if panel {
                state.writer_toggle_assistant(id);
            }
            let buf = paint_full(&mut state, id);
            let content = crate::walkthrough::walk_area(ratatui::layout::Rect::new(
                0, 0, cols, 30,
            ));
            let layout = crate::ui::writer::writer_layout(content, panel);
            let narrow = crate::ui::writer::toolbar_narrow(cols);
            for (rect, button) in
                crate::ui::writer::toolbar_pill_rects(layout.title, narrow, state.writers.get(&id).unwrap())
            {
                if rect.width == 0 {
                    continue;
                }
                let first = buf[(rect.x, rect.y)].symbol().chars().next();
                let last = buf[(rect.x + rect.width - 1, rect.y)]
                    .symbol()
                    .chars()
                    .next();
                assert_eq!(first, Some(left), "{button:?} left cap at rect start (doc={doc} panel={panel} {cols})");
                assert_eq!(last, Some(right), "{button:?} right cap at rect end (doc={doc} panel={panel} {cols})");
            }
            assert!(state.manager.remove(id));
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn wheel_moves_three_lines_per_notch() {
        let (mut state, id, dir) = writer_agent();
        let text: String = (0..60).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.join("long.md"), &text).unwrap();
        open_doc(&mut state, id, "long.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "line 0");
        super::super::handle_writer_mouse(
            &mut state,
            event::MouseEvent {
                kind: event::MouseEventKind::ScrollDown,
                column: x,
                row: y,
                modifiers: event::KeyModifiers::NONE,
            },
        );
        let after = paint_full(&mut state, id);
        let editor = editor_area(&state, id);
        // Content starts below the toolbar rule and its padding row.
        let head = editor.y.saturating_add(2);
        let first: String = (editor.x..editor.x + 14)
            .map(|cx| after[(cx, head)].symbol())
            .collect();
        let trimmed = first.trim_start().to_string();
        assert!(
            trimmed.starts_with("line 3")
                && !trimmed[6..].starts_with(|c: char| c.is_ascii_digit()),
            "one notch is three lines: {first:?}"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prompt_input_click_places_the_cursor() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        state.writer_toolbar_open(id);
        for c in "abcd".chars() {
            state.writer_prompt_char(id, c);
        }
        let buf = paint_full(&mut state, id);
        let content =
            crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, 120, 30));
        let layout = crate::ui::writer::writer_layout(content, false);
        // Input row sits one below the prompt origin; the target cell
        // is the second buffer char read back off the real paint.
        let y = layout.body.y.saturating_add(crate::ui::writer::DOC_PROMPT_OFF + 1);
        let row: String = (0..120).map(|x| buf[(x, y)].symbol()).collect();
        let prompt_col = row.find("> abcd").unwrap_or_else(|| panic!("painted prompt input: {row:?}"));
        // Byte index is not a cell: count chars (the box border is
        // multibyte) so the target is a painted cell, not hand math.
        let x = (row[..prompt_col].chars().count() + "> ".len() + 1) as u16;
        assert_eq!(buf[(x, y)].symbol(), "b", "target cell holds the painted char (col {prompt_col} row {row:?})");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let cursor = state.writers.get(&id).unwrap().open_prompt.as_ref().unwrap().cursor;
        assert_eq!(cursor, 1, "click lands on the second char");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chat_input_click_places_the_cursor() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        state.writer_toggle_assistant(id);
        for c in "wxyz".chars() {
            state.writer_chat_char(id, c);
        }
        let buf = paint_full(&mut state, id);
        let content =
            crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, 120, 30));
        let layout = crate::ui::writer::writer_layout(content, true);
        let y = layout.chat.y.saturating_add(1);
        let row: String = (0..120).map(|x| buf[(x, y)].symbol()).collect();
        let prompt_col = row.find("> wxyz").expect("painted chat input");
        let x = (row[..prompt_col].chars().count() + "> ".len() + 3) as u16;
        assert_eq!(buf[(x, y)].symbol(), "z", "target cell holds the painted char");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert_eq!(
            state.writers.get(&id).unwrap().chat_cursor,
            3,
            "click lands past three chars"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn doc_prompt_suggestion_click_fills_from_painted_row() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        state.writer_toolbar_open(id);
        for c in "d".chars() {
            state.writer_prompt_char(id, c);
        }
        let buf = paint_full(&mut state, id);
        let body_y = {
            let content = crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, 120, 30));
            crate::ui::writer::writer_layout(content, false).body.y
        };
        // The suggestion paints below the head (rule + padding); the
        // title row also names d.md, so only rows past it count.
        let mut hit = None;
        for y in body_y..30 {
            let row: String = (0..120).map(|x| buf[(x, y)].symbol()).collect();
            if row.contains("d.md") {
                hit = Some((row.find("d.md").unwrap() as u16, y));
                break;
            }
        }
        let (x, y) = hit.expect("painted suggestion row");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let buffer = state.writers.get(&id).unwrap().open_prompt.as_ref().unwrap().buffer.clone();
        assert_eq!(buffer, "d.md", "click fills from the painted row");
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


    #[test]
    fn find_case_pill_toggles_on_click() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "Foo foo").unwrap();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('f')), now);
        for c in "foo".chars() {
            handle_key_at(&mut state, &mut router, key(event::KeyCode::Char(c)), now);
        }
        assert_eq!(state.writers.get(&id).unwrap().find.as_ref().unwrap().matches.len(), 2);
        // Paint once so the click reads off the bar the paint drew.
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "aa");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let find = state.writers.get(&id).unwrap().find.clone().unwrap();
        assert!(find.case_sensitive, "pill toggled");
        assert_eq!(find.matches, vec![4..7], "research narrowed");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_replace_all_pill_fires_on_click() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "aa aa aa").unwrap();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('f')), now);
        for c in "aa".chars() {
            handle_key_at(&mut state, &mut router, key(event::KeyCode::Char(c)), now);
        }
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('h')), now);
        for c in "b".chars() {
            handle_key_at(&mut state, &mut router, key(event::KeyCode::Char(c)), now);
        }
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Replace all");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert_eq!(doc_text(&state, id), "b b b", "all replaced");
        assert_eq!(
            state.writers.get(&id).unwrap().find.as_ref().unwrap().note.as_deref(),
            Some("3 replaced"),
            "count reported"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_replace_toggle_opens_and_collapses_on_click() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "foo bar foo").unwrap();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('f')), now);
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Replace ▸");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(
            state.writers.get(&id).unwrap().find.as_ref().is_some_and(|f| f.replace_open),
            "toggle opened"
        );
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Replace ▾");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(
            state.writers.get(&id).unwrap().find.as_ref().is_some_and(|f| !f.replace_open),
            "toggle collapsed"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }
