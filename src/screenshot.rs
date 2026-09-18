//! Window screenshots on Linux, Hyprland first: `hyprctl` lists,
//! `grim` captures, PNG output fits the visual raster pixel budget.
//! Every subprocess is bounded; anything missing or slow is a plain
//! error string, never a hang, so the relay stays fail-open.

/// One compositor window: identity plus layout box in logical pixels
/// (`grim -g` takes the same coordinates).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub address: String,
    pub class: String,
    pub title: String,
    pub workspace: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// A finished capture: scratch path plus measured dimensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shot {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub class: String,
    pub title: String,
}

/// Bound for one subprocess: listing is quick, capture can be slow
/// on big scaled outputs.
pub const LIST_TIMEOUT_MS: u64 = 5_000;
pub const CAPTURE_TIMEOUT_MS: u64 = 15_000;

/// Parse `hyprctl clients -j` into windows. Unknown fields ignored
/// so compositor upgrades never break the parse.
pub fn parse_clients(json: &str) -> Result<Vec<Window>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("hyprctl clients unreadable: {e}"))?;
    let list = value.as_array().ok_or_else(|| "hyprctl clients is not a list".to_string())?;
    let str_field = |v: &serde_json::Value, key: &str| {
        v.get(key).and_then(|f| f.as_str()).unwrap_or("").to_string()
    };
    let int_pair = |v: &serde_json::Value, key: &str| -> (i32, i32) {
        let pair = v.get(key).and_then(|f| f.as_array());
        let at = |i: usize| {
            pair.and_then(|p| p.get(i)).and_then(|n| n.as_i64()).unwrap_or(0) as i32
        };
        (at(0), at(1))
    };
    let mut windows = Vec::new();
    for v in list {
        let (x, y) = int_pair(v, "at");
        let (w, h) = int_pair(v, "size");
        let workspace = v
            .get("workspace")
            .and_then(|ws| ws.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        windows.push(Window {
            address: str_field(v, "address"),
            class: str_field(v, "class"),
            title: str_field(v, "title"),
            workspace,
            x,
            y,
            w,
            h,
        });
    }
    Ok(windows)
}

/// Parse `hyprctl activeworkspace -j` into its workspace name.
pub fn active_workspace(json: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("active workspace unreadable: {e}"))?;
    value
        .get("name")
        .and_then(|n| n.as_str())
        .filter(|n| !n.is_empty())
        .map(|n| n.to_string())
        .ok_or_else(|| "active workspace has no name".to_string())
}

/// Pick one window for an app name: a lone class substring wins,
/// several class hits stay ambiguous, otherwise a lone title
/// substring wins. Zero hits name the miss; several name every
/// candidate so the caller can disambiguate.
pub fn match_window(windows: &[Window], app: &str) -> Result<Window, String> {
    let needle = app.trim().to_lowercase();
    if needle.is_empty() {
        return Err("screenshot needs an app name".to_string());
    }
    let class_hits: Vec<&Window> = windows
        .iter()
        .filter(|w| w.class.to_lowercase().contains(&needle))
        .collect();
    if class_hits.len() == 1 {
        return Ok(class_hits[0].clone());
    }
    if class_hits.len() > 1 {
        return Err(ambiguous(app, &class_hits));
    }
    let title_hits: Vec<&Window> = windows
        .iter()
        .filter(|w| w.title.to_lowercase().contains(&needle))
        .collect();
    match title_hits.len() {
        1 => Ok(title_hits[0].clone()),
        0 => Err(format!("no window matching \"{app}\"")),
        _ => Err(ambiguous(app, &title_hits)),
    }
}

fn ambiguous(app: &str, candidates: &[&Window]) -> String {
    let list: Vec<String> = candidates
        .iter()
        .map(|w| format!("{} — {}", w.class, w.title))
        .collect();
    format!("ambiguous \"{app}\": {}", list.join("; "))
}

/// `grim -g` geometry for one window: `x,y WxH` in layout pixels.
pub fn grim_geometry(window: &Window) -> String {
    format!("{},{} {}x{}", window.x, window.y, window.w, window.h)
}

