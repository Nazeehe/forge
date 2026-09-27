use super::*;
use crate::app::test_support::*;

#[cfg(feature = "visual")]
#[test]
fn visual_empty_state_describes_tab_and_invocation() {
    let mut state = AppState::new();
    state.term_size = (40, 180);
    let id = state.manager.spawn_agent(
        "agent", &std::env::temp_dir(), "exec cat",
        crate::infra::ids::RunId::generate(), "codex",
    ).unwrap();
    let view = state.visual_view(id, false);
    let text: String = view.lines.iter().flatten().map(|span| span.text.as_str()).collect::<Vec<_>>().join("\n");
    assert!(text.contains("renders diagrams"), "describes the tab: {text:?}");
    assert!(text.contains("visualize the flow for"), "shows how to invoke it: {text:?}");
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_current_paint_tracks_what_the_terminal_shows() {
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    complete(&mut state, id, 1, fake_png(64, 100, 100));
    show_visual_overlay(&mut state, id);
    assert_eq!(state.visual_current_paint(), None, "nothing shown yet");
    let paint = paint_for(&state, id);
    state.visual_take_show(true, paint).expect("show");
    assert_eq!(state.visual_current_paint(), Some(paint));
    // Viewport moved without a reshow: the terminal is stale, so
    // the TUI must build bytes again.
    assert!(state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
    let zoomed = paint_for(&state, id);
    assert_ne!(state.visual_current_paint(), Some(zoomed), "stale paint");
    state.visual_take_show(true, zoomed).expect("reshow");
    assert_eq!(state.visual_current_paint(), Some(zoomed));
    // Leaving the tab clears it: nothing current anymore.
    state.overlay_view = None;
    assert_eq!(state.visual_current_paint(), None);
}

#[cfg(feature = "visual")]
#[test]
fn visual_zoom_and_scroll_clamp_to_the_overflow() {
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    complete(&mut state, id, 1, fake_png(64, 1000, 1000));
    // Zoomed out at minimum: further out changes nothing.
    for _ in 0..20 {
        state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomOut, 200, 50, 8.0, 16.0);
    }
    let slot = state.visual_slots.get(&id).unwrap();
    assert_eq!(slot.zoom, crate::visual::MIN_ZOOM);
    state.dirty = false;
    assert!(!state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomOut, 200, 50, 8.0, 16.0));
    assert!(!state.dirty, "no-op zoom stays clean");
    // Zoom to maximum: 1000x1000 at 8x overflows a 200x50 tab by
    // (600, 350) cells.
    for _ in 0..30 {
        state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomIn, 200, 50, 8.0, 16.0);
    }
    assert_eq!(state.visual_slots.get(&id).unwrap().zoom, crate::visual::MAX_ZOOM);
    assert!(!state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
    state.dirty = false;
    assert!(state.visual_scroll(id, 5, 7, 200, 50, 8.0, 16.0));
    assert!(state.dirty, "scroll marks dirty");
    let slot = state.visual_slots.get(&id).unwrap();
    assert_eq!((slot.scroll_x, slot.scroll_y), (5, 7));
    assert!(!state.visual_scroll(id, 0, 0, 200, 50, 8.0, 16.0), "no-op scroll");
    assert!(state.visual_scroll(id, 10_000, 10_000, 200, 50, 8.0, 16.0));
    let slot = state.visual_slots.get(&id).unwrap();
    assert_eq!((slot.scroll_x, slot.scroll_y), (600, 350), "pinned to overflow");
    assert!(state.visual_scroll(id, -1, -1, 200, 50, 8.0, 16.0));
    // Unknown sessions never dirty.
    state.dirty = false;
    let ghost = crate::session::SessionId::fresh();
    assert!(!state.visual_scroll(ghost, 1, 1, 200, 50, 8.0, 16.0));
    assert!(!state.visual_zoom(ghost, crate::ui::visual::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
    assert!(!state.dirty);
}

#[cfg(feature = "visual")]
#[test]
fn visual_paint_fits_and_centers_a_small_diagram() {
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    complete(&mut state, id, 1, fake_png(64, 400, 400));
    // 400x400 at 8x16 cells is natively 50x25: scaled up until the
    // 50 rows are full, then centered across the 200 columns.
    let paint = state
        .visual_paint(id, ratatui::layout::Rect::new(1, 2, 200, 50), 8.0, 16.0)
        .expect("paint");
    assert_eq!((paint.out_cols, paint.out_rows), (100, 50));
    assert_eq!((paint.cursor_x, paint.cursor_y), (51, 2), "centered");
    assert_eq!((paint.sx, paint.sy, paint.sw, paint.sh), (0, 0, 400, 400));
}

#[cfg(feature = "visual")]
#[test]
fn visual_paint_png_skips_reencode_for_full_frames() {
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    let mut rgba = Vec::new();
    for p in [[255u8, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 255, 255]] {
        rgba.extend_from_slice(&p);
    }
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: fake_png(30, 2, 2),
        rgba,
        width: 2,
        height: 2,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        shapes: Vec::new(),
        vb: [0.0, 0.0, 2.0, 2.0],
        selected: None,
        input_active: false,
        chat_open: false,
        chat_scroll: 0,
        draft: None,
        questions: Vec::new(),
    });
    let full = crate::visual::VisualPaint {
        zoom_bits: 1f32.to_bits(),
        ox: 0, oy: 0, out_cols: 1, out_rows: 1,
        cursor_x: 0, cursor_y: 0,
        sx: 0, sy: 0, sw: 2, sh: 2,
        selected: None,
    };
    assert_eq!(state.visual_frame_png(id, 1, full).unwrap(), fake_png(30, 2, 2));
    // A cropped viewport re-encodes just its region: top-left red.
    let cut = crate::visual::VisualPaint { sw: 1, sh: 1, ..full };
    let png = state.visual_frame_png(id, 1, cut).expect("cropped bytes");
    let (back, w, h) = crate::visual::decode_rgba(&png).expect("decodes");
    assert_eq!((w, h), (1, 1));
    assert_eq!(&back[..4], &[255, 0, 0, 255]);
    assert!(state.visual_frame_png(id, 2, full).is_none(), "stale generation");
}

