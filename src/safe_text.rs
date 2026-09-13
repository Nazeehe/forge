//! Security-grade display encoding for untrusted text.
//!
//! Raw input always remains the policy/execution input; only what is SHOWN in
//! the TUI goes through [`encode_for_display`]. Controls, bidi overrides and
//! invisibles become visible so hostile text cannot spoof audit or approval UI.

/// Encode untrusted text for display.
///
/// - C0 controls → U+2400 block symbols (`␀`..`␟`); DEL → `␡`.
/// - C1 controls, bidi controls and invisible format chars → `\u{HHHH}`.
/// - Everything else (including emoji/CJK) passes through untouched.
pub fn encode_for_display(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' => out.push('⏎'),
            '\t' => out.push('⇥'),
            '\x00'..='\x1F' => out.push(char::from_u32(0x2400 + c as u32).unwrap_or('�')),
            '\x7F' => out.push('␡'),
            c if is_c1(c) || is_bidi_control(c) || is_invisible(c) => {
                out.push_str(&format!("\\u{{{:04X}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Multiline variant of [`encode_for_display`]: preserves `\n` row breaks
/// (e.g. terminal screen contents, where the parser already consumed escape
/// sequences) while encoding every other control identically. Splitting on
/// `\n` first means a newline can never smuggle encoded content.
pub fn encode_multiline_for_display(s: &str) -> String {
    s.split('\n')
        .map(encode_for_display)
        .collect::<Vec<_>>()
        .join("\n")
}

/// True if `s` contains Unicode bidi control characters.
pub fn contains_bidi_controls(s: &str) -> bool {
    s.chars().any(is_bidi_control)
}

/// True if `s` contains invisible format characters (zero-width, BOM, ...).
pub fn contains_invisibles(s: &str) -> bool {
    s.chars().any(is_invisible)
}

/// Heuristic spoof flag: Latin mixed with Cyrillic or Greek (classic
/// lookalike-attack shape). Pure single-script text is never suspicious.
pub fn looks_suspicious(s: &str) -> bool {
    let latin = s.chars().any(|c| c.is_ascii_alphabetic());
    let cyrillic = s.chars().any(|c| ('\u{400}'..='\u{45F}').contains(&c));
    let greek = s.chars().any(|c| ('\u{370}'..='\u{3FF}').contains(&c));
    latin && (cyrillic || greek)
}

fn is_c1(c: char) -> bool {
    ('\u{80}'..='\u{9F}').contains(&c)
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')
}

fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{00AD}' | '\u{2060}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_keeps_row_breaks_but_encodes_the_rest() {
        assert_eq!(
            encode_multiline_for_display("l1\nl2\x07"),
            "l1\nl2␇"
        );
    }

    #[test]
    fn controls_become_visible() {
        assert_eq!(encode_for_display("a\x00b\x07c"), "a␀b␇c");
        assert_eq!(encode_for_display("x\x7fy"), "x␡y");
        assert_eq!(encode_for_display("l1\nl2\ttab"), "l1⏎l2⇥tab");
    }

    #[test]
    fn bidi_and_invisibles_exposed() {
        assert!(contains_bidi_controls("a\u{202E}b"));
        assert!(!contains_bidi_controls("plain"));
        assert!(contains_invisibles("a\u{200B}b"));
        assert!(contains_invisibles("\u{FEFF}bom"));
        assert!(!contains_invisibles("plain"));
        assert_eq!(encode_for_display("a\u{202E}b"), "a\\u{202E}b");
        assert_eq!(encode_for_display("a\u{200B}b"), "a\\u{200B}b");
        assert_eq!(encode_for_display("\u{85}nel"), "\\u{0085}nel");
    }

    #[test]
    fn normal_text_untouched() {
        let s = "héllo 世界 🎉 — fine";
        assert_eq!(encode_for_display(s), s);
        assert!(!looks_suspicious(s));
    }

    #[test]
    fn mixed_script_lookalikes_flagged() {
        assert!(!looks_suspicious("paypal"));
        assert!(!looks_suspicious("привет")); // pure Cyrillic: fine
        assert!(looks_suspicious("p\u{430}ypal")); // Cyrillic а
        assert!(looks_suspicious("\u{3B1}bc")); // Greek α + Latin
    }
}
