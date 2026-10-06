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
            mtime: None,
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
    fn markdown_styles_land_on_painted_cells() {
        let mut session = doc_session("# Head\n\n```rs\nlet x = 1;\n```\n**bold** and *em*\n");
        let buf = paint_doc_to(&mut session, 120, 30);
        let find_row = |needle: &str| -> u16 {
            (0..30)
                .find(|y| row_text(&buf, *y, 0, 120).contains(needle))
                .expect("painted row")
        };
        let head = find_row("# Head");
        // The '#' cell may carry the cursor; assert past it.
        let hcell = buf[(6, head)].clone();
        assert_eq!(hcell.fg, ratatui::style::Color::Cyan, "heading hue");
        assert!(
            hcell.modifier.contains(ratatui::style::Modifier::BOLD),
            "heading bold"
        );
        let code = find_row("let x = 1;");
        assert_eq!(
            buf[(4, code)].fg,
            ratatui::style::Color::Green,
            "fence content one code style"
        );
        let fence = find_row("```rs");
        assert_eq!(
            buf[(4, fence)].fg,
            ratatui::style::Color::Green,
            "fence delimiter code-styled"
        );
        let text = buffer_text(&buf);
        for marker in ["# Head", "```rs", "```", "**bold**", "*em*"] {
            assert!(text.contains(marker), "marker stays in the buffer: {marker}");
        }
        let bold = find_row("**bold**");
        // Cell scan, not byte find: the box border is multibyte.
        let bcol = (0..120)
            .find(|x| row_text(&buf, bold, *x, 120).starts_with("bold"))
            .expect("bold cell");
        assert!(
            buf[(bcol, bold)].modifier.contains(ratatui::style::Modifier::BOLD),
            "bold span bold"
        );
    }

    #[test]
    fn preview_renders_the_heading_without_its_hash() {
        let mut session = doc_session("# Head\n\nbody\n");
        session.preview = true;
        let buf = paint_doc_to(&mut session, 120, 30);
        let head = (0..30)
            .find(|y| row_text(&buf, *y, 0, 120).contains("Head"))
            .expect("heading previews");
        assert!(
            !row_text(&buf, head, 0, 120).contains('#'),
            "no hash markers in preview"
        );
        // The paint never touches editor state: cursor and scroll
        // survive preview.
        assert_eq!(
            session.editor.as_ref().unwrap().cursor,
            edtui::Index2::new(0, 0)
        );
    }

    #[test]
    fn status_counts_words_chars_selection_and_save() {
        let mut session = doc_session("hello world\nfoo\n");
        session.doc.as_mut().unwrap().dirty = true;
        session.selection = Some(0..5);
        session.save_note = Some("saved".to_string());
        let buf = paint_doc_to(&mut session, 120, 30);
        let status = (0..30)
            .map(|y| row_text(&buf, y, 0, 120))
            .find(|row| row.contains("rev 0"))
            .expect("status row");
        assert!(status.contains("3w 16c"), "words and chars: {status:?}");
        assert!(status.contains("sel 1w 5c"), "selection counts: {status:?}");
        assert!(status.contains("unsaved"), "unsaved word: {status:?}");
        assert!(status.contains("saved"), "save result: {status:?}");
    }

    #[test]
    fn line_numbers_show_and_shrink_the_text_width() {
        let mut session = doc_session("a\nb\n");
        paint_doc_to(&mut session, 120, 30);
        let plain_cols = session.editor_cols;
        session.line_numbers = true;
        let buf = paint_doc_to(&mut session, 120, 30);
        let layout = super::layout::writer_layout(ratatui::layout::Rect::new(0, 0, 120, 30), false);
        // The head holds the rule plus one padding row (A6); the
        // first text row with its number gutter sits below them.
        let text_y = layout.editor.y.saturating_add(super::layout::DOC_PROMPT_OFF);
        assert_eq!(buf[(layout.editor.x, text_y)].symbol(), "1");
        assert_eq!(
            session.editor_cols,
            plain_cols.saturating_sub(2),
            "number gutter leaves the wrap math exact"
        );
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
        // row below the toolbar rule (A6 head), and the highlight
        // fills exactly the target cells.
        assert_eq!(buf[(3, 5)].symbol(), "▌", "gutter mark");
        for x in [8, 9, 10] {
            assert_eq!(
                buf[(x, 5)].style().bg,
                Some(Color::Yellow),
                "highlight cell {x}"
            );
        }
        assert_ne!(buf[(4, 5)].style().bg, Some(Color::Yellow), "outside range");
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
            run: None,
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
        // One divider column sits between editor and panel.
        assert_eq!(
            layout.panel.x,
            layout.editor.x + layout.editor.width + 1,
            "divider column"
        );
        assert_eq!(
            layout.panel.width,
            114 - 114 * 65 / 100 - 1,
            "panel takes the rest minus the divider"
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
    fn bottom_stack_pins_status_with_no_reserved_rows() {
        let hidden = writer_layout(Rect::new(0, 0, 120, 30), false);
        assert_eq!(hidden.error.y, 27, "error slot owns the last row");
        assert_eq!(hidden.status.y, 26, "status pinned to the bottom");
        assert_eq!(hidden.action.height, 0, "no action row when hidden");
        // Looked at the rows under the status: row 27 is the fixed
        // error slot (blank when there is no error, so toggling it
        // never shoves the layout) and row 28 is the deliberate box
        // bottom padding mirroring the top chrome. Neither is
        // reserved space to remove.
        let buf = paint_empty_to(&WriterSession::default(), 120, 30);
        let row28: String = (1..119).map(|x| buf[(x, 28)].symbol()).collect();
        assert!(
            row28.trim().is_empty(),
            "row 28 interior stays blank box padding: {row28:?}"
        );
        assert_eq!(
            hidden.body.y.saturating_add(hidden.body.height),
            hidden.status.y,
            "no reserved rows: the body meets the status"
        );
        let shown = writer_layout(Rect::new(0, 0, 120, 30), true);
        assert_eq!(shown.error.y, 27);
        assert_eq!(shown.status.y, 26, "status pinned in both geometries");
        assert_eq!(shown.action.height, 1);
        assert_eq!(
            shown.body.y.saturating_add(shown.body.height),
            shown.chat.y,
            "no gap row above the chat"
        );
        assert_eq!(shown.chat.y.saturating_add(shown.chat.height), shown.action.y);
        assert_eq!(shown.action.y.saturating_add(1), shown.status.y);
        assert_eq!(shown.status.y.saturating_add(1), shown.error.y);
    }

    #[test]
    fn recent_rows_align_the_age_column() {
        use std::time::SystemTime;
        let mut session = WriterSession::default();
        session.recent_cache = ["a.md", "a-much-longer-file-name.md"]
            .iter()
            .map(|rel| crate::writer::recent::RecentEntry {
                rel: rel.to_string(),
                mtime: SystemTime::now(),
                opened_this_run: false,
            })
            .collect();
        let buf = paint_empty_to(&session, 120, 30);
        let mut cols = Vec::new();
        for y in 0..30 {
            let cells: Vec<String> =
                (0..120).map(|x| buf[(x, y)].symbol().to_string()).collect();
            if cells.concat().contains("edited") {
                // First cell whose remaining row starts "edited".
                cols.push(
                    (0..120)
                        .find(|x| cells[*x as usize..].concat().starts_with("edited"))
                        .unwrap_or(usize::MAX),
                );
            }
        }
        assert_eq!(cols.len(), 2, "both recent rows show ages");
        assert_eq!(cols[0], cols[1], "ages start in one column: {cols:?}");
    }

    #[test]
    fn panel_content_starts_below_the_toolbar_rule() {
        let session = WriterSession {
            panel_visible: true,
            ..WriterSession::default()
        };
        let buf = paint_empty_to(&session, 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30), true);
        assert_eq!(layout.panel.y, layout.body.y.saturating_add(2));
        assert_eq!(
            layout.panel.y.saturating_add(layout.panel.height),
            layout.body.y.saturating_add(layout.body.height),
            "panel still ends at the body bottom"
        );
        for y in layout.body.y..layout.panel.y {
            let row: String = (layout.panel.x..layout.panel.x.saturating_add(layout.panel.width))
                .map(|x| buf[(x, y)].symbol())
                .collect();
            assert!(row.trim().is_empty(), "no panel content on the rule rows: {row:?}");
        }
        let first: String = (layout.panel.x
            ..layout.panel.x.saturating_add(layout.panel.width))
            .map(|x| buf[(x, layout.panel.y)].symbol())
            .collect();
        assert!(
            first.contains("proposals and answers land here"),
            "panel head below the rule: {first:?}"
        );
    }

    #[test]
    fn panel_visible_paints_editor_panel_divider() {
        let mut session = doc_session("aaa");
        session.panel_visible = true;
        let buf = paint_doc_to(&mut session, 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30), true);
        assert_eq!(
            layout.panel.x,
            layout.editor.x.saturating_add(layout.editor.width).saturating_add(1),
            "one divider column between editor and panel"
        );
        for y in layout.body.y..layout.body.y.saturating_add(layout.body.height) {
            assert_eq!(
                buf[(layout.panel.x.saturating_sub(1), y)].symbol(),
                "│",
                "divider row {y}"
            );
        }
    }

    #[test]
    fn assistant_pressed_uses_check_never_default_star() {
        let mut session = doc_session("x");
        session.panel_visible = true;
        let buf = paint_doc_to(&mut session, 120, 30);
        let text = buffer_text(&buf);
        assert!(
            text.contains("✓Assistant"),
            "pressed toggle carries ✓: {text}"
        );
        assert!(
            !text.contains("*Assistant"),
            "`*` stays the default-action marker: {text}"
        );
    }

    #[test]
    fn doc_view_has_toolbar_rule_and_top_padding_like_empty() {
        let mut session = doc_session("aaa bbb");
        session.panel_visible = true;
        let buf = paint_doc_to(&mut session, 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30), true);
        let rule: String = (layout.editor.x
            ..layout.editor.x.saturating_add(layout.editor.width))
            .map(|x| buf[(x, layout.body.y)].symbol())
            .collect();
        assert!(
            rule.chars().all(|c| c == '─'),
            "rule under the toolbar: {rule:?}"
        );
        let pad: String = (layout.editor.x
            ..layout.editor.x.saturating_add(layout.editor.width))
            .map(|x| buf[(x, layout.body.y + 1)].symbol())
            .collect();
        assert!(pad.trim().is_empty(), "one padding row: {pad:?}");
        let first: String = (layout.editor.x
            ..layout.editor.x.saturating_add(12))
            .map(|x| buf[(x, layout.body.y + 2)].symbol())
            .collect();
        assert!(first.contains("aaa"), "editor head below the padding: {first:?}");
    }

    #[test]
    fn editor_theme_uses_semantic_roles_only() {
        use ratatui::style::Color;
        let mut session = doc_session("aaa bbb");
        session.panel_visible = true;
        let buf = paint_doc_to(&mut session, 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30), true);
        let mut rgb_cells = Vec::new();
        for y in layout.editor.y..layout.editor.y.saturating_add(layout.editor.height) {
            for x in layout.editor.x..layout.editor.x.saturating_add(layout.editor.width) {
                let cell = &buf[(x, y)];
                if matches!(cell.fg, Color::Rgb(..)) || matches!(cell.bg, Color::Rgb(..)) {
                    rgb_cells.push((x, y, cell.symbol().to_string()));
                }
            }
        }
        assert!(
            rgb_cells.is_empty(),
            "EdTUI cursor/line-numbers must map to semantic roles, got RGB at {rgb_cells:?}"
        );
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
            cursor: 3,
            select_all: false,
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
                    ToolbarButton::Process,
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
        assert_eq!(menu.height, 4, "Save as, Preview, Assistant, numbers");
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
        // Pressed carries ✓, never the default-action `*` (A7).
        assert!(buffer_text(&buf).contains("✓Assistant"), "pressed marker");
        assert!(!buffer_text(&buf).contains("*Assistant"), "no default star");
        let shut = paint_empty_to(&WriterSession::default(), 120, 30);
        assert!(buffer_text(&shut).contains("Assistant"), "rest state");
        assert!(!buffer_text(&shut).contains("✓Assistant"), "no pressed mark");
    }

    #[test]
    fn doc_title_row_shows_toolbar_and_filename() {
        let mut session = doc_session("aaa bbb");
        let buf = paint_doc_to(&mut session, 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        let row = row_text(&buf, layout.title.y, layout.title.x, layout.title.x + layout.title.width);
        assert!(row.contains("New"), "toolbar in the title row: {row:?}");
        assert!(row.contains("Open"), "toolbar open: {row:?}");
        assert!(row.contains("Preview"), "preview enabled with a doc: {row:?}");
        assert!(!row.contains("░Preview░"), "no disabled marks: {row:?}");
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
            cursor: 7,
            select_all: false,
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
        // All eight rows paint above the Tip, wherever the body ends.
        let mut found = false;
        for y in layout.body.y..tip_y {
            if row_text(&full, y, layout.body.x, layout.body.x + 40).contains("f7.md") {
                found = true;
            }
        }
        assert!(found, "eight rows fit above the Tip");
    }

    #[test]
    fn prompt_head_names_the_session_folder() {
        let mut session = WriterSession::default();
        session.recent_cwd = Some(std::path::PathBuf::from("/s"));
        session.open_prompt = Some(crate::app::writer::WriterOpenPrompt {
            buffer: String::new(),
            kind: crate::app::writer::PromptKind::New,
            cursor: 0,
            select_all: false,
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
            cursor: 4,
            select_all: false,
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

    #[test]
    fn find_bar_paints_in_the_error_slot_without_shifting() {
        let mut session = doc_session("foo bar foo");
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        let editor_rows: Vec<String> = (layout.body.y..layout.status.y)
            .map(|y| row_text(&paint_doc_to(&mut session, 120, 30), y, 0, 120))
            .collect();
        session.find = Some(crate::app::writer::WriterFind {
            query: "foo".to_string(),
            cursor: 3,
            matches: vec![0..3, 8..11],
            current: 0,
            ..Default::default()
        });
        let buf = paint_doc_to(&mut session, 120, 30);
        // Full width: the query field pads to its rect, so the
        // counter and pills sit further right.
        let bar = row_text(&buf, layout.error.y, layout.error.x, layout.error.x + 120);
        assert!(bar.contains("Find"), "bar row: {bar:?}");
        assert!(bar.contains("1/2"), "counter: {bar:?}");
        assert!(bar.contains("aa"), "case pill: {bar:?}");
        // Every editor row above the status paints exactly as without
        // the bar: the fixed slot shifts nothing.
        for (i, y) in (layout.body.y..layout.status.y).enumerate() {
            assert_eq!(
                row_text(&buf, y, 0, 120),
                editor_rows[i],
                "editor row {y} moved"
            );
        }
    }

    #[test]
    fn find_matches_highlight_with_current_reversed() {
        let mut session = doc_session("foo bar foo");
        session.find = Some(crate::app::writer::WriterFind {
            query: "foo".to_string(),
            cursor: 3,
            matches: vec![0..3, 8..11],
            current: 1,
            ..Default::default()
        });
        let buf = paint_doc_to(&mut session, 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        // A rule and a padding row sit under the toolbar (shared
        // DOC_PROMPT_OFF with the paint); the gutter takes one cell,
        // so the text starts one cell into the editor rect.
        let y = layout.body.y.saturating_add(DOC_PROMPT_OFF);
        let x0 = layout.editor.x;
        // Cell right of the editor cursor (which masks the first
        // match cell, like the E4 heading test's `#`).
        assert_eq!(buf[(x0 + 1, y)].symbol(), "o", "match cell");
        let plain = buf[(x0 + 1, y)].clone();
        assert_eq!(
            plain.fg,
            ratatui::style::Color::LightYellow,
            "match hue"
        );
        assert!(
            !plain.modifier.contains(ratatui::style::Modifier::REVERSED),
            "only the current reverses"
        );
        let current = buf[(x0 + 8, y)].clone();
        assert_eq!(current.symbol(), "f", "second match cell");
        assert!(
            current.modifier.contains(ratatui::style::Modifier::REVERSED),
            "current match reverses"
        );
    }

    #[test]
    fn find_replace_toggle_paints_both_states() {
        let mut session = doc_session("foo bar foo");
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        session.find = Some(crate::app::writer::WriterFind {
            query: "foo".to_string(),
            cursor: 3,
            matches: vec![0..3, 8..11],
            current: 0,
            ..Default::default()
        });
        let shut = paint_doc_to(&mut session, 120, 30);
        let row = row_text(&shut, layout.error.y, layout.error.x, layout.error.x + 120);
        assert!(row.contains("Replace ▸"), "closed toggle: {row:?}");
        assert!(row.contains("Alt+H replace"), "hint: {row:?}");
        session.find.as_mut().unwrap().replace_open = true;
        let open = paint_doc_to(&mut session, 120, 30);
        let row = row_text(&open, layout.error.y, layout.error.x, layout.error.x + 120);
        assert!(row.contains("Replace ▾"), "open toggle: {row:?}");
        assert!(row.contains("Replace next"), "action pill: {row:?}");
        assert!(row.contains("Replace all"), "action pill: {row:?}");
    }

    #[test]
    fn fixed_slot_priority_is_confirm_bar_error_banner() {
        use crate::app::writer::{ConfirmAction, PendingConfirm};
        let mut session = doc_session("aaa");
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        let slot = |buf: &ratatui::buffer::Buffer| {
            row_text(&buf, layout.error.y, layout.error.x, layout.error.x + 60)
        };
        session.banner = Some("Reloaded: changed on disk".to_string());
        let row = slot(&paint_doc_to(&mut session, 120, 30));
        assert!(row.contains("Reloaded"), "banner alone: {row:?}");
        session.error = Some("boom".to_string());
        let row = slot(&paint_doc_to(&mut session, 120, 30));
        assert!(row.contains("boom") && !row.contains("Reloaded"), "error wins: {row:?}");
        session.find = Some(crate::app::writer::WriterFind {
            query: "a".to_string(),
            cursor: 1,
            ..Default::default()
        });
        let row = slot(&paint_doc_to(&mut session, 120, 30));
        assert!(row.contains("Find") && !row.contains("boom"), "bar wins: {row:?}");
        session.pending_confirm = Some(PendingConfirm {
            message: "Changed on disk: d.md".to_string(),
            actions: vec![ConfirmAction::ReloadFromDisk, ConfirmAction::KeepMine],
        });
        let row = slot(&paint_doc_to(&mut session, 120, 30));
        assert!(row.contains("Changed on disk"), "confirm wins: {row:?}");
    }

    /// Cell debut of every marker part: muted `@@` runs, the verb in
    /// the accent role, the prompt italic, the target on the marker
    /// background. Columns come off the painted row, never by hand.
    #[test]
    fn marker_wrap_parts_paint_with_roles() {
        let mut session = doc_session("@@fix typo@@this is teh@@\n");
        let buf = paint_doc_to(&mut session, 120, 30);
        let row = (0..30)
            .find(|y| row_text(&buf, *y, 0, 120).contains("this is teh"))
            .expect("marker row");
        let col_of = |needle: &str| {
            (0..120)
                .find(|x| row_text(&buf, row, *x, 120).starts_with(needle))
                .expect("part cell")
        };
        let opener = col_of("@@fix");
        let typo = col_of("typo");
        assert_eq!(typo, opener + 6, "one marker, contiguous cells");
        // The first opener cell carries the cursor; assert past it.
        assert_eq!(
            buf[(opener + 1, row)].fg,
            ratatui::style::Color::DarkGray,
            "opener muted"
        );
        assert_eq!(
            buf[(opener + 2, row)].fg,
            ratatui::style::Color::Yellow,
            "verb accent"
        );
        let prompt = buf[(typo, row)].clone();
        assert_eq!(prompt.fg, ratatui::style::Color::White, "prompt text");
        assert!(
            prompt.modifier.contains(ratatui::style::Modifier::ITALIC),
            "prompt italic"
        );
        let target = col_of("this is teh");
        assert_eq!(
            buf[(target, row)].bg,
            ratatui::style::Color::DarkGray,
            "target background"
        );
        let close = col_of("teh") + 3;
        assert_eq!(
            buf[(close, row)].fg,
            ratatui::style::Color::DarkGray,
            "target close muted"
        );
    }

    #[test]
    fn question_verbs_paint_in_the_distinct_role() {
        let mut session = doc_session("@@ask capital@@Paris@@\n");
        let buf = paint_doc_to(&mut session, 120, 30);
        let row = (0..30)
            .find(|y| row_text(&buf, *y, 0, 120).contains("capital"))
            .expect("marker row");
        let verb = (0..120)
            .find(|x| row_text(&buf, row, *x, 120).starts_with("ask"))
            .expect("verb cell");
        assert_eq!(
            buf[(verb, row)].fg,
            ratatui::style::Color::Cyan,
            "ask reads as a question"
        );
        let mut session = doc_session("@@rewrite?@@x@@\n");
        let buf = paint_doc_to(&mut session, 120, 30);
        let row = (0..30)
            .find(|y| row_text(&buf, *y, 0, 120).contains("rewrite"))
            .expect("marker row");
        let verb = (0..120)
            .find(|x| row_text(&buf, row, *x, 120).starts_with("rewrite"))
            .expect("verb cell");
        assert_eq!(
            buf[(verb, row)].fg,
            ratatui::style::Color::Cyan,
            "? verb reads as a question"
        );
    }

    #[test]
    fn standalone_marker_paints_without_a_target() {
        let mut session = doc_session("@@note hi @@end\n");
        let buf = paint_doc_to(&mut session, 120, 30);
        assert!(
            buffer_text(&buf).contains("@@note hi @@end"),
            "marker stays literal"
        );
        let row = (0..30)
            .find(|y| row_text(&buf, *y, 0, 120).contains("note hi"))
            .expect("marker row");
        let verb = (0..120)
            .find(|x| row_text(&buf, row, *x, 120).starts_with("note"))
            .expect("verb cell");
        assert_eq!(
            buf[(verb, row)].fg,
            ratatui::style::Color::Yellow,
            "verb accent"
        );
        let end = (0..120)
            .find(|x| row_text(&buf, row, *x, 120).starts_with("end"))
            .expect("end cell");
        assert_eq!(
            buf[(end, row)].fg,
            ratatui::style::Color::DarkGray,
            "closer muted"
        );
    }

    #[test]
    fn error_span_paints_danger_with_a_gutter_x() {
        let mut session = doc_session("@@fix@@@@target@@\n");
        let buf = paint_doc_to(&mut session, 120, 30);
        let row = (0..30)
            .find(|y| row_text(&buf, *y, 0, 120).contains("target"))
            .expect("error row");
        let fix = (0..120)
            .find(|x| row_text(&buf, row, *x, 120).starts_with("fix"))
            .expect("error cell");
        assert_eq!(
            buf[(fix, row)].fg,
            ratatui::style::Color::Red,
            "error span in the error role"
        );
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        let text_y = layout.editor.y.saturating_add(super::layout::DOC_PROMPT_OFF);
        assert_eq!(
            buf[(layout.gutter.x, text_y)].symbol(),
            "✕",
            "gutter marks the error row"
        );
    }

    #[test]
    fn cursor_on_error_shows_the_reason_in_the_error_slot() {
        let mut session = doc_session("@@fix@@@@target@@\n");
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        let slot = |buf: &ratatui::buffer::Buffer| {
            row_text(&buf, layout.error.y, layout.error.x, layout.error.x + 60)
        };
        // The cursor opens at (0, 0): inside the nesting span.
        let row = slot(&paint_doc_to(&mut session, 120, 30));
        assert!(row.contains("inside a marker"), "reason on cursor: {row:?}");
        // Plain text: the slot stays empty.
        session.editor.as_mut().unwrap().cursor = edtui::Index2::new(0, 12);
        let row = slot(&paint_doc_to(&mut session, 120, 30));
        assert!(row.trim().is_empty(), "no reason off the error: {row:?}");
        // A real error still outranks the marker reason.
        session.editor.as_mut().unwrap().cursor = edtui::Index2::new(0, 0);
        session.error = Some("boom".to_string());
        let row = slot(&paint_doc_to(&mut session, 120, 30));
        assert!(row.contains("boom") && !row.contains("doubled"), "error wins: {row:?}");
    }

    /// PRD §7 answers: an over-long prompt paints exactly like a
    /// parse error (error style, gutter ✕, actionable reason), and
    /// the run exclusion keeps it out of the markup.
    #[test]
    fn overlong_prompt_paints_as_an_error() {
        let long = "p".repeat(2001);
        let text = format!("@@{long}@@target@@\n");
        let mut session = doc_session(&text);
        // Park the cursor on the prompt span so its reason shows.
        session.editor.as_mut().unwrap().cursor = edtui::Index2::new(0, 5);
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        let buf = paint_doc_to(&mut session, 120, 30);
        assert!(
            (0..30).any(|y| buf[(layout.gutter.x, y)].symbol() == "✕"),
            "gutter marks the prompt rows"
        );
        let slot = row_text(&buf, layout.error.y, layout.error.x, layout.error.x + 60);
        assert!(slot.contains("prompt too long"), "reason: {slot:?}");
        let status = (0..30)
            .map(|y| row_text(&buf, y, 0, 120))
            .find(|row| row.contains("rev 0"))
            .expect("status row");
        assert!(status.contains("1 error"), "count: {status:?}");
        assert!(!status.contains("marker"), "no marker: {status:?}");
    }

    #[test]
    fn status_counts_markers_and_errors() {
        let mut session = doc_session("@@fix a@@b@@ and @@c@@d@@\ntext @@ more\n");
        let buf = paint_doc_to(&mut session, 120, 30);
        let status = (0..30)
            .map(|y| row_text(&buf, y, 0, 120))
            .find(|row| row.contains("rev 0"))
            .expect("status row");
        assert!(status.contains("2 markers"), "marker count: {status:?}");
        assert!(status.contains("1 error"), "error count: {status:?}");
        let mut session = doc_session("@@fix a@@b@@\n");
        let buf = paint_doc_to(&mut session, 120, 30);
        let status = (0..30)
            .map(|y| row_text(&buf, y, 0, 120))
            .find(|row| row.contains("rev 0"))
            .expect("status row");
        assert!(status.contains("1 marker"), "singular: {status:?}");
        assert!(!status.contains("markers"), "no plural for one: {status:?}");
        let mut session = doc_session("plain\n");
        let buf = paint_doc_to(&mut session, 120, 30);
        let status = (0..30)
            .map(|y| row_text(&buf, y, 0, 120))
            .find(|row| row.contains("rev 0"))
            .expect("status row");
        assert!(!status.contains("marker"), "no markers, no count: {status:?}");
        assert!(!status.contains("error"), "no errors, no count: {status:?}");
    }

    fn run_session() -> WriterSession {
        use crate::app::writer::runs::{RunMarkerStatus, WriterRun, WriterRunState};
        use crate::app::writer::runs::RunMarker;
        use crate::writer::process::ProcessShape;
        let mut session =
            doc_session("alpha @@fix typo@@this is teh@@\n\n@@ask capital@@Paris@@\n");
        session.runs.push(WriterRun {
            id: 1,
            file: "d.md".to_string(),
            rev: 0,
            started_at: std::time::Instant::now(),
            markers: vec![
                RunMarker {
                    marker: crate::writer::process::ProcessMarker {
                        index: 0,
                        line: 1,
                        whole: 6..31,
                        shape: ProcessShape::Wrap,
                        verb: Some("fix".to_string()),
                        prompt: "typo".to_string(),
                        target: Some("this is teh".to_string()),
                    },
                    status: RunMarkerStatus::Started,
                    note: None,
                },
                RunMarker {
                    marker: crate::writer::process::ProcessMarker {
                        index: 1,
                        line: 3,
                        whole: 33..55,
                        shape: ProcessShape::Wrap,
                        verb: Some("ask".to_string()),
                        prompt: "capital".to_string(),
                        target: Some("Paris".to_string()),
                    },
                    status: RunMarkerStatus::Pending,
                    note: None,
                },
            ],
            state: WriterRunState::Active,
            saw_activity: true,
            reported: true,
        });
        session.process = Some(crate::app::writer::process::ProcessLock {
            run_id: 1,
            pre_text: "alpha @@fix typo@@this is teh@@\n\n@@ask capital@@Paris@@\n".to_string(),
            outside: vec!["alpha ".to_string(), "\n\n".to_string(), "\n".to_string()],
            spans: vec![6..31, 33..55],
            stopped: false,
        });
        session
    }

    /// M5-fix: after the first Stop the pill offers Force stop
    /// (same pill, same funnel; the rect comes from the same label
    /// the paint uses).
    #[test]
    fn process_pill_reads_stop_then_force_stop() {
        let mut session = run_session();
        assert_eq!(
            super::layout::toolbar_action_label(&session, ToolbarButton::Process),
            "Stop"
        );
        session.process.as_mut().unwrap().stopped = true;
        assert_eq!(
            super::layout::toolbar_action_label(&session, ToolbarButton::Process),
            "Force stop"
        );
        session.process = None;
        assert_eq!(
            super::layout::toolbar_action_label(&session, ToolbarButton::Process),
            "Process"
        );
    }

    #[test]
    fn started_marker_paints_the_spin_gutter() {
        let mut session = run_session();
        let buf = paint_doc_to(&mut session, 120, 30);
        let row = (0..30)
            .find(|y| row_text(&buf, *y, 0, 120).contains("teh"))
            .expect("marker row");
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        assert_eq!(
            buf[(layout.gutter.x, row)].symbol(),
            "⟳",
            "gutter spins on the started marker"
        );
    }

    #[test]
    fn run_status_shows_counts_and_details() {
        let mut session = run_session();
        session.process = None;
        session.last_run = Some(crate::app::writer::process::LastRun {
            id: 3,
            done: 3,
            skipped: 1,
            failed: 0,
        });
        let buf = paint_doc_to(&mut session, 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        let status = row_text(&buf, layout.status.y, 0, 120);
        assert!(
            status.contains("Run: 3 done · 1 skipped (Details)"),
            "status: {status:?}"
        );
    }

    #[test]
    fn violation_banner_paints_revert_and_keep_pills() {
        let mut session = run_session();
        session.pending_confirm = Some(crate::app::writer::PendingConfirm {
            message: "Agent changed text outside markers".to_string(),
            actions: vec![
                crate::app::writer::ConfirmAction::RevertRun("pre".to_string()),
                crate::app::writer::ConfirmAction::Keep,
            ],
        });
        let buf = paint_doc_to(&mut session, 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30), false);
        let slot = row_text(&buf, layout.error.y, 0, 120);
        assert!(slot.contains("Agent changed text outside markers"), "slot: {slot:?}");
        assert!(slot.contains("Revert run"), "slot: {slot:?}");
        assert!(slot.contains("Keep"), "slot: {slot:?}");
    }
