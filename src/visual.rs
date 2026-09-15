//! Mermaid diagrams for the Visual tab (U1 spike): flowchart source in,
//! PNG bytes out. No terminal, no state, no wiring yet.

static SPIKE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Largest raster accepted into a slot: enforced on the decoded PNG
/// before storing, so a hostile diagram cannot eat the image budget
/// in one frame. Transient worker memory is bounded by the input cap.
pub const MAX_RASTER_PIXELS: u64 = 2048 * 2048;

/// One rasterized diagram, measured and pixel-capped: PNG bytes for
/// Kitty transmit plus decoded RGBA8 for the half-block fallback.
pub struct RasterFrame {
    pub png: Vec<u8>,
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Render plus measure plus pixel cap plus RGBA decode, all on the
/// worker. In-memory raster would need a direct resvg dep; the
/// scratch file stands until that earns its keep.
pub fn render_frame(source: &str) -> Result<RasterFrame, String> {
    let png = render_png_bytes(source)?;
    let (width, height) = png_dimensions(&png)?;
    if width as u64 * height as u64 > MAX_RASTER_PIXELS {
        return Err(format!(
            "raster {width}x{height} exceeds {MAX_RASTER_PIXELS} pixels"
        ));
    }
    let (rgba, dw, dh) = decode_rgba(&png)?;
    if dw != width || dh != height {
        return Err("decoded dimensions disagree with IHDR".to_string());
    }
    Ok(RasterFrame { png, rgba, width, height })
}

/// Render one Mermaid diagram to PNG bytes. The crate's PNG writer is
/// file-based, so rendering round-trips a scratch file.
pub fn render_png_bytes(source: &str) -> Result<Vec<u8>, String> {
    let svg = mermaid_rs_renderer::render(source).map_err(|e| e.to_string())?;
    let n = SPIKE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("forge-visual-{n}-{}.png", std::process::id()));
    mermaid_rs_renderer::render::write_output_png(
        &svg,
        &path,
        &mermaid_rs_renderer::RenderConfig::default(),
        &mermaid_rs_renderer::Theme::modern(),
    )
    .map_err(|e| e.to_string())?;
    let bytes =
        std::fs::read(&path).map_err(|e| format!("cannot read scratch png: {e}"))?;
    let _ = std::fs::remove_file(&path);
    Ok(bytes)
}

/// Largest diagram source accepted: enforced before any allocation.
pub const MAX_SOURCE_CHARS: usize = 65_536;

/// Validate a show request without rendering: format allowlist first
/// (the error names the supported set), then non-empty bounded content.
pub fn check_request(content: &str, format: &str) -> Result<(), String> {
    if format != "mermaid" {
        return Err(format!(
            "unsupported format {format:?}: supported formats: mermaid"
        ));
    }
    if content.is_empty() {
        return Err("visual_show needs content".to_string());
    }
    if content.chars().count() > MAX_SOURCE_CHARS {
        return Err(format!(
            "content exceeds {MAX_SOURCE_CHARS} characters"
        ));
    }
    Ok(())
}

/// Width/height from the PNG IHDR chunk: bytes 16..24, big-endian u32s.
pub fn png_dimensions(png: &[u8]) -> Result<(u32, u32), String> {
    if png.len() < 24 || !png.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Err("not a PNG".to_string());
    }
    let w = u32::from_be_bytes(png[16..20].try_into().map_err(|_| "short IHDR".to_string())?);
    let h = u32::from_be_bytes(png[20..24].try_into().map_err(|_| "short IHDR".to_string())?);
    Ok((w, h))
}

/// Kitty graphics chunk size: base64 payload bytes per escape.
pub const KITTY_CHUNK: usize = 4096;

/// Whether the Kitty graphics protocol is expected: Ghostty/kitty
/// advertise via TERM_PROGRAM, kitty also via TERM.
pub fn kitty_supported(term_program: Option<&str>, term: Option<&str>) -> bool {
    let program = term_program.unwrap_or_default().to_lowercase();
    if program == "ghostty" || program == "kitty" {
        return true;
    }
    term.unwrap_or_default().to_lowercase().contains("kitty")
}

