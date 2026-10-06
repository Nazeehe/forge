    use super::super::test_support::*;
    use crate::tui::input::{prefix_key, InputRouter};
    use crate::tui::keys::handle_key_at;
    use crossterm::event;

    #[test]
    fn prefix_d_opens_the_writer_tab() {
        let (mut state, id, dir) = writer_agent();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, prefix_key(), now);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('d')), now);
        assert_eq!(state.writer_overlay_active(), Some(id));
        assert!(state.overlay_view.is_some(), "overlay tab opens");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn typing_reaches_the_editor_not_the_pane() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('!')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "!aaa bbb"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_state_keys_drive_the_prompt() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "aaa").unwrap();
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // Esc cancels a fresh prompt before anything opens.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('n')), now);
        assert!(state.writers.get(&id).unwrap().open_prompt.is_some());
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Esc), now);
        assert!(state.writers.get(&id).unwrap().open_prompt.is_none());
        // `o` opens the prompt, typing fills it, Enter submits.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('o')), now);
        assert!(state.writers.get(&id).unwrap().open_prompt.is_some());
        for code in [
            event::KeyCode::Char('d'),
            event::KeyCode::Char('.'),
            event::KeyCode::Char('m'),
            event::KeyCode::Char('d'),
        ] {
            handle_key_at(&mut state, &mut router, key(code), now);
        }
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Enter), now);
        let session = state.writers.get(&id).unwrap();
        assert!(session.open_prompt.is_none(), "prompt closes on open");
        assert!(session.editor.is_some(), "editor builds");
        assert_eq!(session.doc.as_ref().unwrap().text, "aaa");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tab_indents_in_editor_while_f6_cycles_focus() {
        use crate::app::writer::WriterFocus;
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        // F6 below must reach Chat: the panel owns it (A1).
        state.writer_toggle_assistant(id);
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // Tab in the editor indents (two spaces) instead of cycling.
        handle_key_at(&mut state, &mut router, tab(), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "  aaa bbb",
            "tab inserts two spaces at the cursor"
        );
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Editor);
        // Shift+Tab outdents back.
        let shift_tab = event::KeyEvent::new(
            event::KeyCode::Tab,
            event::KeyModifiers::SHIFT,
        );
        handle_key_at(&mut state, &mut router, shift_tab, now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "aaa bbb"
        );
        // F6 owns focus cycling now; Tab elsewhere is ignored.
        let f6 = key(event::KeyCode::F(6));
        handle_key_at(&mut state, &mut router, f6, now);
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Chat);
        handle_key_at(&mut state, &mut router, tab(), now);
        assert_eq!(
            state.writers.get(&id).unwrap().focus,
            WriterFocus::Chat,
            "tab never cycles, even in the chat box"
        );
        handle_key_at(&mut state, &mut router, f6, now);
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Thread);
        handle_key_at(&mut state, &mut router, f6, now);
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Editor);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prompt_keys_edit_at_the_cursor() {
        use crossterm::event::KeyCode;
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        state.writer_toolbar_open(id);
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        for c in "ac".chars() {
            handle_key_at(&mut state, &mut router, key(KeyCode::Char(c)), now);
        }
        handle_key_at(&mut state, &mut router, key(KeyCode::Left), now);
        handle_key_at(&mut state, &mut router, key(KeyCode::Char('b')), now);
        let buffer = state.writers.get(&id).unwrap().open_prompt.as_ref().unwrap().buffer.clone();
        assert_eq!(buffer, "abc", "keys reach the prompt cursor");
        handle_key_at(&mut state, &mut router, key(KeyCode::Delete), now);
        let buffer = state.writers.get(&id).unwrap().open_prompt.as_ref().unwrap().buffer.clone();
        assert_eq!(buffer, "ab", "delete under the cursor");
        handle_key_at(&mut state, &mut router, key(KeyCode::Home), now);
        handle_key_at(&mut state, &mut router, ctrl(KeyCode::Char('a')), now);
        handle_key_at(&mut state, &mut router, key(KeyCode::Char('z')), now);
        let prompt = state.writers.get(&id).unwrap().open_prompt.as_ref().unwrap().clone();
        assert_eq!(prompt.buffer, "z", "ctrl+a types to replace through keys");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chat_keys_home_end_and_ctrl_a() {
        use crossterm::event::KeyCode;
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        state.writer_toggle_assistant(id);
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::F(6)), now);
        for c in "hi".chars() {
            handle_key_at(&mut state, &mut router, key(KeyCode::Char(c)), now);
        }
        handle_key_at(&mut state, &mut router, key(KeyCode::Home), now);
        assert_eq!(state.writers.get(&id).unwrap().chat_cursor, 0);
        handle_key_at(&mut state, &mut router, key(KeyCode::End), now);
        assert_eq!(state.writers.get(&id).unwrap().chat_cursor, 2);
        handle_key_at(&mut state, &mut router, ctrl(KeyCode::Char('a')), now);
        assert!(state.writers.get(&id).unwrap().chat_select_all);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chat_box_edits_and_enter_sends() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        // The chat box lives in the panel: F6 only reaches it while
        // the panel is visible (A1).
        state.writer_toggle_assistant(id);
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // F6 reaches the chat box now that Tab never cycles.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::F(6)), now);
        for c in ['h', 'i', '?'] {
            handle_key_at(&mut state, &mut router, key(event::KeyCode::Char(c)), now);
        }
        assert_eq!(state.writers.get(&id).unwrap().chat_input, "hi?");
        // The editor never saw the typing.
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "aaa bbb"
        );
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Enter), now);
        let session = state.writers.get(&id).unwrap();
        assert!(session.chat_input.is_empty(), "chat clears on send");
        assert!(session.queue.back().expect("queued").contains("action=chat"));
        // Esc returns to the editor.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Esc), now);
        assert_eq!(
            state.writers.get(&id).unwrap().focus,
            crate::app::writer::WriterFocus::Editor
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn thread_keys_select_accept_and_reject() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        let doc = state.writers.get(&id).unwrap().doc.clone().unwrap();
        let session = state.writers.get_mut(&id).unwrap();
        session
            .proposals
            .propose(&doc, None, 0..3, "AAA".to_string(), None)
            .unwrap();
        session
            .proposals
            .propose(&doc, None, 4..7, "BBB".to_string(), None)
            .unwrap();
        state.open_writer_overlay();
        // Chat and thread live in the panel: F6 only reaches them
        // while it is visible (A1).
        state.writer_toggle_assistant(id);
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // F6 twice reaches the thread now that Tab never cycles.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::F(6)), now);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::F(6)), now);
        // `j` selects the first pending proposal, Enter accepts it.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('j')), now);
        let first = state.writers.get(&id).unwrap().selected_proposal;
        assert!(first.is_some(), "j selects");
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Enter), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "AAA bbb"
        );
        // `j` then `x` rejects the second.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('j')), now);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('x')), now);
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.proposals.pending_count(), 0, "both settled");
        assert_eq!(session.doc.as_ref().unwrap().text, "AAA bbb");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ctrl_s_saves_and_alt_r_rephrases() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('!')), now);
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('s')), now);
        assert_eq!(std::fs::read_to_string(dir.join("d.md")).unwrap(), "!aaa bbb");
        assert_eq!(state.writers.get(&id).unwrap().error, None);
        // Alt+R rephrases the paragraph under the cursor (Ctrl+R is
        // the editor's redo and stays untouched).
        let alt_r = event::KeyEvent::new(event::KeyCode::Char('r'), event::KeyModifiers::ALT);
        handle_key_at(&mut state, &mut router, alt_r, now);
        let session = state.writers.get(&id).unwrap();
        assert!(session.queue.back().expect("queued").contains("action=rephrase"));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ctrl_z_undo_and_ctrl_y_redo_with_ctrl_r_dead() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('!')), now);
        // CUA undo/redo: Ctrl+Z undoes, Ctrl+Y redoes.
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('z')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "aaa bbb"
        );
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('y')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "!aaa bbb",
            "Ctrl+Y redoes"
        );
        // Ctrl+Shift+Z redoes too.
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('z')), now);
        let shift_ctrl_z = event::KeyEvent::new(
            event::KeyCode::Char('Z'),
            event::KeyModifiers::CONTROL | event::KeyModifiers::SHIFT,
        );
        handle_key_at(&mut state, &mut router, shift_ctrl_z, now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "!aaa bbb",
            "Ctrl+Shift+Z redoes"
        );
        // Ctrl+R is dead under CUA (it was emacs redo); Alt+R rephrases.
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('z')), now);
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('r')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "aaa bbb",
            "Ctrl+R redoes nothing"
        );
        let alt_r = event::KeyEvent::new(event::KeyCode::Char('r'), event::KeyModifiers::ALT);
        handle_key_at(&mut state, &mut router, alt_r, now);
        let session = state.writers.get(&id).unwrap();
        assert!(session.queue.back().expect("queued").contains("action=rephrase"));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn emacs_isms_are_dead_under_cua() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // Emacs kill/yank/move bindings retired with the map: the
        // text and cursor must not move.
        for c in ['k', 'e', 'd', 'u', 'v', 'g'] {
            handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char(c)), now);
        }
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.doc.as_ref().unwrap().text, "aaa bbb");
        assert_eq!(cursor_offset(&state, id), 0, "cursor unmoved");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ctrl_a_selects_everything_for_rephrase() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('a')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            Some(0..7),
            "whole document selected"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ctrl_backspace_and_ctrl_delete_kill_cua_words() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // Cursor starts at 0; forward-kill eats "aaa ".
        handle_key_at(
            &mut state,
            &mut router,
            ctrl(event::KeyCode::Delete),
            now,
        );
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "bbb"
        );
        // One undo restores the word (single capture).
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('z')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "aaa bbb"
        );
        // Backward-kill from the end eats "bbb", keeping the space.
        let end = key(event::KeyCode::End);
        handle_key_at(&mut state, &mut router, end, now);
        handle_key_at(
            &mut state,
            &mut router,
            ctrl(event::KeyCode::Backspace),
            now,
        );
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "aaa "
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn alt_a_toggles_the_assistant_panel() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        let alt_a = event::KeyEvent::new(event::KeyCode::Char('a'), event::KeyModifiers::ALT);
        handle_key_at(&mut state, &mut router, alt_a, now);
        assert!(state.writers.get(&id).unwrap().panel_visible, "alt+a shows");
        handle_key_at(&mut state, &mut router, alt_a, now);
        assert!(!state.writers.get(&id).unwrap().panel_visible, "alt+a hides");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recent_keys_navigate_and_enter_opens() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("alpha.md"), "A\n").unwrap();
        std::fs::write(dir.join("beta.md"), "B\n").unwrap();
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Down), now);
        assert_eq!(state.writers.get(&id).unwrap().recent_sel, 1);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Up), now);
        assert_eq!(state.writers.get(&id).unwrap().recent_sel, 0);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Down), now);
        // Same-nanosecond mtimes sort either way: read the row, then open it.
        let target = state.writers.get(&id).unwrap().recent_cache[1].rel.clone();
        let want = std::fs::read_to_string(dir.join(&target)).unwrap();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Enter), now);
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.doc.as_ref().unwrap().path_rel, target);
        assert_eq!(session.doc.as_ref().unwrap().text, want, "enter opens the selection");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prompt_tab_completes_enter_submits_and_buttons_fire() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("alpha.md"), "A\n").unwrap();
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // The Start pill opens the New prompt through the real path.
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "*New document");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        for c in "alp".chars() {
            handle_key_at(&mut state, &mut router, key(event::KeyCode::Char(c)), now);
        }
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Tab), now);
        assert_eq!(
            state.writers.get(&id).unwrap().open_prompt.as_ref().unwrap().buffer,
            "alpha.md",
            "tab completes from the scan"
        );
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Enter), now);
        // New on an existing file offers Open instead; the confirm
        // pill (default-marked, unlike the toolbar twin) fires it.
        assert!(state.writers.get(&id).unwrap().pending_confirm.is_some());
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "*Open");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.doc.as_ref().unwrap().path_rel, "alpha.md");
        assert_eq!(session.doc.as_ref().unwrap().text, "A\n");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ctrl_twins_work_from_the_doc_state() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('n')), now);
        assert!(state.writers.get(&id).unwrap().open_prompt.as_ref().is_some_and(
            |p| p.kind == crate::app::writer::PromptKind::New
        ));
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Esc), now);
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('o')), now);
        assert!(state.writers.get(&id).unwrap().open_prompt.as_ref().is_some_and(
            |p| p.kind == crate::app::writer::PromptKind::Open
        ));
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Esc), now);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('!')), now);
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('s')), now);
        assert_eq!(std::fs::read_to_string(dir.join("d.md")).unwrap(), "!aaa bbb");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn esc_clears_confirms_and_closes_the_menu() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('!')), now);
        state.writer_close_doc(id);
        assert!(state.writers.get(&id).unwrap().pending_confirm.is_some());
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Esc), now);
        assert!(state.writers.get(&id).unwrap().pending_confirm.is_none(), "esc clears");
        assert!(state.writers.get(&id).unwrap().doc.is_some(), "nothing closed");
        // Narrow menu dismisses the same way.
        state.term_size = (30, 80);
        state.writers.get_mut(&id).unwrap().more_open = true;
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Esc), now);
        assert!(!state.writers.get(&id).unwrap().more_open, "menu closes");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn shift_ctrl(code: event::KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(
            code,
            event::KeyModifiers::CONTROL | event::KeyModifiers::SHIFT,
        )
    }

    fn doc_text(state: &crate::app::AppState, id: crate::session::SessionId) -> String {
        state.writers.get(&id).unwrap().doc.as_ref().unwrap().text.clone()
    }

    #[test]
    fn ctrl_c_copies_selection_and_queues_osc52() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // Select "bbb" at the end through the real key path.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::End), now);
        for _ in 0..3 {
            handle_key_at(&mut state, &mut router, shift(event::KeyCode::Left), now);
        }
        assert_eq!(state.writers.get(&id).unwrap().selection, Some(4..7));
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('c')), now);
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.clip.0.borrow().clone(), "bbb", "internal clipboard");
        assert_eq!(session.selection, Some(4..7), "copy keeps the selection");
        let queued = state.take_osc52();
        assert_eq!(queued.len(), 1, "one sequence queued");
        assert_eq!(
            queued[0], "\x1b]52;c;YmJi\x07",
            "OSC 52 carries base64(bbb)"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ctrl_c_without_selection_queues_nothing() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('c')), now);
        assert!(state.take_osc52().is_empty(), "no selection, no output");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ctrl_x_cuts_in_one_undo_step() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::End), now);
        for _ in 0..3 {
            handle_key_at(&mut state, &mut router, shift(event::KeyCode::Left), now);
        }
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('x')), now);
        assert_eq!(doc_text(&state, id), "aaa ", "cut removes the range");
        assert_eq!(
            state.writers.get(&id).unwrap().clip.0.borrow().clone(),
            "bbb",
            "cut copies first"
        );
        assert_eq!(state.take_osc52().len(), 1, "cut announces too");
        // One undo step restores the whole cut.
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('z')), now);
        assert_eq!(doc_text(&state, id), "aaa bbb", "single undo restores");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ctrl_v_replaces_the_selection() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // Cut "bbb" to fill the clipboard, then select "aaa".
        handle_key_at(&mut state, &mut router, key(event::KeyCode::End), now);
        for _ in 0..3 {
            handle_key_at(&mut state, &mut router, shift(event::KeyCode::Left), now);
        }
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('x')), now);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Home), now);
        for _ in 0..3 {
            handle_key_at(&mut state, &mut router, shift(event::KeyCode::Right), now);
        }
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('v')), now);
        assert_eq!(doc_text(&state, id), "bbb ", "paste replaces the selection");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ctrl_v_with_empty_clipboard_is_a_noop() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('v')), now);
        assert_eq!(doc_text(&state, id), "aaa bbb", "nothing pasted");
        assert!(state.take_osc52().is_empty(), "paste emits nothing");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn shift_home_end_page_and_ctrl_shift_bounds_select() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "line one\nline two\nline three\n").unwrap();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // Paint once so page moves know the viewport height.
        paint_full(&mut state, id);
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // Line two, four cells in: Shift+Home selects to its start.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Down), now);
        for _ in 0..4 {
            handle_key_at(&mut state, &mut router, key(event::KeyCode::Right), now);
        }
        handle_key_at(&mut state, &mut router, shift(event::KeyCode::Home), now);
        assert_eq!(state.writers.get(&id).unwrap().selection, Some(9..13));
        // Shift+End extends from the same anchor to the line end.
        handle_key_at(&mut state, &mut router, shift(event::KeyCode::End), now);
        assert_eq!(state.writers.get(&id).unwrap().selection, Some(13..17));
        // Ctrl+Shift+Home reaches the document head.
        handle_key_at(
            &mut state,
            &mut router,
            shift_ctrl(event::KeyCode::Home),
            now,
        );
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            Some(0..13),
            "extends to the head"
        );
        // Ctrl+Shift+End reaches the document end.
        handle_key_at(
            &mut state,
            &mut router,
            shift_ctrl(event::KeyCode::End),
            now,
        );
        let total = doc_text(&state, id).chars().count();
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            Some(13..total),
            "extends to the end"
        );
        // Shift+PageUp/PageDown move by the painted height.
        handle_key_at(&mut state, &mut router, shift(event::KeyCode::PageUp), now);
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            Some(0..13),
            "page up selects to the head"
        );
        handle_key_at(
            &mut state,
            &mut router,
            shift(event::KeyCode::PageDown),
            now,
        );
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            Some(13..total),
            "page down selects back to the end"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn alt(code: event::KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, event::KeyModifiers::ALT)
    }

    #[test]
    fn alt_p_toggles_preview_and_needs_a_doc() {
        let (mut state, id, dir) = writer_agent();
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // No doc: Alt+P is a silent no-op, like the Assistant key.
        handle_key_at(&mut state, &mut router, alt(event::KeyCode::Char('p')), now);
        assert!(!state.writers.get(&id).unwrap().preview);
        open_doc(&mut state, id, "d.md");
        handle_key_at(&mut state, &mut router, alt(event::KeyCode::Char('p')), now);
        assert!(state.writers.get(&id).unwrap().preview, "preview on");
        handle_key_at(&mut state, &mut router, alt(event::KeyCode::Char('p')), now);
        assert!(!state.writers.get(&id).unwrap().preview, "preview off");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preview_scroll_keys_clamp_and_home_end() {
        let (mut state, id, dir) = writer_agent();
        let text: String = (0..60).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.join("d.md"), &text).unwrap();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        paint_full(&mut state, id);
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, alt(event::KeyCode::Char('p')), now);
        for _ in 0..3 {
            handle_key_at(&mut state, &mut router, key(event::KeyCode::Down), now);
        }
        assert_eq!(state.writers.get(&id).unwrap().preview_scroll, 3);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Up), now);
        assert_eq!(state.writers.get(&id).unwrap().preview_scroll, 2);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::PageDown), now);
        assert!(state.writers.get(&id).unwrap().preview_scroll > 2, "page moves");
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Home), now);
        assert_eq!(state.writers.get(&id).unwrap().preview_scroll, 0);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::End), now);
        let scrolled = state.writers.get(&id).unwrap().preview_scroll;
        assert!(scrolled > 0, "end reaches the tail");
        // Pinned: scrolling again past the end changes nothing.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::PageDown), now);
        assert_eq!(state.writers.get(&id).unwrap().preview_scroll, scrolled);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preview_ignores_typing_and_esc_exits() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, alt(event::KeyCode::Char('p')), now);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('!')), now);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Enter), now);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Backspace), now);
        assert_eq!(doc_text(&state, id), "aaa bbb", "read-only in preview");
        assert!(state.writers.get(&id).unwrap().preview, "still previewing");
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Esc), now);
        assert!(!state.writers.get(&id).unwrap().preview, "esc exits preview");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ctrl_s_records_the_save_note() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "aaa bbb").unwrap();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('!')), now);
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('s')), now);
        assert_eq!(state.writers.get(&id).unwrap().save_note.as_deref(), Some("saved"));
        let buf = paint_full(&mut state, id);
        let text: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("saved"), "status shows the save");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prefix_escapes_while_editing() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // The prefix still reaches the router: Ctrl-b t can leave.
        handle_key_at(&mut state, &mut router, prefix_key(), now);
        assert!(router.is_pending(), "prefix escapes the editor");
        // ... and the editor never saw the chord.
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "aaa bbb"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }
