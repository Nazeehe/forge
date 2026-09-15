//! Mermaid diagrams for the Visual tab (U1 spike): flowchart source in,
//! PNG bytes out. No terminal, no state, no wiring yet.

static SPIKE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Largest raster accepted into a slot: enforced on the decoded PNG
/// before storing, so a hostile diagram cannot eat the image budget
/// in one frame. Transient worker memory is bounded by the input cap.
pub const MAX_RASTER_PIXELS: u64 = 2048 * 2048;

/// One rasterized diagram, measured and pixel-capped.
pub struct RasterFrame {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Render plus measure plus pixel cap. In-memory raster would need a
/// direct resvg dep; the scratch file stands until that earns its keep.
pub fn render_frame(source: &str) -> Result<RasterFrame, String> {
    let png = render_png_bytes(source)?;
    let (width, height) = png_dimensions(&png)?;
    if width as u64 * height as u64 > MAX_RASTER_PIXELS {
        return Err(format!(
            "raster {width}x{height} exceeds {MAX_RASTER_PIXELS} pixels"
        ));
    }
    Ok(RasterFrame { png, width, height })
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
}