/// Process environment version of [`kitty_supported`].
pub fn kitty_supported_env() -> bool {
    let program = std::env::var("TERM_PROGRAM").ok();
    let term = std::env::var("TERM").ok();
    kitty_supported(program.as_deref(), term.as_deref())
}

/// Standard base64 (no newlines) for Kitty payloads.
pub fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let mut n: u32 = 0;
        for (i, &b) in chunk.iter().enumerate() {
            n |= (b as u32) << (16 - 8 * i);
        }
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Transmit-and-display escape for PNG bytes at the cursor: `c` cell
/// columns wide, aspect preserved by the terminal. Payload chunks at
/// [`KITTY_CHUNK`] with `m=1` continuations; the caller positions the
/// cursor first.
pub fn kitty_transmit(png: &[u8], image_id: u32, cols: u16, rows: u16) -> String {
    let payload = base64_encode(png);
    let raw = payload.as_bytes();
    let mut chunks: Vec<&[u8]> = raw.chunks(KITTY_CHUNK).collect();
    if chunks.is_empty() {
        chunks.push(b"");
    }
    let total = chunks.len();
    let mut out = String::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = if i + 1 < total { 1 } else { 0 };
        if i == 0 {
            // Fixed placement id: re-transmitting the same (image,
            // placement) pair replaces in place without flicker, per
            // the spec. Without `p`, a same-id re-display flashes.
            out.push_str(&format!(
                "\x1b_Ga=T,f=100,t=d,S={},V={},i={image_id},p=1,c={cols},r={rows},m={more};",
                png.len(),
                png.len()
            ));
        } else {
            out.push_str(&format!("\x1b_Gm={more};"));
        }
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        out.push_str("\x1b\\");
    }
    out
}

/// Delete one image by id, freeing its data too (`a=d,d=I`). The
/// action is lowercase: an uppercase action is ignored and the
/// placement leaks, ghosting over later tabs and stacking a fresh
/// copy on every revisit.
pub fn kitty_delete(image_id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={image_id}\x1b\\")
}

/// Delete all visible placements (`a=d` alone). Startup purge for
/// placements leaked while the delete above was malformed: those ids
/// are untracked, so only a blanket clear reclaims them.
pub fn kitty_delete_all() -> String {
    "\x1b_Ga=d\x1b\\".to_string()
}

/// One half-block cell: upper pixel on lower pixel.
pub struct HalfCell {
    pub ch: char,
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
}

/// RGBA8 rows to half-block rows, nearest-neighbor scaled to
/// `max_cols`. A missing bottom row pads opaque black.
pub fn halfblock_rows(rgba: &[u8], width: u32, height: u32, max_cols: usize) -> Vec<Vec<HalfCell>> {
    let (width, height) = (width as usize, height as usize);
    if width == 0 || height == 0 || max_cols == 0 {
        return Vec::new();
    }
    let cols = width.min(max_cols);
    let mut rows = Vec::new();
    let mut y = 0;
    while y < height {
        let mut row = Vec::with_capacity(cols);
        for dx in 0..cols {
            let sx = dx * width / cols;
            row.push(HalfCell {
                ch: '▀',
                fg: sample(rgba, width, height, sx, y),
                bg: sample(rgba, width, height, sx, y + 1),
            });
        }
        rows.push(row);
        y += 2;
    }
    rows
}

/// One RGB triple, alpha flattened away; out-of-range reads pad black.
fn sample(rgba: &[u8], width: usize, height: usize, x: usize, y: usize) -> (u8, u8, u8) {
    if x >= width || y >= height {
        return (0, 0, 0);
    }
    let i = (y * width + x) * 4;
    match rgba.get(i..i + 3) {
        Some(s) if s.len() == 3 => (s[0], s[1], s[2]),
        _ => (0, 0, 0),
    }
}

/// Decode PNG bytes to RGBA8 triples plus dimensions. Only 8-bit
/// RGB/RGBA sources are accepted (everything resvg emits).
pub fn decode_rgba(png_bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let decoder = png::Decoder::new(png_bytes);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("png header: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("png decode: {e}"))?;
    if info.bit_depth != png::BitDepth::Eight {
        return Err("only 8-bit PNG sources are supported".to_string());
    }
    let frame = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => frame.to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(frame.len() / 3 * 4);
            for px in frame.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        other => return Err(format!("unsupported PNG color type: {other:?}")),
    };
    Ok((rgba, info.width, info.height))
}