/// Run one helper with a hard timeout, killing it past the bound.
/// Stdout past 8 MiB aborts the same way: listings are small.
pub fn run_bounded(
    prog: &str,
    args: &[&str],
    timeout_ms: u64,
) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let mut child = std::process::Command::new(prog)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("{prog} not found: screenshots need it on PATH ({e})"))?;
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_millis(timeout_ms.max(1)))
        .unwrap_or_else(|| std::time::Instant::now() + std::time::Duration::from_secs(3600));
    loop {
        match child.try_wait() {
            Err(e) => {
                let _ = child.kill();
                return Err(format!("{prog} wait failed: {e}"));
            }
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(format!("{prog} failed"));
                }
                let mut out = Vec::new();
                if let Some(stdout) = child.stdout.as_mut() {
                    // Listings are small; anything past 8 MiB is a
                    // runaway, not a window list.
                    if stdout.take(8 * 1024 * 1024 + 1).read_to_end(&mut out).is_err() {
                        return Err(format!("{prog} output unreadable"));
                    }
                    if out.len() > 8 * 1024 * 1024 {
                        return Err(format!("{prog} output too large"));
                    }
                }
                return Ok(out);
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{prog} timed out"));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}

/// Nearest-neighbor downscale of RGBA8 to fit the raster budget,
/// preserving aspect via a single uniform factor.
pub fn downscale_to_budget(
    rgba: &[u8],
    width: u32,
    height: u32,
) -> (Vec<u8>, u32, u32) {
    let pixels = width.max(1) as u64 * height.max(1) as u64;
    if pixels <= crate::visual::MAX_RASTER_PIXELS {
        return (rgba.to_vec(), width, height);
    }
    // One uniform factor keeps the aspect; flooring keeps the
    // product under budget.
    let factor =
        (crate::visual::MAX_RASTER_PIXELS as f64 / pixels as f64).sqrt();
    let sw = ((width as f64 * factor) as u32).max(1);
    let sh = ((height as f64 * factor) as u32).max(1);
    let stride = width.max(1) as usize * 4;
    let mut small = vec![0u8; sw as usize * sh as usize * 4];
    for y in 0..sh as usize {
        let sy = (y as u64 * height as u64 / sh as u64) as usize;
        for x in 0..sw as usize {
            let sx = (x as u64 * width as u64 / sw as u64) as usize;
            let src = sy * stride + sx * 4;
            let dst = (y * sw as usize + x) * 4;
            if let (Some(s), Some(d)) = (rgba.get(src..src + 4), small.get_mut(dst..dst + 4)) {
                d.copy_from_slice(s);
            }
        }
    }
    (small, sw, sh)
}

/// Capture one named app window: match, check visibility, capture
/// to a 0600 scratch PNG, downscale past the pixel budget.
/// `focus` brings a hidden window forward first (focus steal);
/// without it a hidden window is an error, never a surprise focus.
pub fn capture(app: &str, focus: bool) -> Result<Shot, String> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let raw = run_bounded("hyprctl", &["clients", "-j"], LIST_TIMEOUT_MS)
        .map_err(|e| format!("window list failed: {e}"))?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let window = match_window(&parse_clients(&text)?, app)?;
    if window.w <= 0 || window.h <= 0 {
        return Err(format!("\"{app}\" has no capturable size"));
    }
    let raw = run_bounded("hyprctl", &["activeworkspace", "-j"], LIST_TIMEOUT_MS)
        .map_err(|e| format!("workspace check failed: {e}"))?;
    let here = active_workspace(&String::from_utf8_lossy(&raw))?;
    if window.workspace != here {
        if !focus {
            return Err(format!(
                "\"{}\" is on workspace {}: pass focus:true to bring it forward",
                window.title, window.workspace,
            ));
        }
        run_bounded(
            "hyprctl",
            &["dispatch", "focuswindow", &format!("address:{}", window.address)],
            LIST_TIMEOUT_MS,
        )
        .map_err(|e| format!("focus failed: {e}"))?;
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    // 0600 placeholder first: grim truncates in place, keeping the
    // mode, so captures never land world-readable.
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("forge-shot-{n}-{}.png", std::process::id()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("scratch file failed: {e}"))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, []).map_err(|e| format!("scratch file failed: {e}"))?;
    }
    // grim takes the output path positionally and truncates the
    // placeholder in place, keeping its 0600 mode.
    let geometry = grim_geometry(&window);
    capture_into(&path, &geometry)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("capture unreadable: {e}"))?;
    if bytes.len() > 64 * 1024 * 1024 {
        let _ = std::fs::remove_file(&path);
        return Err("capture too large".to_string());
    }
    let (width, height) = crate::visual::png_dimensions(&bytes)?;
    let (final_w, final_h) = if width as u64 * height as u64 > crate::visual::MAX_RASTER_PIXELS {
        let (rgba, dw, dh) = crate::visual::decode_rgba(&bytes)?;
        if dw != width || dh != height {
            let _ = std::fs::remove_file(&path);
            return Err("decoded dimensions disagree with IHDR".to_string());
        }
        let (small, sw, sh) = downscale_to_budget(&rgba, width, height);
        let png = crate::visual::encode_png(&small, sw, sh)?;
        std::fs::write(&path, &png).map_err(|e| format!("capture rewrite failed: {e}"))?;
        (sw, sh)
    } else {
        (width, height)
    };
    Ok(Shot {
        path: path.to_string_lossy().into_owned(),
        width: final_w,
        height: final_h,
        class: window.class,
        title: window.title,
    })
}

