    use super::*;
    use super::editor::paint_empty;
    use crate::app::writer::WriterSession;
    use ratatui::layout::Rect;
    use ratatui::{backend::TestBackend, Terminal};

    fn paint_empty_to(
        session: &WriterSession,
        w: u16,
        h: u16,
    ) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                paint_empty(f, f.area(), session);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_text(buf: &ratatui::buffer::Buffer, y: u16, x0: u16, x1: u16) -> String {
        (x0..x1.min(buf.area.width))
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    fn doc_session(text: &str) -> WriterSession {
        use edtui::{EditorMode, EditorState, Lines};
        let mut session = WriterSession::default();
        session.doc = Some(crate::writer::document::Document {
            path_rel: "d.md".to_string(),
            abs_path: std::path::PathBuf::from("/tmp/d.md"),
            text: text.to_string(),
            revision: 0,
            disk_hash: 0,
            dirty: false,
        });
        let mut editor = EditorState::new(Lines::from(text));
        editor.mode = EditorMode::Insert;
        editor.set_clipboard(session.clip.clone());
        session.editor = Some(editor);
        session
    }

    fn paint_doc_to(session: &mut WriterSession, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                paint(f, f.area(), session, crate::session::Activity::Idle, w);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
        buf.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn doc_paints_title_editor_status_and_actions() {
        let mut session = doc_session("aaa bbb");
        // Panel-era UI needs the assistant shown (hidden default).
        session.panel_visible = true;
        let buf = paint_doc_to(&mut session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("d.md"), "title row: {text}");
        assert!(text.contains("aaa bbb"), "editor text");
        assert!(text.contains("rev 0"), "status revision");
        assert!(text.contains("1:1"), "status line:col");
        assert!(text.contains("Idle"), "agent activity word");
        assert!(!text.contains("unsaved"), "clean doc hides it");
        assert!(text.contains("Rephrase"), "action row");
        assert!(text.contains("Save"), "action row");
        assert!(text.contains("no selection"), "chat chip");
        assert!(text.contains("proposals and answers land here"), "panel placeholder");
    }

    #[test]
    fn dirty_doc_marks_title_and_status() {
        let mut session = doc_session("aaa");
        session
            .doc
            .as_mut()
            .unwrap()
            .apply_edit(0..3, "bbb")
            .unwrap();
        let buf = paint_doc_to(&mut session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("d.md ●"), "title dot");
        assert!(text.contains("unsaved"), "status word");
        assert!(text.contains("rev 1"), "revision bumped");
    }

    #[test]
    fn selected_proposal_paints_diff_gutter_and_highlight() {
        use ratatui::style::Color;
        let mut session = doc_session("aaa bbb ccc");
        // Panel-era UI needs the assistant shown (hidden default).
        session.panel_visible = true;
        let pid = session
            .proposals
            .propose(
                session.doc.as_ref().unwrap(),
                None,
                4..7,
                "BBB".to_string(),
                Some("louder".to_string()),
            )
            .unwrap();
        session.selected_proposal = Some(pid);
        let buf = paint_doc_to(&mut session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("- bbb"), "diff row: {text}");
        assert!(text.contains("+ BBB"), "diff row");
        assert!(text.contains("note: louder"), "note row");
        assert!(text.contains("Accept"), "accept pill");
        assert!(text.contains("Reject"), "reject pill");
        assert!(text.contains("> P"), "selected marker");
        // Single-line doc: the gutter mark sits on the editor's first
        // row, and the highlight fills exactly the target cells.
        assert_eq!(buf[(3, 3)].symbol(), "▌", "gutter mark");
        for x in [8, 9, 10] {
            assert_eq!(
                buf[(x, 3)].style().bg,
                Some(Color::Yellow),
                "highlight cell {x}"
            );
        }
        assert_ne!(buf[(4, 3)].style().bg, Some(Color::Yellow), "outside range");
    }

    #[test]
    fn stale_entry_shows_note_without_pills() {
        let mut session = doc_session("aaa bbb");
        // Panel-era UI needs the assistant shown (hidden default).
        session.panel_visible = true;
        let pid = session
            .proposals
            .propose(session.doc.as_ref().unwrap(), None, 0..3, "AAA".to_string(), None)
            .unwrap();
        session.selected_proposal = Some(pid);
        session.doc.as_mut().unwrap().apply_edit(0..3, "zzz").unwrap();
        session.proposals.on_edit(&(0..3));
        let buf = paint_doc_to(&mut session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("stale: text changed"), "stale note: {text}");
        assert!(!text.contains("Accept"), "no accept affordance");
        assert!(!text.contains("Reject"), "no reject affordance");
    }

    #[test]
    fn answer_renders_markdown_in_the_thread() {
        let mut session = doc_session("aaa");
        // Panel-era UI needs the assistant shown (hidden default).
        session.panel_visible = true;
        session.thread.push(crate::app::writer::WriterThreadEntry {
            request_id: 1,
            answer: "# Why\n\nbecause reasons".to_string(),
        });
        let buf = paint_doc_to(&mut session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("Why"), "heading: {text}");
        assert!(text.contains("because reasons"), "body");
        assert!(text.contains("A1:"), "answer header");
    }

    #[test]
    fn narrow_panel_shows_the_widen_notice() {
        let mut session = doc_session("aaa bbb");
        // Panel-era UI needs the assistant shown (hidden default).
        session.panel_visible = true;
        let buf = paint_doc_to(&mut session, 80, 24);
        let layout = writer_layout(Rect::new(0, 0, 80, 24), true);
        assert!(panel_collapsed(80));
        let row = row_text(&buf, layout.panel.y, layout.panel.x, layout.panel.x + 22);
        assert!(row.contains("widen to ≥100 columns"), "notice: {row:?}");
        assert!(buffer_text(&buf).contains("aaa bbb"), "editor keeps painting");
    }

    #[test]
    fn chat_chip_shows_the_live_selection() {
        let mut session = doc_session("aaa bbb");
        // Panel-era UI needs the assistant shown (hidden default).
        session.panel_visible = true;
        session.selection = Some(2..5);
        let buf = paint_doc_to(&mut session, 120, 30);
        assert!(buffer_text(&buf).contains("[2–5] (✕)"), "chip with detach");
    }

    #[test]
    fn layout_keeps_the_65_35_split_and_fixed_slots() {
        let area = Rect::new(0, 0, 120, 30);
        let layout = writer_layout(area, true);
        // Border 1 + side pad 2 on the left.
        assert_eq!((layout.title.x, layout.title.y), (3, 2));
        assert_eq!(layout.title.width, 114);
        // Editor takes 65% of the 114-wide content.
        assert_eq!(layout.editor.width, 114 * 65 / 100 - 1);
        assert_eq!(layout.gutter.width, 1);
        assert_eq!(
            layout.panel.width,
            114 - 114 * 65 / 100,
            "panel takes the rest"
        );
        // Fixed slots stack at the bottom: chat 3, status/action/error 1.
        assert_eq!(layout.chat.height, 3);
        assert_eq!(layout.status.height, 1);
        assert_eq!(layout.action.height, 1);
        assert_eq!(layout.error.height, 1);
        assert_eq!(
            layout.error.y, 27,
            "error owns the last content row (inner 2..28)"
        );
        assert!(panel_collapsed(80));
        assert!(!panel_collapsed(100));
        assert!(!panel_collapsed(120));
    }

    #[test]
    fn empty_state_paints_both_pills_and_title() {
        let session = WriterSession::default();
        let buf = paint_empty_to(&session, 120, 30);
        let text: String = buf
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Writer"), "title row");
        assert!(text.contains("*New document"), "default-marked pill");
        assert!(text.contains("Open…"), "open pill");
        // Rounded border corners, never blank panels.
        assert_eq!(buf[(0, 0)].symbol(), "╭");
        assert_eq!(buf[(119, 0)].symbol(), "╮");
    }

    #[test]
    fn rest_pill_caps_use_the_semantic_rest_cap() {
        // No new inline colours: rest caps come from the shared theme
        // helper, mirroring the topbar/dialog pills.
        let buf = paint_empty_to(&WriterSession::default(), 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30), true);
        let (_, open_rect) = empty_pill_rects(layout.body);
        assert!(open_rect.height > 0, "open pill paints");
        let left_cap = buf[(open_rect.x, open_rect.y)].fg;
        let right_cap =
            buf[(open_rect.x + open_rect.width - 1, open_rect.y)].fg;
        let rest = crate::ui::theme::pill_rest_cap();
        assert_eq!(left_cap, rest, "left cap");
        assert_eq!(right_cap, rest, "right cap");
    }

    #[test]
    fn prompt_paints_with_title_and_buffer() {
        let mut session = WriterSession::default();
        session.open_prompt = Some(crate::app::writer::WriterOpenPrompt {
            buffer: "zz9".to_string(),
            create: false,
        });
        let buf = paint_empty_to(&session, 120, 30);
        let text: String = buf
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Open document — path:"), "prompt title");
        assert!(text.contains("zz9"), "buffer text");
    }

    #[test]
    fn error_slot_holds_the_fixed_row() {
        let mut session = WriterSession::default();
        session.error = Some("no live agent tab".to_string());
        let buf = paint_empty_to(&session, 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30), true);
        let row = row_text(&buf, layout.error.y, layout.error.x, layout.error.x + 20);
        assert!(row.contains("no live agent tab"), "slot row: {row:?}");
        // Same row empty without an error: geometry never moves.
        let clean = paint_empty_to(&WriterSession::default(), 120, 30);
        let same = row_text(&clean, layout.error.y, layout.error.x, layout.error.x + 20);
        assert_eq!(same.trim(), "", "slot row: {same:?}");
    }
