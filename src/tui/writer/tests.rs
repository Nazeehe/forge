    use crate::app::AppState;
    use crate::tui::input::{prefix_key, InputRouter};
    use crate::tui::keys::handle_key_at;
    use crossterm::event;

    fn key(code: event::KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, event::KeyModifiers::NONE)
    }

    fn ctrl(code: event::KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, event::KeyModifiers::CONTROL)
    }

    fn tab() -> event::KeyEvent {
        key(event::KeyCode::Tab)
    }

    fn shift_select(state: &mut AppState, id: crate::session::SessionId, count: usize) {
        let shift = event::KeyModifiers::SHIFT;
        for _ in 0..count {
            state.writer_feed_key(
                id,
                event::KeyEvent::new(event::KeyCode::Right, shift),
            );
        }
    }

    fn writer_agent() -> (AppState, crate::session::SessionId, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "forge-tui-writer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0),
        ));
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

    fn open_doc(state: &mut AppState, id: crate::session::SessionId, name: &str) {
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
    fn tab_cycles_editor_chat_thread() {
        use crate::app::writer::WriterFocus;
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Editor);
        handle_key_at(&mut state, &mut router, tab(), now);
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Chat);
        handle_key_at(&mut state, &mut router, tab(), now);
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Thread);
        handle_key_at(&mut state, &mut router, tab(), now);
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Editor);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chat_box_edits_and_enter_sends() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, tab(), now);
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
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, tab(), now);
        handle_key_at(&mut state, &mut router, tab(), now);
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
    fn ctrl_r_redoes_and_alt_r_rephrases() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('!')), now);
        // Ctrl+U undoes, Ctrl+R redoes: the editor keeps its redo.
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('u')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "aaa bbb"
        );
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('r')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "!aaa bbb",
            "Ctrl+R still redoes"
        );
        // Alt+R rephrases instead.
        let alt_r = event::KeyEvent::new(event::KeyCode::Char('r'), event::KeyModifiers::ALT);
        handle_key_at(&mut state, &mut router, alt_r, now);
        let session = state.writers.get(&id).unwrap();
        assert!(session.queue.back().expect("queued").contains("action=rephrase"));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

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
        super::handle_writer_mouse(&mut state, click_at(x, y));
        super::handle_writer_mouse(
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

    fn click_at(x: u16, y: u16) -> event::MouseEvent {
        event::MouseEvent {
            kind: event::MouseEventKind::Down(event::MouseButton::Left),
            column: x,
            row: y,
            modifiers: event::KeyModifiers::NONE,
        }
    }

    fn find_text(buf: &ratatui::buffer::Buffer, needle: &str) -> (u16, u16) {
        find_all_text(buf, needle)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("needle {needle:?} not painted"))
    }

    fn find_all_text(buf: &ratatui::buffer::Buffer, needle: &str) -> Vec<(u16, u16)> {
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
    fn paint_full(state: &mut AppState, id: crate::session::SessionId) -> ratatui::buffer::Buffer {
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

    #[test]
    fn empty_pills_fire_on_click() {
        let (mut state, id, dir) = writer_agent();
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "*New document");
        super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert!(session.open_prompt.as_ref().is_some_and(|p| p.create));
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Open…");
        super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(state.writers.get(&id).unwrap().open_prompt.as_ref().is_some_and(|p| !p.create));
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
        // Save pill writes the typed text.
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('!')), now);
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Save");
        super::handle_writer_mouse(&mut state, click_at(x, y));
        assert_eq!(std::fs::read_to_string(dir.join("d.md")).unwrap(), "!aaa bbb");
        // Rephrase pill queues over the whole paragraph (the last
        // "Rephrase" on screen: the chat hint above names it too).
        let buf = paint_full(&mut state, id);
        let (x, y) = *find_all_text(&buf, "Rephrase")
            .last()
            .expect("rephrase pill painted");
        super::handle_writer_mouse(&mut state, click_at(x, y));
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
        // Click the diff row: selects the proposal.
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "- aaa");
        super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(state.writers.get(&id).unwrap().selected_proposal.is_some());
        // Click Accept: the range is replaced with one undo step.
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Accept");
        super::handle_writer_mouse(&mut state, click_at(x, y));
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
        super::handle_writer_mouse(&mut state, click_at(x, y));
        assert_eq!(
            state.writers.get(&id).unwrap().editor.as_ref().unwrap().cursor,
            edtui::Index2::new(0, 6)
        );
        // Drag back to the first char: selects exactly "bbb".
        super::handle_writer_mouse(
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
            super::handle_writer_mouse(
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
        let area = crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, 120, 30));
        let editor = crate::ui::writer::writer_layout(area).editor;
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
        shift_select(&mut state, id, 3);
        assert!(state.writers.get(&id).unwrap().selection.is_some());
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "(✕)");
        super::handle_writer_mouse(&mut state, click_at(x, y));
        assert_eq!(state.writers.get(&id).unwrap().selection, None);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn cursor_offset(state: &AppState, id: crate::session::SessionId) -> usize {
        let session = state.writers.get(&id).unwrap();
        crate::app::writer::adapter::editor_cursor_offset(
            session.editor.as_ref().unwrap(),
        )
    }

    fn shift(code: event::KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, event::KeyModifiers::SHIFT)
    }

    fn ctrl_key(code: event::KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, event::KeyModifiers::CONTROL)
    }

    fn editor_area(state: &AppState) -> ratatui::layout::Rect {
        let (rows, cols) = state.term_size;
        let area = crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, cols, rows));
        crate::ui::writer::writer_layout(area).editor
    }

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
        super::handle_writer_mouse(&mut state, click_at(x, y));
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
        // Paint once so the editor learns its screen area.
        paint_full(&mut state, id);
        let page = editor_area(&state).height as usize;
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
        let editor = editor_area(&state);
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
        let layout = crate::ui::writer::writer_layout(area);
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
        let editor = editor_area(&state);
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
        super::handle_writer_mouse(&mut state, click_at(x, y));
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
        let w = editor_area(&state).width as usize;
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
