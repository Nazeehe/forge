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

    #[test]
    fn cycle_focus_skips_panel_areas_while_hidden() {
        use crate::app::writer::WriterFocus;
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        assert!(!state.writers.get(&id).unwrap().panel_visible, "hidden default");
        for _ in 0..3 {
            state.writer_cycle_focus(id);
            assert_eq!(
                state.writers.get(&id).unwrap().focus,
                WriterFocus::Editor,
                "F6 stays in the editor while the panel is hidden"
            );
        }
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn modal_paints_above_writer_overlay() {
        use crate::ui::dialogs::quit::{Confirm, ConfirmKind};
        use ratatui::{backend::TestBackend, Terminal};
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // Fill the editor past the modal rows through real typing, so a
        // Writer paint that runs after the modal visibly overwrites it.
        for _ in 0..20 {
            for _ in 0..70 {
                state.writer_feed_key(id, key(crossterm::event::KeyCode::Char('x')));
            }
            state.writer_feed_key(id, key(crossterm::event::KeyCode::Enter));
        }
        state.confirm = Some(Confirm::new(ConfirmKind::QuitForge, true));
        let (rows, cols) = state.term_size;
        let views = state.views();
        let chrome = crate::ui::Chrome {
            tabs: Vec::new(),
            topbar: crate::ui::topbar::TopBar { tabs: Vec::new() },
            detail: None,
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
            board_open: false,
            board: None,
            tetris_open: false,
            tetris: None,
            grid: false,
            pills: true,
        };
        let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        terminal
            .draw(|f| {
                crate::tui::paint_frame(f, f.area(), &mut state, &views, &chrome);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let screen: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        assert!(
            screen.contains("Are you sure you want to quit?"),
            "quit modal stays visible above the Writer overlay"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn narrow_resize_keeps_writer_overlay() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        assert_eq!(state.writer_overlay_active(), Some(id));
        state.apply(crate::infra::event::AppEvent::Resize(30, 80));
        assert_eq!(
            state.writer_overlay_active(),
            Some(id),
            "narrow keeps Writer: the panel degrades to a notice instead"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn narrow_resize_still_evicts_other_overlays() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let visual = state.writer_slot(id).unwrap() - 2;
        assert!(state.select_top_tab(visual));
        state.apply(crate::infra::event::AppEvent::Resize(30, 80));
        assert_eq!(state.writer_overlay_active(), None);
        assert!(state.overlay_view.is_none(), "non-Writer overlay evicted");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn narrow_topbar_keeps_writer_tab_reachable() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 80);
        let tabs = state.topbar().tabs;
        // The shown position maps back to the real Writer slot (R1:
        // Visual/Walkthrough drop when narrow, so shown != slot).
        let shown = tabs
            .iter()
            .position(|t| t.label.contains("Writer"))
            .expect("writer tab reachable below 100 cols");
        assert!(state.select_top_tab(shown), "shown index selects Writer");
        assert_eq!(state.writer_overlay_active(), Some(id));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn narrow_strip_paints_writer_tab_clickable() {
        use crate::tui::mouse::forward_mouse;
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::{backend::TestBackend, Terminal};
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 90);
        state.pill_tabs = true;
        let views = state.views();
        let chrome = crate::ui::Chrome {
            tabs: Vec::new(),
            topbar: state.topbar(),
            detail: None,
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
            board_open: false,
            board: None,
            tetris_open: false,
            tetris: None,
            grid: false,
            pills: true,
        };
        let mut terminal = Terminal::new(TestBackend::new(90, 30)).unwrap();
        terminal
            .draw(|f| {
                crate::tui::paint_frame(f, f.area(), &mut state, &views, &chrome);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        // The Writer label is really painted (not just listed), then
        // clicked through the real dispatch. Cell-based: the emoji
        // icons make byte offsets lie.
        let mut hit = None;
        for y in 0..30 {
            let cells: Vec<String> =
                (0..90).map(|x| buf[(x, y)].symbol().to_string()).collect();
            if let Some(x) = (0..90)
                .find(|x| cells[*x as usize..].concat().starts_with("Writer"))
            {
                hit = Some((x, y));
                break;
            }
        }
        let (x, y) = hit.expect("writer tab painted at 90 cols");
        forward_mouse(
            &mut state,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );
        assert_eq!(
            state.writer_overlay_active(),
            Some(id),
            "painted narrow tab opens Writer"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cycle_focus_visits_chat_and_thread_while_shown() {
        use crate::app::writer::WriterFocus;
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        state.writer_toggle_assistant(id);
        state.writer_cycle_focus(id);
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Chat);
        state.writer_cycle_focus(id);
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Thread);
        state.writer_cycle_focus(id);
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Editor);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