/// Fallback text-cell size in pixels, used when the terminal does
/// not report its pixel dimensions (many do not fill in
/// `ws_xpixel`/`ws_ypixel`): 8x16 keeps the 1:2 cell aspect the
/// half-block fallback draws with exactly.
pub const FALLBACK_CELL_PX: (f64, f64) = (8.0, 16.0);

/// Zoom direction for the Visual tab buttons and `+`/`-` keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomDir {
    In,
    Out,
}

/// Zoom factor per step and its clamps: 1.25x per press keeps text
/// readable for a few steps without jumping past the content.
pub const ZOOM_STEP: f32 = 1.25;
pub const MIN_ZOOM: f32 = 0.25;
pub const MAX_ZOOM: f32 = 8.0;

/// Text-cell size from the terminal's pixel report, guarded: any
/// zero or nonsense dimension falls back to [`FALLBACK_CELL_PX`].
pub fn cell_px(term_cols: u16, term_rows: u16, px_w: u32, px_h: u32) -> (f64, f64) {
    if term_cols == 0 || term_rows == 0 || px_w == 0 || px_h == 0 {
        return FALLBACK_CELL_PX;
    }
    let (cw, ch) = (px_w as f64 / term_cols as f64, px_h as f64 / term_rows as f64);
    if !cw.is_finite() || !ch.is_finite() || cw <= 0.0 || ch <= 0.0 {
        return FALLBACK_CELL_PX;
    }
    (cw, ch)
}

/// Display size in cells for an image in a tab region: contain the
/// native pixels (never upscale past 1.0, so a small diagram stays
/// small and centered instead of taking the tab fullscreen), then
/// apply the zoom factor, which may overflow the region for
/// scrolling. Always at least one cell per axis.
pub fn fit_display(
    img_w: u32,
    img_h: u32,
    zoom: f32,
    area_cols: u16,
    area_rows: u16,
    cell_w: f64,
    cell_h: f64,
) -> (u16, u16) {
    let (img_w, img_h) = (img_w.max(1) as f64, img_h.max(1) as f64);
    let (cell_w, cell_h) = (cell_w.max(1.0), cell_h.max(1.0));
    let fit = (1.0f64)
        .min(area_cols as f64 * cell_w / img_w)
        .min(area_rows as f64 * cell_h / img_h);
    let scale = (fit * zoom.max(MIN_ZOOM) as f64).max(0.0);
    let cols = (img_w * scale / cell_w).round().max(1.0) as u16;
    let rows = (img_h * scale / cell_h).round().max(1.0) as u16;
    (cols, rows)
}

/// One zoom step, clamped to [`MIN_ZOOM`]/[`MAX_ZOOM`].
pub fn zoom_step(level: f32, dir: ZoomDir) -> f32 {
    let next = match dir {
        ZoomDir::In => level * ZOOM_STEP,
        ZoomDir::Out => level / ZOOM_STEP,
    };
    next.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// Pin a scroll offset (in displayed cells) to the overflow of the
/// display past the tab region; when the display fits, no scroll.
pub fn clamp_scroll(
    ox: u16,
    oy: u16,
    disp_cols: u16,
    disp_rows: u16,
    area_cols: u16,
    area_rows: u16,
) -> (u16, u16) {
    let max_x = disp_cols.saturating_sub(area_cols);
    let max_y = disp_rows.saturating_sub(area_rows);
    (ox.min(max_x), oy.min(max_y))
}

/// Source-pixel crop plus output cells for the visible part of a
/// zoomed/scrolled display: the viewport starts at (`ox`, `oy`) in
/// displayed cells and is cut to the tab region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewCrop {
    pub sx: u32,
    pub sy: u32,
    pub sw: u32,
    pub sh: u32,
    pub out_cols: u16,
    pub out_rows: u16,
}

