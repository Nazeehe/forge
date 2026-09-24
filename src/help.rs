//! HTML help manual: embedded in the binary with `include_str!` and
//! extracted to `/tmp/forge-help.html` when the operator asks for help
//! (`Ctrl-b h`), then opened in the default browser. The file is
//! rewritten on every open so it never goes stale.

/// The full manual text, baked into the binary.
pub const HTML: &str = include_str!("help.html");

/// Where the manual is extracted for the browser.
pub fn path() -> std::path::PathBuf {
    std::env::temp_dir().join("forge-help.html")
}

/// Write the embedded manual to [`path`], replacing any older copy.
pub fn extract() -> std::io::Result<std::path::PathBuf> {
    let dest = path();
    crate::fs_atomic::write_atomic(&dest, HTML.as_bytes())?;
    Ok(dest)
}

/// Extract the manual and open it in the default browser. Returns the
/// extracted path. Best effort: a missing browser errors here and the
/// caller warns instead of failing.
pub fn open() -> std::io::Result<std::path::PathBuf> {
    let dest = extract()?;
    std::process::Command::new("xdg-open")
        .arg(&dest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every documented feature keeps its section: drift between the
    /// app and the manual fails here, not in a user's browser.
    #[test]
    fn manual_covers_every_feature() {
        for section in [
            "id=\"keys\"",
            "id=\"sessions\"",
            "id=\"groups\"",
            "id=\"permissions\"",
            "id=\"telegram\"",
            "id=\"themes\"",
            "id=\"restore\"",
            "id=\"mouse\"",
            "id=\"sidebar\"",
            "id=\"cli\"",
            "id=\"files\"",
        ] {
            assert!(HTML.contains(section), "manual lost section {section}");
        }
    }

    #[test]
    fn manual_has_dark_light_toggle() {
        assert!(HTML.contains("id=\"theme-toggle\""), "toggle button present");
        assert!(HTML.contains("prefers-color-scheme"), "respects OS theme");
        assert!(HTML.contains("data-theme"), "theme switch mechanism");
    }

    #[test]
    fn extract_writes_the_manual_to_tmp() {
        let dest = extract().expect("extract writes");
        assert_eq!(dest, path());
        let text = std::fs::read_to_string(&dest).expect("manual readable");
        assert!(text.contains("Forge"), "manual body: {text:?}");
        assert_eq!(text, HTML, "extracted copy matches the embed");
    }
}
