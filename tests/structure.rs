//! Source-structure guard for the `src/` restructure.
//!
//! A pure move adds no behavior, so the failing test here is a structure
//! test whose allow-lists shrink as the restructure proceeds. Each step
//! starts red by deleting that step's entries, then goes green after
//! the move.

use std::fs;
use std::path::PathBuf;

const MAX_FILE_LINES: usize = 1500;

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn flat_rs_files() -> Vec<String> {
    let mut out = Vec::new();
    for entry in fs::read_dir(src_dir()).expect("read src/") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().map(|e| e == "rs").unwrap_or(false) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name != "main.rs" {
                out.push(name);
            }
        }
    }
    out.sort();
    out
}

fn all_rs_files() -> Vec<(String, usize)> {
    fn visit(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, usize)>) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                visit(&path, root, out);
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let content = fs::read_to_string(&path).expect("read rs file");
                let lines = content.lines().count();
                out.push((rel, lines));
            }
        }
    }
    let root = src_dir();
    let mut out = Vec::new();
    visit(&root, &root, &mut out);
    out.sort();
    out
}

#[test]
fn flat_src_is_only_main() {
    let unexpected = flat_rs_files();
    assert!(
        unexpected.is_empty(),
        "flat src/*.rs files must move into feature folders: {unexpected:?}"
    );
}

#[test]
fn no_file_exceeds_1500_lines() {
    let oversize: Vec<String> = all_rs_files()
        .into_iter()
        .filter(|(_, lines)| *lines > MAX_FILE_LINES)
        .map(|(rel, lines)| format!("{rel} ({lines} lines)"))
        .collect();
    assert!(
        oversize.is_empty(),
        "files over {MAX_FILE_LINES} lines must split: {oversize:?}"
    );
}

/// Agent names that must only appear as string literals inside `src/agents/`.
///
/// Core code never matches on agent name strings: adding an agent is one
/// `agents.json` entry plus one adapter file. Any `"claude"` / `"codex"` /
/// `"muse"` / `"agy"` literal outside `src/agents/` is a leak, unless it is
/// listed in `ALLOWLIST` with a written reason.
const AGENT_NAMES: &[&str] = &["claude", "codex", "muse", "agy"];

/// (file substring, literal substring, reason). Empty: no leaks are allowed
/// outside `src/agents/`. Add entries only with a written reason.
const ALLOWLIST: &[(&str, &str, &str)] = &[];

fn is_skipped_file(rel: &str) -> bool {
    if rel.starts_with("agents/") {
        return true;
    }
    let file = rel.rsplit('/').next().unwrap_or(rel);
    if file == "tests.rs" || file == "test_support.rs" {
        return true;
    }
    false
}

