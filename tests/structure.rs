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
