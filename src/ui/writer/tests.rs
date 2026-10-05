    use super::empty::paint_empty;
    use super::*;
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
                paint_empty(f, f.area(), session, w);
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
        assert!(text.contains("Save as"), "toolbar Save-as");
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
        let open_rect = super::start_open_rect(
            layout.body.x + super::EMPTY_INDENT,
            layout.body.y + 7,
        );
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
            kind: crate::app::writer::PromptKind::Open,
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
    fn toolbar_geometry_pinned_at_100_and_200() {
        use super::layout::{toolbar_narrow, toolbar_pill_rects, ToolbarButton};
        for width in [100u16, 200u16] {
            assert!(!toolbar_narrow(width), "full set at {width}");
            let layout = writer_layout(Rect::new(0, 0, width, 30), false);
            let session = WriterSession::default();
            let rects = toolbar_pill_rects(layout.title, false, &session);
            let buttons: Vec<ToolbarButton> = rects.iter().map(|(_, b)| *b).collect();
            assert_eq!(
                buttons,
                vec![
                    ToolbarButton::New,
                    ToolbarButton::Open,
                    ToolbarButton::Save,
                    ToolbarButton::SaveAs,
                    ToolbarButton::Close,
                    ToolbarButton::Preview,
                    ToolbarButton::Assistant,
                ],
                "order at {width}"
            );
            // Left-aligned from the row edge with 1-cell gaps and the
            // `│` separator between Close and Preview.
            let mut x = layout.title.x;
            for (rect, _) in rects.iter().take(5) {
                assert_eq!(rect.x, x, "pill x at {width}");
                assert_eq!(rect.y, layout.title.y);
                x += rect.width + 1;
            }
            // Separator occupies one cell with a gap on each side.
            assert_eq!(rects[5].0.x, x + 2, "preview past the separator at {width}");
        }
    }

    #[test]
    fn toolbar_narrow_collapses_to_more_with_menu_below() {
        use super::layout::{toolbar_narrow, toolbar_pill_rects, ToolbarButton};
        assert!(toolbar_narrow(80), "narrow below 100 cols");
        let layout = writer_layout(Rect::new(0, 0, 80, 30), false);
        let session = WriterSession::default();
        let rects = toolbar_pill_rects(layout.title, true, &session);
        let buttons: Vec<ToolbarButton> = rects.iter().map(|(_, b)| *b).collect();
        assert_eq!(
            buttons,
            vec![
                ToolbarButton::New,
                ToolbarButton::Open,
                ToolbarButton::Save,
                ToolbarButton::Close,
                ToolbarButton::More,
            ],
            "short set plus More"
        );
        let (more_rect, _) = rects.last().copied().unwrap();
        let menu = super::layout::more_menu_rect(more_rect);
        assert_eq!(menu.x, more_rect.x, "menu under the More pill");
        assert_eq!(menu.y, layout.title.y + 1);
        assert_eq!(menu.height, 3, "Save as, Preview, Assistant");
    }

    #[test]
    fn confirm_pills_pin_after_the_message() {
        use super::layout::{confirm_label, confirm_pill_rects, pill_width};
        use crate::app::writer::ConfirmAction;
        let slot = Rect::new(3, 28, 114, 1);
        let actions = vec![ConfirmAction::Overwrite("/x/b.md".into()), ConfirmAction::Cancel];
        let rects = confirm_pill_rects(slot, "b.md exists:", &actions);
        assert_eq!(rects.len(), 2);
        let msg_w = "b.md exists:".chars().count() as u16;
        assert_eq!(rects[0].x, slot.x + msg_w + 2, "two cells past the message");
        assert_eq!(rects[0].width, pill_width(confirm_label(&actions[0]), true));
        assert_eq!(
            rects[1].x,
            rects[0].x + rects[0].width + 2,
            "two-cell gap between pills"
        );
        assert_eq!(rects[1].width, pill_width(confirm_label(&actions[1]), false));
    }

    #[test]
    fn empty_variant_a_body_rows() {
        let session = WriterSession::default();
        let buf = paint_empty_to(&session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("📝"), "header mark");
        assert!(text.contains("Writer"), "header");
        assert!(text.contains("Markdown editor"), "subhead");
        assert!(text.contains("Start"), "start block");
        assert!(text.contains("*New document"), "new pill keeps its default mark");
        assert!(text.contains("Ctrl+N"), "new key twin");
        assert!(text.contains("Open…"), "open pill");
        assert!(text.contains("Ctrl+O"), "open key twin");
        assert!(text.contains("Recent"), "recent block");
        assert!(text.contains("No Markdown files found"), "empty recent slot");
        assert!(text.contains("Tip"), "tip line");
        assert!(text.contains("agent can open a document"), "tip text");
        assert!(text.contains("F6 focus"), "long hint row");
        // Toolbar row 1 in the empty state too, with visible disableds.
        assert!(text.contains("New"), "toolbar new");
        assert!(text.contains("░Save░"), "toolbar save disabled, never color alone");
        assert!(text.contains("░Preview░"), "preview disabled until E8");
    }

    #[test]
    fn toolbar_shows_pressed_assistant_while_open() {
        let mut session = WriterSession::default();
        session.panel_visible = true;
        let buf = paint_empty_to(&session, 120, 30);
        assert!(buffer_text(&buf).contains("*Assistant"), "pressed marker");
        let shut = paint_empty_to(&WriterSession::default(), 120, 30);
        assert!(buffer_text(&shut).contains("Assistant"), "rest state");
        assert!(!buffer_text(&shut).contains("*Assistant"), "no pressed mark");
    }

    #[test]
    fn doc_title_row_shows_toolbar_and_filename() {
        let mut session = doc_session("aaa bbb");
        let buf = paint_doc_to(&mut session, 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        let row = row_text(&buf, layout.title.y, layout.title.x, layout.title.x + layout.title.width);
        assert!(row.contains("New"), "toolbar in the title row: {row:?}");
        assert!(row.contains("Open"), "toolbar open: {row:?}");
        assert!(row.contains("░Preview░"), "preview disabled: {row:?}");
        assert!(row.contains("d.md"), "file name in the title row: {row:?}");
        assert!(row.find("d.md").unwrap() > row.find("New").unwrap(), "name trails the pills");
        session.doc.as_mut().unwrap().dirty = true;
        let buf = paint_doc_to(&mut session, 120, 30);
        let row = row_text(&buf, layout.title.y, layout.title.x, layout.title.x + layout.title.width);
        assert!(row.contains("●"), "dirty dot: {row:?}");
    }

    #[test]
    fn recent_rows_show_selection_marker_and_ages() {
        use std::time::{Duration, SystemTime};
        let now = SystemTime::now();
        let mut session = WriterSession::default();
        session.recent_cwd = Some(std::path::PathBuf::from("/s"));
        session.recent_cache = vec![
            crate::writer::recent::RecentEntry {
                rel: "a.md".to_string(),
                mtime: now - Duration::from_secs(720),
                opened_this_run: true,
            },
            crate::writer::recent::RecentEntry {
                rel: "old/b.md".to_string(),
                mtime: now - Duration::from_secs(3 * 24 * 3600),
                opened_this_run: false,
            },
        ];
        session.recent_sel = 1;
        let buf = paint_empty_to(&session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("a.md"), "first row");
        assert!(text.contains("12 min ago"), "relative age");
        assert!(text.contains("3 days ago"), "older age");
        assert!(text.find("a.md").unwrap() < text.find("old/b.md").unwrap(), "opened first");
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        // Selected row carries the ▸ marker; the other row does not.
        let sel_row = row_text(&buf, layout.body.y + 11, layout.body.x, layout.body.x + 30);
        assert!(sel_row.contains("▸"), "marker on the selected row: {sel_row:?}");
        let first_row = row_text(&buf, layout.body.y + 10, layout.body.x, layout.body.x + 30);
        assert!(!first_row.contains("▸"), "no marker elsewhere: {first_row:?}");
    }

    #[test]
    fn prompt_block_replaces_start_with_suggestions_and_buttons() {
        use std::time::SystemTime;
        let mut session = WriterSession::default();
        session.recent_cwd = Some(std::path::PathBuf::from("/s"));
        session.open_prompt = Some(crate::app::writer::WriterOpenPrompt {
            buffer: "notes/d".to_string(),
            kind: crate::app::writer::PromptKind::New,
        });
        session.recent_cache = vec![
            crate::writer::recent::RecentEntry {
                rel: "notes/demo.md".to_string(),
                mtime: SystemTime::now(),
                opened_this_run: false,
            },
            crate::writer::recent::RecentEntry {
                rel: "notes/design.md".to_string(),
                mtime: SystemTime::now(),
                opened_this_run: false,
            },
            crate::writer::recent::RecentEntry {
                rel: "other.md".to_string(),
                mtime: SystemTime::now(),
                opened_this_run: false,
            },
        ];
        let buf = paint_empty_to(&session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("New document in /s/"), "prompt head with cwd");
        assert!(text.contains("notes/d"), "typed prefix");
        assert!(text.contains("notes/demo.md"), "first suggestion");
        assert!(text.contains("notes/design.md"), "second suggestion");
        assert!(!text.contains("Start"), "start block replaced");
        assert!(text.contains("*Create"), "default create pill");
        assert!(text.contains("Cancel"), "cancel pill");
        assert!(text.contains("Tab completes"), "completion hint");
    }

    #[test]
    fn confirm_paints_message_and_default_pills_in_the_slot() {
        use crate::app::writer::{ConfirmAction, PendingConfirm};
        let mut session = WriterSession::default();
        session.pending_confirm = Some(PendingConfirm {
            message: "b.md exists:".to_string(),
            actions: vec![
                ConfirmAction::Overwrite("/s/b.md".into()),
                ConfirmAction::Cancel,
            ],
        });
        let buf = paint_empty_to(&session, 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        let row = row_text(
            &buf,
            layout.error.y,
            layout.error.x,
            layout.error.x + layout.error.width,
        );
        assert!(row.contains("b.md exists:"), "message: {row:?}");
        assert!(row.contains("*Overwrite"), "default first action: {row:?}");
        assert!(row.contains("Cancel"), "cancel: {row:?}");
    }

    #[test]
    fn tip_sits_in_a_fixed_slot_with_zero_or_eight_recents() {
        use std::time::SystemTime;
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        let tip_y = layout.body.y + layout.body.height - 1;
        // Zero recents: the Tip still sits on the body bottom row.
        let bare = paint_empty_to(&WriterSession::default(), 120, 30);
        let row = row_text(&bare, tip_y, layout.body.x, layout.body.x + 40);
        assert!(row.contains("Tip"), "fixed slot when empty: {row:?}");
        // Eight recents: the same row, not shoved down.
        let mut session = WriterSession::default();
        session.recent_cache = (0..8)
            .map(|i| crate::writer::recent::RecentEntry {
                rel: format!("f{i}.md"),
                mtime: SystemTime::now(),
                opened_this_run: false,
            })
            .collect();
        let full = paint_empty_to(&session, 120, 30);
        let row = row_text(&full, tip_y, layout.body.x, layout.body.x + 40);
        assert!(row.contains("Tip"), "fixed slot when full: {row:?}");
        // And the last recent row sits exactly one row above it.
        let above = row_text(&full, tip_y - 1, layout.body.x, layout.body.x + 40);
        assert!(above.contains("f7.md"), "eight rows fit above: {above:?}");
    }

    #[test]
    fn prompt_head_names_the_session_folder() {
        let mut session = WriterSession::default();
        session.recent_cwd = Some(std::path::PathBuf::from("/s"));
        session.open_prompt = Some(crate::app::writer::WriterOpenPrompt {
            buffer: String::new(),
            kind: crate::app::writer::PromptKind::New,
        });
        let buf = paint_empty_to(&session, 120, 30);
        assert!(
            buffer_text(&buf).contains("New document in /s/"),
            "cwd in the head"
        );
        // Overlong folders middle-ellipsize instead of clipping.
        let long = format!("/{}", "deep/".repeat(30));
        session.recent_cwd = Some(std::path::PathBuf::from(&long));
        let buf = paint_empty_to(&session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("New document in"), "head survives");
        assert!(text.contains("…"), "ellipsis marks the cut");
        assert!(!text.contains(&long), "the middle is what goes");
    }

    #[test]
    fn saveas_prompt_overlays_the_doc_editor() {
        let mut session = doc_session("aaa bbb");
        session.open_prompt = Some(crate::app::writer::WriterOpenPrompt {
            buffer: "c.md".to_string(),
            kind: crate::app::writer::PromptKind::SaveAs,
        });
        let buf = paint_doc_to(&mut session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("Save as — path:"), "prompt head over the editor");
        assert!(text.contains("c.md"), "typed target");
        assert!(text.contains("aaa bbb"), "editor still paints below");
    }

    #[test]
    fn action_row_holds_only_rephrase_beside_the_toolbar_save() {
        let mut session = doc_session("aaa");
        session.doc.as_mut().unwrap().dirty = true;
        session.panel_visible = true;
        let buf = paint_doc_to(&mut session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("Rephrase"), "AI action stays");
        // The toolbar carries the only Save pill now (dirty: enabled).
        // Pin the action row cells: Rephrase paints there, Save does not.
        let layout = writer_layout(Rect::new(0, 0, 120, 30), true);
        let action_row = row_text(&buf, layout.action.y, layout.action.x, layout.action.x + 30);
        assert!(action_row.contains("Rephrase"), "action row: {action_row:?}");
        assert!(!action_row.contains("Save"), "no bottom Save: {action_row:?}");
        let hidden = paint_doc_to(&mut doc_session("aaa"), 120, 30);
        assert!(!buffer_text(&hidden).contains("Rephrase"), "row hides with the panel");
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
