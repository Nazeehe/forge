    use super::super::test_support::*;

    #[test]
    fn assistant_hidden_by_default_with_full_width_editor() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        assert!(!state.writers.get(&id).unwrap().panel_visible, "hidden default");
        let buf = paint_full(&mut state, id);
        let area = crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, 120, 30));
        let hidden = crate::ui::writer::writer_layout(area, false);
        let shown = crate::ui::writer::writer_layout(area, true);
        assert_eq!(hidden.panel.width, 0, "no panel rect");
        assert!(
            hidden.editor.width > shown.editor.width,
            "editor takes the panel width"
        );
        assert!(
            !panel_text(&buf, shown).contains("thread"),
            "no panel content painted: {buf:?}"
        );
        let row: String = (0..120).map(|x| buf[(x, hidden.action.y)].symbol()).collect();
        assert!(!row.contains("Rephrase"), "no action row: {row:?}");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn assistant_toggle_shows_panel_and_action_row() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        state.writer_toggle_assistant(id);
        assert!(state.writers.get(&id).unwrap().panel_visible);
        let buf = paint_full(&mut state, id);
        let area = crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, 120, 30));
        let shown = crate::ui::writer::writer_layout(area, true);
        assert!(shown.panel.width > 0);
        let row: String = (0..120).map(|x| buf[(x, shown.action.y)].symbol()).collect();
        assert!(row.contains("Rephrase"), "action row back: {row:?}");
        state.writer_toggle_assistant(id);
        assert!(!state.writers.get(&id).unwrap().panel_visible);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hiding_assistant_returns_focus_to_editor() {
        use crate::app::writer::WriterFocus;
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        state.writer_toggle_assistant(id);
        state.writers.get_mut(&id).unwrap().focus = WriterFocus::Chat;
        state.writer_toggle_assistant(id);
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Editor);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