pub fn crop_for_view(
    img_w: u32,
    img_h: u32,
    disp_cols: u16,
    disp_rows: u16,
    area_cols: u16,
    area_rows: u16,
    ox: u16,
    oy: u16,
) -> ViewCrop {
    let (img_w, img_h) = (img_w.max(1), img_h.max(1));
    let (disp_cols, disp_rows) = (disp_cols.max(1) as u32, disp_rows.max(1) as u32);
    let (ox, oy) = (ox as u32, oy as u32);
    let out_cols = (disp_cols.saturating_sub(ox).min(area_cols as u32)).max(1) as u16;
    let out_rows = (disp_rows.saturating_sub(oy).min(area_rows as u32)).max(1) as u16;
    let sx = (ox.saturating_mul(img_w) / disp_cols).min(img_w - 1);
    let sy = (oy.saturating_mul(img_h) / disp_rows).min(img_h - 1);
    let sw = ((out_cols as u32).saturating_mul(img_w) / disp_cols).clamp(1, img_w - sx);
    let sh = ((out_rows as u32).saturating_mul(img_h) / disp_rows).clamp(1, img_h - sy);
    ViewCrop { sx, sy, sw, sh, out_cols, out_rows }
}

/// Copy a [`ViewCrop`] rectangle out of decoded RGBA8. Out-of-range
/// crops clamp instead of panicking on a stale layout racing a new
/// raster.
pub fn crop_rgba(rgba: &[u8], img_w: u32, img_h: u32, crop: ViewCrop) -> Vec<u8> {
    let stride = img_w.max(1) as usize * 4;
    let rows = img_h.max(1) as usize;
    let sx = (crop.sx as usize * 4).min(stride);
    let sy = (crop.sy as usize).min(rows.saturating_sub(1));
    let sw = (crop.sw as usize * 4).min(stride.saturating_sub(sx));
    let sh = (crop.sh as usize).min(rows.saturating_sub(sy));
    let mut out = Vec::with_capacity(sw * sh);
    for y in 0..sh {
        let base = (sy + y) * stride + sx;
        out.extend_from_slice(&rgba.get(base..base + sw).unwrap_or(&[]));
    }
    out
}

/// Encode RGBA8 to PNG bytes for Kitty transmit of a cropped view.
pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width.max(1), height.max(1));
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("png header failed: {e}"))?;
    writer
        .write_image_data(rgba)
        .map_err(|e| format!("png encode failed: {e}"))?;
    drop(writer);
    Ok(out)
}

