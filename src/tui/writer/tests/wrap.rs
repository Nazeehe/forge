    //! `@@` auto-wrap (§5.1) through the real key path: every row
    //! of the spec table plus Alt+M, undo granularity, and Esc.

    use super::super::test_support::*;
    use crate::tui::input::InputRouter;
    use crate::tui::keys::handle_key_at;
    use crossterm::event;

    type State = crate::app::AppState;
    type Id = crate::session::SessionId;

    fn at() -> event::KeyEvent {
        key(event::KeyCode::Char('@'))
    }

    fn chr(c: char) -> event::KeyEvent {
        key(event::KeyCode::Char(c))
    }

    fn alt_m() -> event::KeyEvent {
        event::KeyEvent::new(event::KeyCode::Char('m'), event::KeyModifiers::ALT)
    }

    fn esc() -> event::KeyEvent {
        key(event::KeyCode::Esc)
    }

    fn enter() -> event::KeyEvent {
        key(event::KeyCode::Enter)
    }

    fn ctrl_z() -> event::KeyEvent {
        event::KeyEvent::new(
            event::KeyCode::Char('z'),
            event::KeyModifiers::CONTROL,
        )
    }

    /// Cursor to offset 4, then Shift+Right ×3: selects "bbb" in the
    /// seeded "aaa bbb", all through the real key path.
    fn select_bbb(
        state: &mut State,
        router: &mut InputRouter,
        now: std::time::Instant,
        id: Id,
    ) {
        for _ in 0..4 {
            handle_key_at(&mut *state, &mut *router, key(event::KeyCode::Right), now);
        }
        for _ in 0..3 {
            handle_key_at(
                &mut *state,
                &mut *router,
                event::KeyEvent::new(
                    event::KeyCode::Right,
                    event::KeyModifiers::SHIFT,
                ),
                now,
            );
        }
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            Some(4..7),
            "bbb selected"
        );
    }

    fn setup() -> (State, Id, std::path::PathBuf, InputRouter, std::time::Instant) {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let router = InputRouter::new();
        let now = std::time::Instant::now();
        (state, id, dir, router, now)
    }

    fn teardown(state: &mut State, id: Id, dir: &std::path::PathBuf) {
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn at_over_selection_arms_without_replacing() {
        let (mut state, id, dir, mut router, now) = setup();
        select_bbb(&mut state, &mut router, now, id);
        handle_key_at(&mut state, &mut router, at(), now);
        let session = state.writers.get(&id).unwrap();
        assert_eq!(doc_text(&state, id), "aaa bbb", "selection kept");
        assert_eq!(
            session.selection,
            Some(4..7),
            "selection still live"
        );
        assert_eq!(
            session.banner.as_deref(),
            Some("type @ again to wrap"),
            "hint shows"
        );
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn second_at_wraps_with_placeholder_selected() {
        let (mut state, id, dir, mut router, now) = setup();
        select_bbb(&mut state, &mut router, now, id);
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(&mut state, &mut router, at(), now);
        assert_eq!(
            doc_text(&state, id),
            "aaa @@verb prompt@@bbb@@",
            "wrap shape"
        );
        let text = doc_text(&state, id);
        let start = text.find("verb prompt").expect("placeholder");
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            Some(start..start + "verb prompt".len()),
            "placeholder selected"
        );
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn typing_replaces_the_placeholder() {
        let (mut state, id, dir, mut router, now) = setup();
        select_bbb(&mut state, &mut router, now, id);
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(&mut state, &mut router, at(), now);
        for c in "fix it".chars() {
            handle_key_at(&mut state, &mut router, chr(c), now);
        }
        assert_eq!(doc_text(&state, id), "aaa @@fix it@@bbb@@");
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn enter_finishes_after_the_closing_mark() {
        let (mut state, id, dir, mut router, now) = setup();
        select_bbb(&mut state, &mut router, now, id);
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(&mut state, &mut router, at(), now);
        for c in "fix it".chars() {
            handle_key_at(&mut state, &mut router, chr(c), now);
        }
        handle_key_at(&mut state, &mut router, enter(), now);
        let text = doc_text(&state, id);
        assert_eq!(text, "aaa @@fix it@@bbb@@", "no newline inserted");
        assert_eq!(
            cursor_offset(&state, id),
            text.chars().count(),
            "cursor after the closing @@"
        );
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn tab_finishes_after_the_closing_mark() {
        let (mut state, id, dir, mut router, now) = setup();
        select_bbb(&mut state, &mut router, now, id);
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(&mut state, &mut router, tab(), now);
        let text = doc_text(&state, id);
        assert_eq!(text, "aaa @@verb prompt@@bbb@@", "no indent inserted");
        assert_eq!(
            cursor_offset(&state, id),
            text.chars().count(),
            "cursor after the closing @@"
        );
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn esc_while_editing_restores_with_one_undo_step() {
        let (mut state, id, dir, mut router, now) = setup();
        select_bbb(&mut state, &mut router, now, id);
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(&mut state, &mut router, chr('X'), now);
        assert_eq!(doc_text(&state, id), "aaa @@X@@bbb@@");
        handle_key_at(&mut state, &mut router, esc(), now);
        assert_eq!(doc_text(&state, id), "aaa bbb", "original text back");
        handle_key_at(&mut state, &mut router, ctrl_z(), now);
        assert_eq!(
            doc_text(&state, id),
            "aaa @@X@@bbb@@",
            "one undo reverts the restore"
        );
        handle_key_at(&mut state, &mut router, ctrl_z(), now);
        assert_eq!(
            doc_text(&state, id),
            "aaa @@verb prompt@@bbb@@",
            "typing undoes next"
        );
        handle_key_at(&mut state, &mut router, ctrl_z(), now);
        assert_eq!(doc_text(&state, id), "aaa bbb", "wrap undoes last");
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn other_key_after_first_at_types_over() {
        let (mut state, id, dir, mut router, now) = setup();
        select_bbb(&mut state, &mut router, now, id);
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(&mut state, &mut router, chr('x'), now);
        assert_eq!(doc_text(&state, id), "aaa @x", "@ then the key");
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn arrow_after_first_at_commits_it_then_moves() {
        let (mut state, id, dir, mut router, now) = setup();
        select_bbb(&mut state, &mut router, now, id);
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(
            &mut state,
            &mut router,
            key(event::KeyCode::Left),
            now,
        );
        // Type-over commits the pending `@` (the selection goes),
        // then the move lands.
        assert_eq!(doc_text(&state, id), "aaa @", "pending @ commits");
        assert_eq!(cursor_offset(&state, id), 4, "then the move lands");
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn alt_m_wraps_without_the_gesture() {
        let (mut state, id, dir, mut router, now) = setup();
        select_bbb(&mut state, &mut router, now, id);
        handle_key_at(&mut state, &mut router, alt_m(), now);
        assert_eq!(
            doc_text(&state, id),
            "aaa @@verb prompt@@bbb@@",
            "same wrap as @@"
        );
        let text = doc_text(&state, id);
        let start = text.find("verb prompt").expect("placeholder");
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            Some(start..start + "verb prompt".len()),
            "placeholder selected"
        );
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn trailing_space_stays_outside_the_markers() {
        // Shift+Ctrl+Right grabs "bbb " (word plus trailing space):
        // the closer must not glue to the next word.
        let (mut state, id, dir, mut router, now) = setup();
        std::fs::write(dir.join("e.md"), "aaa bbb ccc").unwrap();
        open_doc(&mut state, id, "e.md");
        for _ in 0..4 {
            handle_key_at(&mut state, &mut router, key(event::KeyCode::Right), now);
        }
        for _ in 0..4 {
            handle_key_at(
                &mut state,
                &mut router,
                event::KeyEvent::new(
                    event::KeyCode::Right,
                    event::KeyModifiers::SHIFT,
                ),
                now,
            );
        }
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            Some(4..8),
            "bbb plus trailing space"
        );
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(&mut state, &mut router, at(), now);
        assert_eq!(
            doc_text(&state, id),
            "aaa @@verb prompt@@bbb@@ ccc",
            "space outside the closer"
        );
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn leading_space_stays_outside_the_markers() {
        let (mut state, id, dir, mut router, now) = setup();
        for _ in 0..3 {
            handle_key_at(&mut state, &mut router, key(event::KeyCode::Right), now);
        }
        for _ in 0..4 {
            handle_key_at(
                &mut state,
                &mut router,
                event::KeyEvent::new(
                    event::KeyCode::Right,
                    event::KeyModifiers::SHIFT,
                ),
                now,
            );
        }
        assert_eq!(
            state.writers.get(&id).unwrap().selection,
            Some(3..7),
            "space plus bbb"
        );
        handle_key_at(&mut state, &mut router, alt_m(), now);
        assert_eq!(
            doc_text(&state, id),
            "aaa @@verb prompt@@bbb@@",
            "space outside the opener"
        );
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn alt_m_without_selection_is_a_no_op() {
        let (mut state, id, dir, mut router, now) = setup();
        handle_key_at(&mut state, &mut router, alt_m(), now);
        assert_eq!(doc_text(&state, id), "aaa bbb", "nothing to wrap");
        assert!(state.writers.get(&id).unwrap().wrap.is_none());
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn wrap_is_one_undo_step() {
        let (mut state, id, dir, mut router, now) = setup();
        select_bbb(&mut state, &mut router, now, id);
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(&mut state, &mut router, ctrl_z(), now);
        assert_eq!(doc_text(&state, id), "aaa bbb", "single undo reverts");
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn esc_while_armed_disarms() {
        let (mut state, id, dir, mut router, now) = setup();
        select_bbb(&mut state, &mut router, now, id);
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(&mut state, &mut router, esc(), now);
        let session = state.writers.get(&id).unwrap();
        assert_eq!(doc_text(&state, id), "aaa bbb", "nothing changed");
        assert!(session.wrap.is_none(), "disarmed");
        assert_eq!(session.banner, None, "hint cleared");
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn at_without_selection_is_plain_text() {
        let (mut state, id, dir, mut router, now) = setup();
        handle_key_at(&mut state, &mut router, at(), now);
        assert_eq!(doc_text(&state, id), "@aaa bbb");
        teardown(&mut state, id, &dir);
    }

    #[test]
    fn click_abandons_the_gesture_keeping_text() {
        let (mut state, id, dir, mut router, now) = setup();
        state.term_size = (30, 120);
        select_bbb(&mut state, &mut router, now, id);
        handle_key_at(&mut state, &mut router, at(), now);
        handle_key_at(&mut state, &mut router, at(), now);
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "aaa");
        super::super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert!(session.wrap.is_none(), "click ends the gesture");
        assert_eq!(
            doc_text(&state, id),
            "aaa @@verb prompt@@bbb@@",
            "wrapped text stays"
        );
        teardown(&mut state, id, &dir);
    }
