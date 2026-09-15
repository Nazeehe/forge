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
pub fn kitty_transmit(png: &[u8], image_id: u32, cols: u16) -> String {
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
            out.push_str(&format!(
                "\x1b_Ga=T,f=100,t=d,S={},V={},i={image_id},c={cols},m={more};",
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

/// Delete escape that also frees the image data (`a=D,d=I`).
pub fn kitty_delete(image_id: u32) -> String {
    format!("\x1b_Ga=D,d=I,i={image_id}\x1b\\")
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
        let esc = kitty_transmit(&png, 7, 80);
        assert!(esc.starts_with("\x1b_G"), "APC open");
        for key in ["a=T", "f=100", "t=d", "i=7", "c=80"] {
            assert!(esc.contains(key), "carries {key}: {esc:?}");
        }
        assert!(esc.contains(&base64_encode(&png)), "payload rides");
        assert!(esc.ends_with("\x1b\\"), "ST close");
        let big = vec![0x41; KITTY_CHUNK + 10];
        let esc = kitty_transmit(&big, 9, 80);
        assert_eq!(esc.matches("\x1b_G").count(), 2, "two chunks");
        assert!(esc.contains("m=1;"), "continuation marked");
        assert!(esc.contains("m=0;"), "final marked");
    }

    #[test]
    fn kitty_delete_frees_by_id() {
        assert_eq!(kitty_delete(9), "\x1b_Ga=D,d=I,i=9\x1b\\");
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