/// Run grim writing straight to the placeholder path.
fn capture_into(path: &std::path::Path, geometry: &str) -> Result<(), String> {
    let path = path.to_string_lossy().into_owned();
    run_bounded("grim", &["-g", geometry, &path], CAPTURE_TIMEOUT_MS)
        .map_err(|e| format!("capture failed: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(class: &str, title: &str) -> Window {
        Window {
            address: "0x1".to_string(),
            class: class.to_string(),
            title: title.to_string(),
            workspace: "1".to_string(),
            x: 12,
            y: 38,
            w: 1776,
            h: 1075,
        }
    }

    #[test]
    fn parse_clients_reads_hyprland_shape() {
        // Real `hyprctl clients -j` shape, trimmed to the fields we
        // use: extra keys must never break the parse.
        let json = r#"[{"address":"0xabc","mapped":true,"hidden":false,"at":[12,38],"size":[1776,1075],"workspace":{"id":1,"name":"1"},"floating":false,"monitor":0,"class":"brave-browser","title":"Inbox - Mail","initialClass":"","initialTitle":"","pid":5354,"xwayland":false,"pinned":false,"fullscreen":0,"fullscreenClient":0,"grouped":[],"tags":[],"swallowing":null,"focusHistoryID":0}]"#;
        let windows = parse_clients(json).expect("parses");
        assert_eq!(windows.len(), 1);
        let w = &windows[0];
        assert_eq!((w.class.as_str(), w.title.as_str()), ("brave-browser", "Inbox - Mail"));
        assert_eq!((w.x, w.y, w.w, w.h), (12, 38, 1776, 1075));
        assert_eq!(w.workspace.as_str(), "1");
        assert!(parse_clients("not json").is_err(), "garbage errors");
        assert!(parse_clients("[]").expect("empty").is_empty(), "no windows");
    }

    #[test]
    fn match_window_prefers_class_then_title() {
        let windows = vec![
            win("com.mitchellh.ghostty", "cargo run"),
            win("brave-browser", "Inbox - Mail"),
        ];
        assert_eq!(match_window(&windows, "ghostty").expect("class").title, "cargo run");
        assert_eq!(match_window(&windows, "GHOSTTY").expect("case").class, "com.mitchellh.ghostty");
        assert_eq!(match_window(&windows, "inbox").expect("title").class, "brave-browser");
        // One phrase matches whole: a multi-word needle that spans
        // class and title matches nothing, while a title word narrows.
        let two = vec![win("brave-browser", "Inbox"), win("brave-browser", "Calendar")];
        assert!(match_window(&two, "brave calendar").is_err(), "whole-phrase miss");
        assert_eq!(match_window(&two, "calendar").expect("title narrows").title, "Calendar");
    }

    #[test]
    fn match_window_reports_misses_and_crowds() {
        let windows = vec![win("brave-browser", "Inbox")];
        let miss = match_window(&windows, "firefox").expect_err("no hit");
        assert!(miss.contains("firefox"), "names the miss: {miss}");
        let empty = match_window(&windows, "   ").expect_err("blank");
        assert!(empty.contains("app name"), "blank: {empty}");
        let two = vec![win("brave-browser", "Inbox"), win("brave-browser", "Calendar")];
        let crowd = match_window(&two, "brave").expect_err("crowd");
        assert!(crowd.contains("Inbox") && crowd.contains("Calendar"), "lists: {crowd}");
    }

    #[test]
    fn grim_geometry_formats_layout_box() {
        assert_eq!(grim_geometry(&win("a", "b")), "12,38 1776x1075");
    }

    #[test]
    fn active_workspace_reads_name() {
        assert_eq!(
            active_workspace(r#"{"id":2,"name":"code"}"#).expect("name"),
            "code"
        );
        assert!(active_workspace("nope").is_err(), "garbage errors");
    }

    #[test]
    fn downscale_fits_pixel_budget_keeping_shape() {
        // 12 MP of flat red must land under the budget with the same
        // aspect and decodable bytes.
        let (w, h) = (4000u32, 3000u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for px in rgba.chunks_exact_mut(4) {
            px[0] = 255;
            px[3] = 255;
        }
        let (small, sw, sh) = downscale_to_budget(&rgba, w, h);
        assert!(sw as u64 * sh as u64 <= crate::visual::MAX_RASTER_PIXELS, "{sw}x{sh}");
        assert_eq!((sw, sh), (2364, 1773), "single uniform factor");
        assert!(!small.is_empty(), "bytes out");
        assert_eq!(small[0], 255, "flat red survives");
        // Under budget passes through untouched.
        let (same, uw, uh) = downscale_to_budget(&rgba[..8 * 4], 8, 1);
        assert_eq!((uw, uh), (8, 1));
        assert_eq!(same.len(), 8 * 4);
    }

    /// Live compositor round trip: only runs with
    /// `FORGE_TEST_HYPRLAND=1` on a Hyprland box. Captures whatever
    /// window sits on the active workspace through the real `capture`
    /// path: match, visibility gate, grim bytes, 0600 file, pixel
    /// budget, decodable PNG.
    #[test]
    fn live_capture_round_trips_on_hyprland() {
        if std::env::var("FORGE_TEST_HYPRLAND").is_err() {
            return;
        }
        let raw = run_bounded("hyprctl", &["clients", "-j"], LIST_TIMEOUT_MS).expect("list");
        let windows = parse_clients(&String::from_utf8_lossy(&raw)).expect("parse");
        let raw = run_bounded("hyprctl", &["activeworkspace", "-j"], LIST_TIMEOUT_MS).expect("ws");
        let here = active_workspace(&String::from_utf8_lossy(&raw)).expect("active");
        let target = windows
            .iter()
            .find(|w| w.workspace == here && w.w > 0 && w.h > 0)
            .expect("a visible window");
        let shot = capture(&target.class, false)
            .or_else(|_| capture(&target.title, false))
            .expect("capture");
        assert!(
            shot.width as u64 * shot.height as u64 <= crate::visual::MAX_RASTER_PIXELS,
            "budget: {}x{}",
            shot.width,
            shot.height
        );
        let bytes = std::fs::read(&shot.path).expect("bytes");
        let (rgba, dw, dh) = crate::visual::decode_rgba(&bytes).expect("decodes");
        assert_eq!((dw, dh), (shot.width, shot.height), "dims agree");
        assert!(!rgba.is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&shot.path).expect("stat").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "private capture");
        }
        let miss = capture("forge-no-such-app-xyz", false).expect_err("miss");
        assert!(miss.contains("no window matching"), "miss: {miss}");
        std::fs::remove_file(&shot.path).ok();
    }

    #[test]
    fn run_bounded_kills_a_hang() {
        let err = run_bounded("sleep", &["30"], 200).expect_err("hang dies");
        assert!(err.contains("timed out"), "err: {err}");
        let out = run_bounded("echo", &["hi"], 5_000).expect("quick runs");
        assert_eq!(out, b"hi\n");
        let missing = run_bounded("forge-no-such-binary-xyz", &[], 5_000).expect_err("missing");
        assert!(missing.contains("not found") || missing.contains("failed"), "err: {missing}");
    }
}