#[cfg(feature = "visual")]
#[test]
fn visual_kitty_view_carries_the_zoom_strip_without_art() {
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    complete(&mut state, id, 1, fake_png(64, 10, 10));
    let view = state.visual_view(id, true);
    let blank: String = view.lines[0].iter().map(|s| s.text.as_str()).collect();
    assert!(blank.is_empty(), "spacer row: {blank:?}");
    let strip: String = view.lines[1].iter().map(|s| s.text.as_str()).collect();
    assert!(strip.contains("zoom in") && strip.contains("zoom out"), "buttons: {strip:?}");
    let text: String = view.lines.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
    assert!(!text.contains('▀'), "image paints over the pane");
}

#[cfg(feature = "visual")]
#[test]
fn visual_view_leaves_a_spacer_row_above_the_strip() {
    // Row zero is always blank so the zoom strip and title sit one
    // row below the tab strip; the strip rides row one.
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    complete(&mut state, id, 1, fake_png(64, 10, 10));
    let view = state.visual_view(id, true);
    let blank: String = view.lines[0].iter().map(|s| s.text.as_str()).collect();
    assert!(blank.is_empty(), "spacer row: {blank:?}");
    let strip: String = view.lines[1].iter().map(|s| s.text.as_str()).collect();
    assert!(strip.contains("zoom in") && strip.contains("zoom out"), "buttons: {strip:?}");
}

