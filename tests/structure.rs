//! Source-structure guard for the `src/` restructure.
//!
//! A pure move adds no behavior, so the failing test here is a structure
//! test whose allow-lists shrink as the restructure proceeds. Each step
//! starts red by deleting that step's entries, then goes green after
//! the move.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

const MAX_FILE_LINES: usize = 1500;

/// Flat `src/*.rs` files (besides `main.rs`) still allowed at the top level.
/// Emptied as feature folders take their files.
const FLAT_ALLOWED: &[&str] = &[
    "app.rs",
    "board.rs",
    "bot.rs",
    "card_edit.rs",
    "comms.rs",
    "create.rs",
    "groups.rs",
    "help.rs",
    "input.rs",
    "listener.rs",
    "mcp.rs",
    "oobe.rs",
    "quit.rs",
    "screenshot.rs",
    "telegram.rs",
    "telegram_dialog.rs",
    "tetris.rs",
    "theme.rs",
    "theme_dialog.rs",
    "tui.rs",
    "ui.rs",
    "visual.rs",
    "walkthrough.rs",
    "whichkey.rs",
];

/// `.rs` files under `src/` (paths relative to `src/`) allowed to exceed
/// [`MAX_FILE_LINES`]. Emptied as the oversized files are split.
const OVERSIZE_ALLOWED: &[&str] = &["app.rs", "comms.rs", "hooks/install.rs", "tui.rs", "ui.rs"];

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
    let allowed: BTreeSet<&str> = FLAT_ALLOWED.iter().copied().collect();
    let mut unexpected = Vec::new();
    for name in flat_rs_files() {
        if !allowed.contains(name.as_str()) {
            unexpected.push(name);
        }
    }
    assert!(
        unexpected.is_empty(),
        "flat src/*.rs files not in FLAT_ALLOWED (move them or allow-list them): {unexpected:?}"
    );
}

#[test]
fn no_file_exceeds_1500_lines() {
    let allowed: BTreeSet<&str> = OVERSIZE_ALLOWED.iter().copied().collect();
    let mut oversize = Vec::new();
    for (rel, lines) in all_rs_files() {
        if lines > MAX_FILE_LINES && !allowed.contains(rel.as_str()) {
            oversize.push(format!("{rel} ({lines} lines)"));
        }
    }
    assert!(
        oversize.is_empty(),
        "files over {MAX_FILE_LINES} lines not in OVERSIZE_ALLOWED: {oversize:?}"
    );
}

#[test]
fn allow_lists_have_no_stale_entries() {
    let root = src_dir();
    let mut stale = Vec::new();
    for entry in FLAT_ALLOWED {
        if !root.join(entry).is_file() {
            stale.push(format!("FLAT_ALLOWED: {entry}"));
        }
    }
    for entry in OVERSIZE_ALLOWED {
        if !root.join(entry).is_file() {
            stale.push(format!("OVERSIZE_ALLOWED: {entry}"));
        }
    }
    assert!(stale.is_empty(), "stale allow-list entries: {stale:?}");
}
