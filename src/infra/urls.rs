//! Clickable URLs in PTY panes: detection and browser open.
//!
//! Agent harnesses print plain `http(s)://` URLs (and some emit OSC 8
//! hyperlinks, which the vendored vt100 parser drops). Forge enables
//! mouse capture, so the outer terminal never sees the click that would
//! have opened the link — Forge itself must hit-test the click against
//! the visible row text and open the browser.

/// Longest URL the opener accepts: spawn args stay bounded no matter
/// what a harness prints.
pub const MAX_URL_LEN: usize = 2048;

/// The URL under 0-based char index `col` in `row`, if any. Only
/// `http://` and `https://` count; a match must cover `col`. Trailing
/// prose punctuation (`.,;:!?'"` and closing brackets) stays out, so
/// `(see https://example.com/x).` yields the bare link.
pub fn find_url_at(row: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = row.chars().collect();
    if col >= chars.len() {
        return None;
    }
    for (start, end) in url_spans(&chars) {
        if col >= start && col < end {
            return Some(chars[start..end].iter().collect());
        }
    }
    None
}

/// Character ranges of every openable URL in `chars`.
fn url_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let Some(scheme_len) = scheme_len_at(chars, i) else {
            i += 1;
            continue;
        };
        let mut end = i + scheme_len;
        while end < chars.len() && is_url_char(chars[end]) {
            end += 1;
        }
        while end > i + scheme_len && is_trailing_punct(chars[end - 1]) {
            end -= 1;
        }
        if end > i + scheme_len {
            out.push((i, end));
        }
        i = end.max(i + 1);
    }
    out
}

/// Length of the `http(s)://` scheme opening at `i`, if any.
fn scheme_len_at(chars: &[char], i: usize) -> Option<usize> {
    let tail = &chars[i..];
    let starts_with = |scheme: &str| {
        scheme.chars().count() <= tail.len()
            && tail
                .iter()
                .zip(scheme.chars())
                .all(|(a, b)| *a == b)
    };
    if starts_with("https://") {
        Some("https://".len())
    } else if starts_with("http://") {
        Some("http://".len())
    } else {
        None
    }
}

/// True for characters a URL can continue through. Whitespace, controls,
/// quotes, angle brackets, and backticks end the link; everything else
/// (including parens, which the trailing trim handles) stays in.
fn is_url_char(c: char) -> bool {
    !(c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '`'))
}

/// Trailing characters that belong to prose, not the link.
fn is_trailing_punct(c: char) -> bool {
    matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | ')' | ']' | '}')
}

/// True when `url` is safe to hand to the desktop opener: `http(s)://`
/// with a non-empty rest, no whitespace/controls/quotes/angle brackets,
/// and within [`MAX_URL_LEN`] bytes. Anything else (`file:`,
/// `javascript:`, `data:`, ...) stays out.
pub fn is_openable_url(url: &str) -> bool {
    if url.is_empty() || url.len() > MAX_URL_LEN {
        return false;
    }
    let rest = match url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        Some(rest) => rest,
        None => return false,
    };
    if rest.is_empty() {
        return false;
    }
    !url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '`'))
}

/// Open `url` in the desktop browser. Best effort like
/// [`crate::ui::help::open`]: a missing browser errors and the caller
/// warns instead of failing. Non-openable URLs are refused, never
/// passed to the opener.
pub fn open_url(url: &str) -> std::io::Result<()> {
    if !is_openable_url(url) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to open non-http(s) URL",
        ));
    }
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(target_os = "macos"))]
    let program = "xdg-open";
    std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_inside_url_returns_it() {
        let row = "see https://example.com/docs for details";
        let col = row.find("example").unwrap();
        assert_eq!(
            find_url_at(row, col),
            Some("https://example.com/docs".to_string()),
            "click inside the URL must hit: {row:?}"
        );
    }

    #[test]
    fn click_outside_url_returns_none() {
        let row = "see https://example.com/docs for details";
        assert_eq!(find_url_at(row, 0), None, "click on plain text: {row:?}");
        let col = row.find("for details").unwrap();
        assert_eq!(find_url_at(row, col), None, "click past the URL: {row:?}");
    }

    #[test]
    fn trailing_punctuation_is_stripped() {
        let row = "(see https://example.com/docs).";
        let col = row.find("example").unwrap();
        assert_eq!(
            find_url_at(row, col),
            Some("https://example.com/docs".to_string()),
            "wrapping parens and trailing dot stay out: {row:?}"
        );
    }

    #[test]
    fn multiple_urls_pick_the_cursor_one() {
        let row = "a https://first.example/x and https://second.example/y end";
        let first = row.find("first").unwrap();
        let second = row.find("second").unwrap();
        assert_eq!(
            find_url_at(row, first),
            Some("https://first.example/x".to_string())
        );
        assert_eq!(
            find_url_at(row, second),
            Some("https://second.example/y".to_string())
        );
    }

    #[test]
    fn col_past_line_end_returns_none() {
        assert_eq!(find_url_at("https://example.com", 999), None);
        assert_eq!(find_url_at("", 0), None);
    }

    #[test]
    fn only_http_and_https_are_openable() {
        assert!(is_openable_url("https://example.com/docs"));
        assert!(is_openable_url("http://localhost:8080/x"));
        for bad in [
            "",
            "ftp://example.com/x",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/plain,hi",
            "https://example.com/has space",
            "https://",
            "http://",
        ] {
            assert!(!is_openable_url(bad), "must refuse {bad:?}");
        }
    }

    #[test]
    fn absurdly_long_urls_are_refused() {
        let long = format!("https://example.com/{}", "a".repeat(8192));
        assert!(!is_openable_url(&long), "unbounded spawn args stay out");
    }
}