#[cfg(feature = "visual")]
#[test]
fn visual_click_selects_shape_toggles_and_clears() {
    // Click resolution honors zoom and scroll: the same shape
    // selects whether the viewport is fitted or zoomed in and
    // panned. Clicking the selected shape toggles off; clicking
    // empty space clears.
    use crate::visual::ShapeBox;
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: Vec::new(),
        rgba: Vec::new(),
        width: 200,
        height: 100,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        selected: None,
        input_active: false,
        chat_open: false,
        chat_scroll: 0,
        draft: None,
        questions: Vec::new(),
        shapes: vec![
            ShapeBox { id: "big".to_string(), label: "Big".to_string(), x: 10.0, y: 10.0, width: 180.0, height: 80.0 },
            ShapeBox { id: "small".to_string(), label: "Small".to_string(), x: 50.0, y: 30.0, width: 40.0, height: 20.0 },
        ],
        vb: [0.0, 0.0, 200.0, 100.0],
    });
    // Fitted: display matches the area, so pixel == SVG unit.
    assert!(state.visual_select_at(id, 60, 20, 200, 50, 8.0, 16.0, false));
    assert_eq!(state.visual_slots.get(&id).unwrap().selected, Some(1));
    assert!(state.visual_select_at(id, 60, 20, 200, 50, 8.0, 16.0, false), "toggle off");
    assert_eq!(state.visual_slots.get(&id).unwrap().selected, None);
    assert!(!state.visual_select_at(id, 5, 5, 200, 50, 8.0, 16.0, false), "empty space, nothing selected");
    // Zoomed and panned: the same shape still resolves.
    assert!(state.visual_zoom(id, crate::ui::visual::VisualButton::ZoomIn, 200, 50, 8.0, 16.0));
    assert!(state.visual_scroll(id, 10, 5, 200, 50, 8.0, 16.0));
    assert!(state.visual_select_at(id, 65, 19, 200, 50, 8.0, 16.0, false));
    assert_eq!(state.visual_slots.get(&id).unwrap().selected, Some(1));
    // Empty space with a selection clears it.
    assert!(state.visual_select_at(id, 0, 0, 200, 50, 8.0, 16.0, false));
    assert_eq!(state.visual_slots.get(&id).unwrap().selected, None);
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_click_selects_real_node_through_full_chain() {
    // End to end on a real render: node A's box center maps to a
    // viewport cell that selects A, and the fallback view shows
    // its label highlighted. This exercises layout, inversion,
    // selection, and rendering together instead of synthetics.
    let source = "flowchart LR\n    A[Start] --> B{Decision}\n    B -->|Yes| C[OK]\n    B -->|No| D[Cancel]\n";
    let frame = crate::visual::render_frame(source).expect("renders");
    let a = frame.shapes.iter().position(|s| s.id == "A").expect("node A");
    let node = &frame.shapes[a];
    let (disp_cols, disp_rows) =
        crate::visual::fit_display(frame.width, frame.height, 1.0, 200, 50, 8.0, 16.0);
    let (cpx, cpy) = crate::visual::svg_to_source(
        node.x + node.width / 2.0,
        node.y + node.height / 2.0,
        frame.width,
        frame.height,
        &frame.vb,
    );
    let vx = cpx.saturating_mul(disp_cols as u32) / frame.width.max(1);
    let vy = cpy.saturating_mul(disp_rows as u32) / frame.height.max(1);
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: frame.png,
        rgba: frame.rgba,
        width: frame.width,
        height: frame.height,
        title: "chain".to_string(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        selected: None,
        input_active: false,
        chat_open: false,
        chat_scroll: 0,
        draft: None,
        questions: Vec::new(),
        shapes: frame.shapes,
        vb: frame.vb,
    });
    assert!(
        state.visual_select_at(id, vx.min(199) as u16, vy.min(49) as u16, 200, 50, 8.0, 16.0, false),
        "center of A selects (cell {vx},{vy})"
    );
    assert_eq!(state.visual_slots.get(&id).unwrap().selected, Some(a));
    let view = state.visual_view(id, false);
    let text: String = view.lines.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
    assert!(text.contains("Start"), "strip names the node: {text:?}");
    assert!(
        view.lines.iter().flat_map(|r| r.iter()).any(|s| {
            s.style.add_modifier.contains(ratatui::style::Modifier::REVERSED)
        }),
        "highlight renders"
    );
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_click_accounts_for_centered_diagram() {
    // Live Ghostty geometry: a tall fitted diagram centers in a
    // wider region on the Kitty path, so the picker must subtract
    // the same offset the paint cursor adds. The click below hit
    // the raster edge left-aligned and the CFG node centered.
    use crate::visual::ShapeBox;
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: Vec::new(),
        rgba: Vec::new(),
        width: 408,
        height: 1496,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        selected: None,
        input_active: false,
        chat_open: false,
        chat_scroll: 0,
        draft: None,
        questions: Vec::new(),
        shapes: vec![
            ShapeBox { id: "CFG".to_string(), label: "Cfg".to_string(), x: 70.0, y: 311.0, width: 227.0, height: 51.0 },
        ],
        vb: [0.0, 0.0, 408.0, 1496.0],
    });
    assert!(
        !state.visual_select_at(id, 92, 15, 184, 68, 8.0, 16.0, false),
        "left-aligned misses the centered diagram"
    );
    assert_eq!(state.visual_slots.get(&id).unwrap().selected, None);
    assert!(
        state.visual_select_at(id, 92, 15, 184, 68, 8.0, 16.0, true),
        "centered hits the node"
    );
    assert_eq!(state.visual_slots.get(&id).unwrap().selected, Some(0));
    assert!(
        state.visual_select_at(id, 5, 5, 184, 68, 8.0, 16.0, true),
        "margin click clears"
    );
    assert_eq!(state.visual_slots.get(&id).unwrap().selected, None);
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_selection_marks_fallback_cells() {
    // The half-block fallback shows selection as reversed cells;
    // with no selection no cell reverses.
    use crate::visual::ShapeBox;
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: Vec::new(),
        rgba: vec![0u8; 64 * 64 * 4],
        width: 64,
        height: 64,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        selected: None,
        input_active: false,
        chat_open: false,
        chat_scroll: 0,
        draft: None,
        questions: Vec::new(),
        shapes: vec![
            ShapeBox { id: "left".to_string(), label: "Left".to_string(), x: 0.0, y: 0.0, width: 20.0, height: 64.0 },
        ],
        vb: [0.0, 0.0, 64.0, 64.0],
    });
    let plain: String = state.visual_view(id, false).lines.iter()
        .flat_map(|r| r.iter()).map(|s| s.text.as_str()).collect();
    assert!(!plain.is_empty());
    let reversed = |view: crate::ui::PaneView| {
        view.lines.iter().flat_map(|r| r.iter()).filter(|s| {
            s.style.add_modifier.contains(ratatui::style::Modifier::REVERSED)
        }).count()
    };
    assert_eq!(reversed(state.visual_view(id, false)), 0, "no selection, no marks");
    state.visual_slots.get_mut(&id).unwrap().selected = Some(0);
    assert!(reversed(state.visual_view(id, false)) > 0, "selection marks cells");
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_kitty_frame_bakes_selection_border() {
    // Kitty has no cell styles: the selection rides as a baked
    // border in the transmitted bytes, so the fingerprint must
    // change with it or the gate would swallow the highlight.
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    let rgba = vec![0u8; 64 * 64 * 4];
    let png = crate::visual::encode_png(&rgba, 64, 64).expect("encodes");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png,
        rgba,
        width: 64,
        height: 64,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        selected: None,
        input_active: false,
        chat_open: false,
        chat_scroll: 0,
        draft: None,
        questions: Vec::new(),
        shapes: vec![
            crate::visual::ShapeBox { id: "box".to_string(), label: "Box".to_string(), x: 8.0, y: 8.0, width: 32.0, height: 32.0 },
        ],
        vb: [0.0, 0.0, 64.0, 64.0],
    });
    show_visual_overlay(&mut state, id);
    let has_border = |bytes: &[u8]| {
        let (rgba, _, _) = crate::visual::decode_rgba(bytes).expect("decodes");
        rgba.chunks_exact(4).any(|p| {
            p[0] == crate::visual::SELECT_RGB.0
                && p[1] == crate::visual::SELECT_RGB.1
                && p[2] == crate::visual::SELECT_RGB.2
        })
    };
    let paint = paint_for(&state, id);
    let plain = state.visual_frame_png(id, 1, paint).expect("bytes");
    assert!(!has_border(&plain), "no selection, no border");
    state.visual_slots.get_mut(&id).unwrap().selected = Some(0);
    let paint = paint_for(&state, id);
    assert_ne!(paint.selected, None, "fingerprint carries selection");
    let marked = state.visual_frame_png(id, 1, paint).expect("bytes");
    assert_ne!(marked, plain, "bytes change with selection");
    assert!(has_border(&marked), "border baked in");
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_toggle_chat_flips_footer_rows() {
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    assert_eq!(state.visual_footer_rows(id, 20), 0, "no slot yet");
    complete(&mut state, id, 1, fake_png(64, 10, 10));
    assert_eq!(state.visual_footer_rows(id, 20), 0, "closed by default");
    assert!(state.visual_toggle_chat(id));
    assert!(state.visual_slots.get(&id).unwrap().chat_open);
    assert_eq!(state.visual_footer_rows(id, 20), 6, "open reserves 30%");
    assert_eq!(state.visual_footer_rows(id, 100), 30, "scales with the tab");
    assert!(state.visual_toggle_chat(id));
    assert!(!state.visual_slots.get(&id).unwrap().chat_open);
    assert_eq!(state.visual_footer_rows(id, 20), 0);
    let (other, _) = spawn_visual_agent(&mut state, "other");
    assert!(!state.visual_toggle_chat(other), "no slot, no toggle");
    assert!(state.manager.remove(other));
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_selection_opens_chat() {
    // Clicking a shape opens the dismissed chat again; the
    // toggle only wins until the next selection.
    use crate::visual::ShapeBox;
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: Vec::new(),
        rgba: Vec::new(),
        width: 200,
        height: 100,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        selected: None,
        draft: None,
        input_active: false,
        chat_open: false,
        chat_scroll: 0,
        questions: Vec::new(),
        shapes: vec![
            ShapeBox { id: "b".to_string(), label: "B".to_string(), x: 10.0, y: 10.0, width: 180.0, height: 80.0 },
        ],
        vb: [0.0, 0.0, 200.0, 100.0],
    });
    assert!(state.visual_select_at(id, 60, 20, 200, 50, 8.0, 16.0, false));
    assert!(state.visual_slots.get(&id).unwrap().chat_open, "select opens");
    assert!(state.visual_toggle_chat(id), "dismissed");
    assert!(state.visual_select_at(id, 60, 20, 200, 50, 8.0, 16.0, false), "toggle off");
    assert!(state.visual_select_at(id, 60, 20, 200, 50, 8.0, 16.0, false), "reselect opens");
    assert!(state.visual_slots.get(&id).unwrap().chat_open);
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_footer_draws_a_titled_qa_box() {
    // The Q/A panel reads as one bordered box: rounded top with
    // the title, padded ask and history rows with solid sides,
    // rounded bottom. Every footer row is exactly content-wide so
    // the box edges align and nothing wraps.
    use ratatui::layout::Rect;
    let mut state = AppState::new();
    state.term_size = (40, 100);
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    complete(&mut state, id, 1, fake_png(64, 10, 10));
    assert!(state.visual_toggle_chat(id));
    let view = state.visual_view(id, true);
    let areas = crate::ui::layout::chrome_areas(Rect::new(0, 0, 100, 40));
    let content = crate::ui::layout::pane_content_area(&areas);
    let foot = state.visual_footer_rows(id, content.height);
    let chrome = crate::ui::visual::visual_chrome(content, state.pill_tabs, foot);
    let ask_idx = (chrome.footer.y - content.y) as usize;
    let block = &view.lines[ask_idx..ask_idx + foot as usize];
    let width = content.width as usize;
    for (i, row) in block.iter().enumerate() {
        assert_eq!(crate::ui::text::spans_width(row), width, "box row {i} fills");
    }
    let text = |row: &Vec<crate::ui::SpanView>| {
        row.iter().map(|s| s.text.as_str()).collect::<String>()
    };
    assert!(text(&block[0]).starts_with("╭") && text(&block[0]).contains("Q/A"), "titled top");
    assert!(text(&block[0]).ends_with("╮"), "top closes");
    assert!(text(&block[1]).starts_with("│"), "pad row sided");
    assert!(text(&block[foot as usize - 3]).starts_with("├"), "divider above input");
    assert!(text(&block[foot as usize - 2]).contains("Click a shape to ask"), "prompt docked at bottom");
    assert!(text(&block[foot as usize - 1]).starts_with("╰"), "bottom closes");
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_ask_row_shows_prompt_placeholder_until_typed() {
    // Selected but nothing typed yet: `Ask about "S": >` plus a
    // gray `type question here` hint. The first keystroke swaps
    // the hint for the draft text.
    use crate::visual::ShapeBox;
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: Vec::new(),
        rgba: vec![0u8; 8 * 8 * 4],
        width: 8,
        height: 8,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        selected: Some(0),
        draft: None,
        input_active: false,
        chat_open: true,
        chat_scroll: 0,
        questions: Vec::new(),
        shapes: vec![
            ShapeBox { id: "A".to_string(), label: "Start".to_string(), x: 0.0, y: 0.0, width: 8.0, height: 8.0 },
        ],
        vb: [0.0, 0.0, 8.0, 8.0],
    });
    let find_ask = |view: crate::ui::PaneView| {
        view.lines
            .into_iter()
            .find(|r| {
                r.iter().any(|s| s.text.contains(">"))
                    && !r.iter().any(|s| s.text.contains("waiting"))
            })
            .expect("prompt row")
    };
    let row = find_ask(state.visual_view(id, true));
    let text: String = row.iter().map(|s| s.text.as_str()).collect();
    assert!(text.contains(">"), "prompt marker: {text:?}");
    assert!(text.contains("type question here"), "placeholder: {text:?}");
    let hint = row.iter().find(|s| s.text.contains("type question here")).unwrap();
    assert_eq!(
        hint.style,
        crate::ui::theme::style(crate::ui::theme::Role::Muted),
        "placeholder is gray"
    );
    state.visual_slots.get_mut(&id).unwrap().draft = Some("why?".to_string());
    let row = find_ask(state.visual_view(id, true));
    let text: String = row.iter().map(|s| s.text.as_str()).collect();
    assert!(text.contains(">") && text.contains("why?"), "typed text: {text:?}");
    assert!(!text.contains("type question here"), "hint swapped out");
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_select_arms_typing_without_enter() {
    // Selecting a shape arms the prompt at once: the first
    // keystroke types, no Enter needed to focus first.
    use crate::visual::ShapeBox;
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: Vec::new(),
        rgba: Vec::new(),
        width: 200,
        height: 100,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        selected: None,
        draft: None,
        input_active: false,
        chat_open: false,
        chat_scroll: 0,
        questions: Vec::new(),
        shapes: vec![
            ShapeBox { id: "b".to_string(), label: "B".to_string(), x: 10.0, y: 10.0, width: 180.0, height: 80.0 },
        ],
        vb: [0.0, 0.0, 200.0, 100.0],
    });
    assert!(state.visual_select_at(id, 60, 20, 200, 50, 8.0, 16.0, false));
    let slot = state.visual_slots.get(&id).unwrap();
    assert!(slot.input_active, "select arms typing");
    assert_eq!(slot.draft.as_deref(), Some(""), "empty draft ready");
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_input_row_docks_at_box_bottom_with_divider() {
    // Claude-style layout: history on top, a divider line, then
    // the prompt row pinned above the bottom border.
    use crate::visual::ShapeBox;
    let mut state = AppState::new();
    state.term_size = (40, 100);
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: Vec::new(),
        rgba: vec![0u8; 8 * 8 * 4],
        width: 8,
        height: 8,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        selected: Some(0),
        draft: Some("why?".to_string()),
        input_active: true,
        chat_open: true,
        chat_scroll: 0,
        questions: vec![crate::visual::VisualQuestion {
            shape_id: "A".to_string(),
            shape_label: "Start".to_string(),
            question: "what?".to_string(),
            answer: Some("because".to_string()),
        }],
        shapes: vec![
            ShapeBox { id: "A".to_string(), label: "Start".to_string(), x: 0.0, y: 0.0, width: 8.0, height: 8.0 },
        ],
        vb: [0.0, 0.0, 8.0, 8.0],
    });
    let view = state.visual_view(id, true);
    use ratatui::layout::Rect;
    let areas = crate::ui::layout::chrome_areas(Rect::new(0, 0, 100, 40));
    let content = crate::ui::layout::pane_content_area(&areas);
    let foot = state.visual_footer_rows(id, content.height);
    let chrome = crate::ui::visual::visual_chrome(content, state.pill_tabs, foot);
    let base = (chrome.footer.y - content.y) as usize;
    let text = |i: usize| {
        view.lines[i].iter().map(|s| s.text.as_str()).collect::<String>()
    };
    assert!(text(base + foot as usize - 1).starts_with("╰"), "bottom closes");
    assert!(text(base + foot as usize - 3).starts_with("├"), "divider above input");
    let input = text(base + foot as usize - 2);
    assert!(input.contains(">") && input.contains("why?"), "prompt row: {input:?}");
    let history: String = view.lines[base + 2..base + foot as usize - 3].iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
    assert!(history.contains("what?") && history.contains("because"), "history on top");
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_strip_carries_a_shortened_alt_in_kitty_mode() {
    // Kitty mode paints no art, so a standalone alt line would
    // hide under the image: the description rides the strip row
    // instead, capped with an ellipsis, and no second copy paints
    // below it. Short descriptions arrive whole.
    let mut state = AppState::new();
    state.term_size = (40, 100);
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    complete(&mut state, id, 1, fake_png(64, 10, 10));
    state.visual_slots.get_mut(&id).unwrap().alt =
        "a very long diagram description that cannot fit the strip row at all".to_string();
    let view = state.visual_view(id, true);
    let strip = &view.lines[1];
    let text: String = strip.iter().map(|s| s.text.as_str()).collect();
    assert!(text.contains("a very long diagram"), "alt head: {text:?}");
    assert!(text.contains("…"), "long alt shortens: {text:?}");
    assert!(!text.contains("at all"), "tail cut: {text:?}");
    let rest: String = view.lines[2..].iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
    assert!(!rest.contains("a very long diagram"), "no occluded copy");
    state.visual_slots.get_mut(&id).unwrap().alt = "a diagram".to_string();
    let view = state.visual_view(id, true);
    let text: String = view.lines[1].iter().map(|s| s.text.as_str()).collect();
    assert!(text.contains("a diagram") && !text.contains("…"), "short alt whole: {text:?}");
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_footer_grows_to_thirty_percent_of_a_tall_tab() {
    // A tall tab gives the Q/A panel room: 30% of content height
    // (ask row plus history), not the old fixed seven rows.
    use ratatui::layout::Rect;
    let mut state = AppState::new();
    state.term_size = (40, 100);
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    complete(&mut state, id, 1, fake_png(64, 10, 10));
    assert!(state.visual_toggle_chat(id));
    assert_eq!(state.visual_footer_rows(id, 36), 11);
    let view = state.visual_view(id, true);
    let areas = crate::ui::layout::chrome_areas(Rect::new(0, 0, 100, 40));
    let content = crate::ui::layout::pane_content_area(&areas);
    assert_eq!(content.height, 36);
    let chrome = crate::ui::visual::visual_chrome(
        content,
        state.pill_tabs,
        state.visual_footer_rows(id, content.height),
    );
    let ask_idx = (chrome.footer.y - content.y) as usize;
    assert_eq!(view.lines.len() - ask_idx, 11, "ask plus ten history rows");
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_footer_renders_fixed_rows_with_markdown() {
    // Open chat costs its 30% footer rows: the ask row plus the
    // history viewport padded with blanks. Answers reuse the
    // walkthrough markdown skin, not plain text. A tall term
    // leaves the whole fixture visible in the viewport.
    use crate::visual::ShapeBox;
    let mut state = AppState::new();
    state.term_size = (40, 100);
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: Vec::new(),
        rgba: vec![0u8; 8 * 8 * 4],
        width: 8,
        height: 8,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        selected: Some(0),
        draft: Some("why?".to_string()),
        input_active: true,
        chat_open: true,
        chat_scroll: 0,
        questions: vec![crate::visual::VisualQuestion {
            shape_id: "A".to_string(),
            shape_label: "Start".to_string(),
            question: "what?".to_string(),
            answer: Some("**bold** done".to_string()),
        }],
        shapes: vec![
            ShapeBox { id: "A".to_string(), label: "Start".to_string(), x: 0.0, y: 0.0, width: 8.0, height: 8.0 },
        ],
        vb: [0.0, 0.0, 8.0, 8.0],
    });
    let view = state.visual_view(id, false);
    let foot = state.visual_footer_rows(id, 36) as usize;
    assert!(view.lines.len() >= foot, "footer present");
    let tail = &view.lines[view.lines.len() - foot..];
    let text: String = tail.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
    assert!(text.contains("why?"), "ask row with draft: {text:?}");
    assert!(text.contains("what?"), "question: {text:?}");
    assert!(text.contains("bold") && text.contains("done"), "answer: {text:?}");
    assert!(
        tail.iter().flat_map(|r| r.iter()).any(|s| {
            s.style.add_modifier.contains(ratatui::style::Modifier::BOLD)
        }),
        "markdown skin, not plain text"
    );
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_chat_history_separates_entries_with_blank_row() {
    // A second question must not sit directly under the previous
    // answer: entries are separated by one blank row, like the
    // walkthrough Q&A log.
    let mut state = AppState::new();
    state.term_size = (40, 100);
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: Vec::new(),
        rgba: Vec::new(),
        width: 8,
        height: 8,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        selected: None,
        draft: None,
        input_active: false,
        chat_open: true,
        chat_scroll: 0,
        questions: vec![
            crate::visual::VisualQuestion {
                shape_id: "A".to_string(),
                shape_label: "S".to_string(),
                question: "q1".to_string(),
                answer: Some("a1".to_string()),
            },
            crate::visual::VisualQuestion {
                shape_id: "A".to_string(),
                shape_label: "S".to_string(),
                question: "q2".to_string(),
                answer: Some("a2".to_string()),
            },
        ],
        shapes: Vec::new(),
        vb: [0.0, 0.0, 8.0, 8.0],
    });
    let slot = state.visual_slots.get(&id).unwrap();
    let rows = state.visual_chat_history_lines(slot);
    let text: Vec<String> = rows
        .iter()
        .map(|r| r.iter().map(|s| s.text.as_str()).collect())
        .collect();
    let second_q = text
        .iter()
        .position(|r| r.contains("q2"))
        .expect("second question renders");
    assert!(
        second_q > 0 && text[second_q - 1].is_empty(),
        "blank row before second question: {text:?}"
    );
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_chat_scroll_clamps_and_retails() {
    // Wheel offset counts rows up from the tail; answering or
    // asking re-tails so fresh content is never stranded above.
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: Vec::new(),
        rgba: Vec::new(),
        width: 8,
        height: 8,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        selected: None,
        draft: None,
        input_active: false,
        chat_open: true,
        chat_scroll: 0,
        questions: (0..10)
            .map(|i| crate::visual::VisualQuestion {
                shape_id: "A".to_string(),
                shape_label: "S".to_string(),
                question: format!("q{i}"),
                answer: Some(format!("a{i}")),
            })
            .collect(),
        shapes: Vec::new(),
        vb: [0.0, 0.0, 8.0, 8.0],
    });
    assert!(state.visual_chat_scroll(id, 3, 5));
    assert_eq!(state.visual_slots.get(&id).unwrap().chat_scroll, 3);
    assert!(state.visual_chat_scroll(id, -99, 5));
    assert_eq!(state.visual_slots.get(&id).unwrap().chat_scroll, 0, "clamped to tail");
    assert!(state.visual_chat_scroll(id, 9999, 5));
    let max = state.visual_slots.get(&id).unwrap().chat_scroll;
    assert!(max > 0, "history exceeds the viewport");
    assert!(!state.visual_chat_scroll(id, 9999, 5), "clamped at head");
    assert!(!state.visual_chat_scroll(id, 0, 5), "no-op at rest");
    assert!(state.manager.remove(id));
}

