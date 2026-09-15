//! Mermaid diagrams for the Visual tab (U1 spike): flowchart source in,
//! PNG bytes out. No terminal, no state, no wiring yet.

static SPIKE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Render one Mermaid diagram to PNG bytes. The crate's PNG writer is
/// file-based, so the spike round-trips a scratch file; U3 decides
/// whether in-memory raster earns a direct resvg dep.
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