/// Remove `#[cfg(test)] mod ... { ... }` blocks so unit-test fixtures using
/// agent names do not count as production leaks.
fn strip_test_modules(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let tag = "#[cfg(test)]";
    while i < bytes.len() {
        // `i` is always a char boundary here.
        if src[i..].starts_with(tag) {
            let mut j = i + tag.len();
            while j < bytes.len() {
                let c = src[j..].chars().next().unwrap();
                if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
                    j += c.len_utf8();
                } else {
                    break;
                }
            }
            if src[j..].starts_with("mod") {
                let is_mod_boundary = src[j + 3..]
                    .chars()
                    .next()
                    .map(|c| !c.is_ascii_alphanumeric() && c != '_')
                    .unwrap_or(true);
                if is_mod_boundary {
                    // Find the opening brace of the module.
                    let mut k = j + 3;
                    while k < bytes.len() {
                        let c = src[k..].chars().next().unwrap();
                        if c == '{' {
                            break;
                        }
                        k += c.len_utf8();
                    }
                    if k >= bytes.len() {
                        break;
                    }
                    // Skip balanced braces, string/char/comment aware enough
                    // for test-module stripping: braces inside strings do not
                    // count.
                    let mut depth = 0;
                    let mut kk = k;
                    let mut in_str = false;
                    let mut in_char = false;
                    let mut escaped = false;
                    while kk < bytes.len() {
                        let c = src[kk..].chars().next().unwrap();
                        let clen = c.len_utf8();
                        if in_str {
                            if escaped {
                                escaped = false;
                            } else if c == '\\' {
                                escaped = true;
                            } else if c == '"' {
                                in_str = false;
                            }
                            kk += clen;
                            continue;
                        }
                        if in_char {
                            if escaped {
                                escaped = false;
                            } else if c == '\\' {
                                escaped = true;
                            } else if c == '\'' {
                                in_char = false;
                            }
                            kk += clen;
                            continue;
                        }
                        if c == '"' {
                            in_str = true;
                            kk += clen;
                            continue;
                        }
                        if c == '\'' {
                            in_char = true;
                            kk += clen;
                            continue;
                        }
                        if c == '{' {
                            depth += 1;
                        } else if c == '}' {
                            depth -= 1;
                            if depth == 0 {
                                kk += clen;
                                break;
                            }
                        }
                        kk += clen;
                    }
                    i = kk;
                    continue;
                }
            }
            out.push_str(tag);
            i += tag.len();
            continue;
        }
        let c = src[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Collect `(line_number, literal_content)` for normal `"..."` and raw
/// `r#"..."#` string literals, skipping comments and char literals.
fn string_literals(src: &str) -> Vec<(usize, String)> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1usize;
    // Count newlines in `bytes[..upto]` without slicing `src` (which would
    // panic on non-char boundaries).
    let newlines_before = |upto: usize| {
        bytes[..upto.min(bytes.len())]
            .iter()
            .filter(|b| **b == b'\n')
            .count()
            + 1
    };
    let advance_line = |from: usize, to: usize, line: &mut usize| {
        for b in &bytes[from..to.min(bytes.len())] {
            if *b == b'\n' {
                *line += 1;
            }
        }
    };
    while i < bytes.len() {
        // Line comment.
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Block comment.
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let start = i;
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            advance_line(start, i, &mut line);
            continue;
        }
        // Raw string: r#*"...".
        if bytes[i] == b'r'
            && (bytes.get(i + 1) == Some(&b'"') || bytes.get(i + 1) == Some(&b'#'))
        {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while bytes.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            if bytes.get(j) == Some(&b'"') {
                let start_line = newlines_before(i);
                j += 1;
                let content_start = j;
                let mut end = None;
                let mut k_scan = j;
                while k_scan < bytes.len() {
                    if bytes[k_scan] == b'"' {
                        let mut k = k_scan + 1;
                        let mut ok = true;
                        for _ in 0..hashes {
                            if bytes.get(k) != Some(&b'#') {
                                ok = false;
                                break;
                            }
                            k += 1;
                        }
                        if ok {
                            end = Some((k_scan, k));
                            break;
                        }
                    }
                    k_scan += 1;
                }
                if let Some((content_end, next)) = end {
                    let content =
                        String::from_utf8_lossy(&bytes[content_start..content_end]).into_owned();
                    out.push((start_line, content));
                    advance_line(i, next, &mut line);
                    i = next;
                    continue;
                }
                // Unterminated: stop.
                break;
            }
        }
        // Char literal: skip '...' (no agent names are single chars).
        if bytes[i] == b'\'' {
            let mut j = i + 1;
            if bytes.get(j) == Some(&b'\\') {
                j += 2;
                // Skip one more for \u{...} style? Enough to avoid misparse.
                if bytes.get(j - 2) == Some(&b'u') {
                    while j < bytes.len() && bytes[j] != b'\'' && bytes[j] != b'\n' {
                        j += 1;
                    }
                }
            } else if j < bytes.len() {
                // Advance one char (may be multi-byte).
                let c = src[j..].chars().next().unwrap();
                j += c.len_utf8();
            }
            if bytes.get(j) == Some(&b'\'') {
                advance_line(i, j + 1, &mut line);
                i = j + 1;
                continue;
            }
        }
        // Normal string.
        if bytes[i] == b'"' {
            let start_line = newlines_before(i);
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'\\' && j + 1 < bytes.len() {
                    j += 2;
                    continue;
                }
                if bytes[j] == b'"' {
                    break;
                }
                j += 1;
            }
            let content_end = j.min(bytes.len());
            let content =
                String::from_utf8_lossy(&bytes[i + 1..content_end]).into_owned();
            out.push((start_line, content));
            let next = if j < bytes.len() { j + 1 } else { j };
            advance_line(i, next, &mut line);
            i = next;
            continue;
        }
        if bytes[i] == b'\n' {
            line += 1;
        }
        // Advance one char to stay on char boundaries.
        let c = src[i..].chars().next().unwrap();
        i += c.len_utf8();
    }
    out
}

#[test]
fn no_agent_name_literals_outside_agents() {
    fn visit(
        dir: &std::path::Path,
        root: &std::path::Path,
        violations: &mut Vec<String>,
    ) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                visit(&path, root, violations);
                continue;
            }
            if path.extension().map(|e| e != "rs").unwrap_or(true) {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if is_skipped_file(&rel) {
                continue;
            }
            let content = fs::read_to_string(&path).expect("read rs file");
            let stripped = strip_test_modules(&content);
            for (line_no, lit) in string_literals(&stripped) {
                for agent in AGENT_NAMES {
                    if lit.contains(agent) {
                        let allowed = ALLOWLIST.iter().any(|(f, l, _)| {
                            rel.contains(f) && lit.contains(l)
                        });
                        if !allowed {
                            // One entry per file:line:agent; a line with two
                            // literals (e.g. `"codex" => ... ".codex/..."`)
                            // reports once.
                            violations.push(format!("{rel}:{line_no}: {agent}"));
                        }
                    }
                }
            }
        }
    }

    let root = src_dir();
    let mut violations = Vec::new();
    visit(&root, &root, &mut violations);
    violations.sort();
    violations.dedup();
    assert!(
        violations.is_empty(),
        "agent name literals outside src/agents/ (add adapter or allowlist with reason):\n{}",
        violations.join("\n"),
    );
}