#[cfg(all(target_os = "linux", feature = "visual"))]
#[test]
fn screenshot_tool_needs_an_app_name() {
    // Broker wiring without touching the compositor: arg
    // validation answers before any subprocess spawns.
    let mut state = AppState::new();
    let (id, live_run) = spawn_visual_agent(&mut state, "agent");
    let out = comms_reply(&mut state, &live_run, "screenshot", "{}");
    assert!(out.contains("needs an app name"), "out: {out}");
    let out = comms_reply(&mut state, &live_run, "screenshot", r#"{"app":""}"#);
    assert!(out.contains("needs an app name"), "blank: {out}");
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_view_shows_input_row_and_pending_answer() {
    // The footer pins the prompt row under a selection and the
    // latest Q&A above it: question plus waiting marker until
    // answered. A tall term leaves both visible in the viewport.
    use crate::visual::ShapeBox;
    let mut state = AppState::new();
    state.term_size = (40, 100);
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: Vec::new(),
        rgba: vec![0u8; 8 * 8 * 4],
        width: 8,
        height: 8,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        selected: Some(0),
        input_active: false,
        chat_open: true,
        chat_scroll: 0,
        draft: Some("why?".to_string()),
        questions: vec![crate::visual::VisualQuestion {
            shape_id: "A".to_string(),
            shape_label: "Start".to_string(),
            question: "what?".to_string(),
            answer: None,
        }],
        shapes: vec![
            ShapeBox { id: "A".to_string(), label: "Start".to_string(), x: 0.0, y: 0.0, width: 8.0, height: 8.0 },
        ],
        vb: [0.0, 0.0, 8.0, 8.0],
    });
    let view = state.visual_view(id, false);
    let text: String = view.lines.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
    assert!(text.contains("Start") && text.contains("why?"), "ask row: {text:?}");
    assert!(text.contains("what?"), "question: {text:?}");
    assert!(text.contains(crate::visual::VISUAL_WAITING_TEXT), "waiting: {text:?}");
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_view_pins_footer_to_chrome_footer_in_kitty_mode() {
    // Kitty mode emits no art: blank filler must push the ask row
    // onto the chrome footer rows. Without it the footer paints
    // under the strip, the transmitted image paints over it, and
    // wheel routing (which uses the chrome rect) desyncs from
    // what is on screen.
    use ratatui::layout::Rect;
    let mut state = AppState::new();
    state.term_size = (40, 100);
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    complete(&mut state, id, 1, fake_png(64, 10, 10));
    assert!(state.visual_toggle_chat(id));
    let view = state.visual_view(id, true);
    let (rows, cols) = state.term_size;
    let areas = crate::ui::layout::chrome_areas(Rect::new(0, 0, cols, rows));
    let content = crate::ui::layout::pane_content_area(&areas);
    let foot = state.visual_footer_rows(id, content.height);
    let chrome = crate::ui::visual::visual_chrome(content, state.pill_tabs, foot);
    let ask_idx = (chrome.footer.y - content.y) as usize;
    assert!(
        ask_idx < view.lines.len(),
        "ask row on screen: {} lines for footer at {ask_idx}",
        view.lines.len()
    );
    let top: String = view.lines[ask_idx].iter().map(|s| s.text.as_str()).collect();
    assert!(top.contains("Q/A"), "box opens the footer: {top:?}");
    let ask: String = view.lines[ask_idx + foot as usize - 2].iter().map(|s| s.text.as_str()).collect();
    assert!(ask.contains("Click a shape to ask"), "prompt docked: {ask:?}");
    assert_eq!(view.lines.len() - ask_idx, foot as usize, "footer is the last block");
}

#[cfg(feature = "visual")]
#[test]
fn visual_view_pins_footer_below_small_fallback_art() {
    // A diagram smaller than the image region leaves blank rows;
    // the footer must still land on the chrome footer, not float
    // directly under the art.
    use ratatui::layout::Rect;
    let mut state = AppState::new();
    state.term_size = (40, 100);
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: Vec::new(),
        rgba: vec![0u8; 8 * 8 * 4],
        width: 8,
        height: 8,
        title: String::new(),
        alt: String::new(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        shapes: Vec::new(),
        vb: [0.0, 0.0, 8.0, 8.0],
        selected: None,
        input_active: false,
        chat_open: true,
        chat_scroll: 0,
        draft: None,
        questions: Vec::new(),
    });
    let view = state.visual_view(id, false);
    let (rows, cols) = state.term_size;
    let areas = crate::ui::layout::chrome_areas(Rect::new(0, 0, cols, rows));
    let content = crate::ui::layout::pane_content_area(&areas);
    let foot = state.visual_footer_rows(id, content.height);
    let chrome = crate::ui::visual::visual_chrome(content, state.pill_tabs, foot);
    let ask_idx = (chrome.footer.y - content.y) as usize;
    assert!(
        ask_idx < view.lines.len(),
        "ask row on screen: {} lines for footer at {ask_idx}",
        view.lines.len()
    );
    let top: String = view.lines[ask_idx].iter().map(|s| s.text.as_str()).collect();
    assert!(top.contains("Q/A"), "box opens the footer: {top:?}");
    let ask: String = view.lines[ask_idx + foot as usize - 2].iter().map(|s| s.text.as_str()).collect();
    assert!(ask.contains("Click a shape to ask"), "prompt docked: {ask:?}");
    assert_eq!(view.lines.len() - ask_idx, foot as usize, "footer is the last block");
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_chat_renders_on_the_bottom_rows_of_the_real_screen() {
    // End to end through the real renderer: a diagram with an
    // open chat and a waiting question must paint the strip at
    // the top and the footer pinned to the bottom, with nothing
    // leaking into the image rows between.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    let mut state = AppState::new();
    state.term_size = (40, 100);
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    complete(&mut state, id, 1, fake_png(64, 10, 10));
    assert!(state.visual_toggle_chat(id));
    state
        .visual_slots
        .get_mut(&id)
        .unwrap()
        .questions
        .push(crate::visual::VisualQuestion {
            shape_id: "A".to_string(),
            shape_label: "Start".to_string(),
            question: "what is this?".to_string(),
            answer: None,
        });
    let view = state.visual_view(id, true);
    let (rows, cols) = state.term_size;
    let area = Rect::new(0, 0, cols, rows);
    let areas = crate::ui::layout::chrome_areas(area);
    let content = crate::ui::layout::pane_content_area(&areas);
    let foot = state.visual_footer_rows(id, content.height);
    let chrome = crate::ui::visual::visual_chrome(content, state.pill_tabs, foot);
    let chrome_ui = crate::ui::Chrome {
        tabs: vec![crate::ui::session_bar::SessionTab {
            title: "agent".to_string(),
            live: true,
            focused: true,
            group: None,
            group_color: None,
        }],
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
    terminal.draw(|f| crate::ui::render(f, area, &[view], &chrome_ui)).unwrap();
    let buf = terminal.backend().buffer();
    let w = buf.area.width as usize;
    let screen: Vec<String> = buf
        .content
        .chunks(w)
        .map(|row| row.iter().map(|c| c.symbol().to_string()).collect())
        .collect();
    assert!(screen[content.y as usize + 1].contains("chat"), "strip: {:?}", screen[3]);
    let foot_y = chrome.footer.y as usize;
    assert!(screen[foot_y].contains("╭─ Q/A"), "box opens on screen");
    assert!(screen[foot_y + foot as usize - 1].contains("╰"), "box closes on screen");
    let bottom: String = screen[foot_y..foot_y + foot as usize].join("\n");
    assert!(bottom.contains("Click a shape to ask"), "ask row: {bottom:?}");
    assert!(bottom.contains("what is this?"), "question: {bottom:?}");
    assert!(
        bottom.contains(crate::visual::VISUAL_WAITING_TEXT),
        "waiting: {bottom:?}"
    );
    let middle: String = screen[content.y as usize + 2..foot_y].join("\n");
    assert!(
        !middle.contains("what is this?"),
        "footer must not float under the strip"
    );
    assert!(state.manager.remove(id));
}

#[cfg(feature = "visual")]
#[test]
fn visual_view_falls_back_to_halfblock_with_alt() {
    let mut state = AppState::new();
    let (id, _) = spawn_visual_agent(&mut state, "agent");
    // 2x2: red/green over blue/white, titled with alt text.
    let mut rgba = Vec::new();
    for p in [[255u8, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 255, 255]] {
        rgba.extend_from_slice(&p);
    }
    state.visual_slots.insert(id, crate::app::VisualSlot {
        generation: 1,
        png: Vec::new(),
        rgba,
        width: 2,
        height: 2,
        title: "flow".to_string(),
        alt: "a diagram".to_string(),
        zoom: 1.0,
        scroll_x: 0,
        scroll_y: 0,
        shapes: Vec::new(),
        vb: [0.0, 0.0, 2.0, 2.0],
        selected: None,
        input_active: false,
        chat_open: false,
        chat_scroll: 0,
        draft: None,
        questions: Vec::new(),
    });
    let view = state.visual_view(id, false);
    assert!(view.title.contains("Visual"), "pane title: {}", view.title);
    let text: String = view.lines.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
    assert!(text.contains("flow"), "title line: {text:?}");
    assert!(text.contains("a diagram"), "alt line: {text:?}");
    assert!(text.contains('▀'), "half-block art: {text:?}");
    // Row zero is the blank spacer; the zoom strip carrying the
    // title rides row one and art follows, since the fitted image
    // fills every row below the strip.
    let blank: String = view.lines[0].iter().map(|s| s.text.as_str()).collect();
    assert!(blank.is_empty(), "spacer row: {blank:?}");
    let strip: String = view.lines[1].iter().map(|s| s.text.as_str()).collect();
    assert!(strip.contains("zoom in") && strip.contains("zoom out"), "buttons: {strip:?}");
    assert!(strip.contains("flow"), "title on the strip: {strip:?}");
    let cell = &view.lines[2][0];
    assert_eq!(cell.style.fg, Some(ratatui::style::Color::Rgb(255, 0, 0)));
    assert_eq!(cell.style.bg, Some(ratatui::style::Color::Rgb(0, 0, 255)));
    // Kitty mode carries title and alt for the record, no art.
    let view = state.visual_view(id, true);
    let text: String = view.lines.iter().flat_map(|r| r.iter().map(|s| s.text.as_str())).collect();
    assert!(text.contains("flow") && text.contains("a diagram"));
    assert!(!text.contains('▀'), "image paints over the pane");
}