/// Fingerprint of exactly what the terminal shows for one visual:
/// zoom/scroll plus the placed crop and cursor. The show gate
/// repaints only when this changes, so scrolling and zooming
/// re-transmit while an untouched frame costs nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualPaint {
    pub zoom_bits: u32,
    pub ox: u16,
    pub oy: u16,
    pub out_cols: u16,
    pub out_rows: u16,
    pub cursor_x: u16,
    pub cursor_y: u16,
    pub sx: u32,
    pub sy: u32,
    pub sw: u32,
    pub sh: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_spike_flowchart_renders_png_bytes() {
        let source = "flowchart LR\n    A[Start] --> B{Decision}\n    B -->|Yes| C[OK]\n    B -->|No| D[Cancel]\n";
        let png = render_png_bytes(source).expect("spike renders");
        assert!(
            png.starts_with(&[0x89, b'P', b'N', b'G']),
            "PNG magic, got {} bytes",
            png.len()
        );
        let (w, h) = png_dimensions(&png).expect("IHDR parses");
        assert!(w > 100 && h > 100, "sane dimensions: {w}x{h}");
    }

    #[test]
    fn base64_vectors_match_rfc() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn kitty_supported_matches_known_terminals() {
        assert!(kitty_supported(Some("ghostty"), None));
        assert!(kitty_supported(Some("kitty"), None));
        assert!(kitty_supported(Some("xterm-256color"), Some("xterm-kitty")));
        assert!(!kitty_supported(Some("xterm-256color"), Some("xterm-256color")));
        assert!(!kitty_supported(None, None));
        assert!(!kitty_supported(Some("tmux"), Some("screen")));
    }

    #[test]
    fn kitty_transmit_frames_single_and_chunked_payloads() {
        let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
        let esc = kitty_transmit(&png, 7, 80, 24);
        assert!(esc.starts_with("\x1b_G"), "APC open");
        for key in ["a=T", "f=100", "t=d", "i=7", "p=1", "c=80", "r=24"] {
            assert!(esc.contains(key), "carries {key}: {esc:?}");
        }
        assert!(esc.contains(&base64_encode(&png)), "payload rides");
        assert!(esc.ends_with("\x1b\\"), "ST close");
        let big = vec![0x41; KITTY_CHUNK + 10];
        let esc = kitty_transmit(&big, 9, 80, 24);
        assert_eq!(esc.matches("\x1b_G").count(), 2, "two chunks");
        assert!(esc.contains("m=1;"), "continuation marked");
        assert!(esc.contains("m=0;"), "final marked");
    }

    #[test]
    fn kitty_delete_frees_by_id() {
        // Spec section "How are images deleted": the action is
        // lowercase `a=d`; uppercase `d=I` also frees the image data.
        // An uppercase action is ignored, leaking the placement.
        assert_eq!(kitty_delete(9), "\x1b_Ga=d,d=I,i=9\x1b\\");
    }

    #[test]
    fn kitty_delete_all_clears_visible_placements() {
        // Spec example: `a=d` alone deletes all visible placements.
        assert_eq!(kitty_delete_all(), "\x1b_Ga=d\x1b\\");
    }

    fn px(r: u8, g: u8, b: u8) -> [u8; 4] {
        [r, g, b, 255]
    }

    #[test]
    fn halfblock_maps_two_rows_to_one() {
        // 2x2: red/green over blue/white.
        let mut rgba = Vec::new();
        for p in [px(255, 0, 0), px(0, 255, 0), px(0, 0, 255), px(255, 255, 255)] {
            rgba.extend_from_slice(&p);
        }
        let rows = halfblock_rows(&rgba, 2, 2, 80);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[0][0].ch, '▀');
        assert_eq!(rows[0][0].fg, (255, 0, 0));
        assert_eq!(rows[0][0].bg, (0, 0, 255));
        assert_eq!(rows[0][1].fg, (0, 255, 0));
        assert_eq!(rows[0][1].bg, (255, 255, 255));
    }

    #[test]
    fn halfblock_pads_odd_height_and_scales_width() {
        // 2x1 red row: bottom pads opaque black.
        let mut rgba = Vec::new();
        for _ in 0..2 {
            rgba.extend_from_slice(&px(255, 0, 0));
        }
        let rows = halfblock_rows(&rgba, 2, 1, 80);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].bg, (0, 0, 0));
        // 4-wide scaled to 2 columns: nearest neighbor picks x0 and x2.
        let mut wide = Vec::new();
        for x in [10u8, 20, 30, 40] {
            wide.extend_from_slice(&px(x, 0, 0));
        }
        let mut tall = wide.clone();
        tall.extend_from_slice(&wide);
        let rows = halfblock_rows(&tall, 4, 2, 2);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[0][0].fg.0, 10);
        assert_eq!(rows[0][1].fg.0, 30);
    }

    #[test]
    fn fit_display_never_upscales_a_small_diagram() {
        // 400x200px on 8x16 cells is natively 50x12.5 cells; a
        // 200x50-cell tab must show it at native size, centered, not
        // stretched to the full tab width.
        let (cols, rows) = fit_display(400, 200, 1.0, 200, 50, 8.0, 16.0);
        assert_eq!((cols, rows), (50, 13));
    }

    #[test]
    fn fit_display_contains_a_large_diagram_with_aspect() {
        // 3000x2000 in a 200x50 tab (1600x800px): scale 0.4.
        let (cols, rows) = fit_display(3000, 2000, 1.0, 200, 50, 8.0, 16.0);
        assert_eq!((cols, rows), (150, 50));
        let shown = cols as f64 * 8.0 / (rows as f64 * 16.0);
        let native = 3000.0 / 2000.0;
        assert!((shown - native).abs() < 0.05, "aspect kept: {shown}");
    }

    #[test]
    fn fit_display_zoom_may_exceed_the_tab_for_scrolling() {
        let (cols, rows) = fit_display(400, 200, 2.0, 200, 50, 8.0, 16.0);
        assert_eq!((cols, rows), (100, 25));
    }

    #[test]
    fn fit_display_never_returns_zero_cells() {
        assert_eq!(fit_display(10, 10, 0.25, 200, 50, 8.0, 16.0), (1, 1));
    }

    #[test]
    fn zoom_step_multiplies_and_clamps() {
        assert_eq!(zoom_step(1.0, ZoomDir::In), 1.25);
        assert_eq!(zoom_step(1.0, ZoomDir::Out), 0.8);
        assert_eq!(zoom_step(8.0, ZoomDir::In), 8.0);
        assert_eq!(zoom_step(0.25, ZoomDir::Out), 0.25);
    }

    #[test]
    fn clamp_scroll_pins_offsets_to_the_overflow() {
        // 100x40 displayed in a 60x20 tab: at most (40, 20).
        assert_eq!(clamp_scroll(500, 500, 100, 40, 60, 20), (40, 20));
        assert_eq!(clamp_scroll(7, 9, 100, 40, 60, 20), (7, 9));
        // Display fits: no scroll at all.
        assert_eq!(clamp_scroll(7, 9, 40, 10, 60, 20), (0, 0));
    }

    #[test]
    fn crop_for_view_is_identity_when_the_display_fits() {
        let crop = crop_for_view(400, 200, 50, 13, 200, 50, 0, 0);
        assert_eq!((crop.sx, crop.sy, crop.sw, crop.sh), (0, 0, 400, 200));
        assert_eq!((crop.out_cols, crop.out_rows), (50, 13));
    }

    #[test]
    fn crop_for_view_maps_scrolled_cells_to_source_pixels() {
        // 1000x500 over 100 displayed cols in a 60-col tab, 10 cols in.
        let crop = crop_for_view(1000, 500, 100, 50, 60, 50, 10, 5);
        assert_eq!((crop.sx, crop.sy), (100, 50));
        assert_eq!((crop.out_cols, crop.out_rows), (60, 45));
        assert_eq!((crop.sw, crop.sh), (600, 450));
    }

    #[test]
    fn crop_rgba_extracts_the_sub_rectangle() {
        // 4x2 image, pixel value = x + 10*y in R.
        let mut rgba = Vec::new();
        for y in 0..2u8 {
            for x in 0..4u8 {
                rgba.extend_from_slice(&[x + 10 * y, 0, 0, 255]);
            }
        }
        let crop = ViewCrop { sx: 1, sy: 0, sw: 2, sh: 2, out_cols: 2, out_rows: 2 };
        let cut = crop_rgba(&rgba, 4, 2, crop);
        assert_eq!(cut.len(), 2 * 2 * 4);
        assert_eq!(cut[0], 1);
        assert_eq!(cut[4], 2);
        assert_eq!(cut[8], 11);
    }

    #[test]
    fn encode_png_round_trips_through_the_decoder() {
        let rgba: Vec<u8> = (0..16).flat_map(|i| [i * 16, 0, 0, 255]).collect();
        let png = encode_png(&rgba, 4, 4).expect("encodes");
        let (back, w, h) = decode_rgba(&png).expect("decodes");
        assert_eq!((w, h), (4, 4));
        assert_eq!(back, rgba);
    }

    #[test]
    fn cell_px_falls_back_when_the_terminal_reports_no_pixels() {
        assert_eq!(cell_px(200, 50, 0, 0), (8.0, 16.0));
        assert_eq!(cell_px(200, 50, 1600, 800), (8.0, 16.0));
        assert_eq!(cell_px(0, 50, 1600, 800), (8.0, 16.0));
    }

    #[test]
    fn decode_rgba_round_trips_rendered_png() {
        let png = render_png_bytes(
            "flowchart LR\n    A-->B\n",
        )
        .expect("renders");
        let (rgba, w, h) = decode_rgba(&png).expect("decodes");
        let (ew, eh) = png_dimensions(&png).unwrap();
        assert_eq!((w, h), (ew, eh));
        assert_eq!(rgba.len(), w as usize * h as usize * 4);
        assert!(decode_rgba(b"nope").is_err(), "garbage rejected");
    }
}
