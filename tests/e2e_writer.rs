//! Writer E2E smoke: the real `forge` binary in a portable-pty with
//! an isolated temp HOME, driven by keys plus SGR mouse bytes, with
//! assertions on the vt100-parsed screen. Every later E task adds its
//! scenario here.
//!
//! The fake agent is `bash -c "exec sleep 300"` through a scratch
//! agents.json (resume.without_id non-empty per the registry
//! schema); `exec cat` would also work, but sleep survives stray
//! stdin closes while the harness owns the pty.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

const COLS: u16 = 120;
const ROWS: u16 = 30;
const STEP_TIMEOUT: Duration = Duration::from_secs(15);

struct Harness {
    home: std::path::PathBuf,
    cwd: std::path::PathBuf,
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    rx: std::sync::mpsc::Receiver<Option<Vec<u8>>>,
    parser: vt100::Parser,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Raw output bytes (capped): escape sequences the vt100
    /// parser swallows (OSC 52) stay observable here.
    raw: Vec<u8>,
}

/// Raw byte log cap: recent frames stay observable, memory stays flat.
const RAW_CAP: usize = 65536;

/// Boot forge with an isolated HOME (scratch agents.json, seeded
/// files) and dismiss anything the first frame shows.
fn boot(files: &[(&str, &str)]) -> Harness {
    let tag = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs(), d.subsec_nanos()))
        .unwrap_or((0, 0));
    let home = std::env::temp_dir().join(format!("forge-e2e-home-{}-{}", tag.0, tag.1));
    let cwd = std::env::temp_dir().join(format!("forge-e2e-cwd-{}-{}", tag.0, tag.1));
    std::fs::create_dir_all(home.join(".forge")).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(
        home.join(".forge/agents.json"),
        r#"{
  "version": 1,
  "agents": [
    {
      "name": "fake",
      "binary": "bash",
      "env_override": "FORGE_E2E_BASH",
      "model_flag": "--model",
      "default_model": "",
      "extra_args": ["-c", "exec sleep 300"],
      "resume": {
        "subcommand": null,
        "with_id": { "flag": "--resume" },
        "without_id": ["--new"]
      },
      "supports_hooks": false,
      "session_attribution": "cwd_window"
    }
  ]
}"#,
    )
    .unwrap();
    for (name, text) in files {
        std::fs::write(cwd.join(name), text).unwrap();
    }
    let pty = portable_pty::native_pty_system();
    let pair = pty
        .openpty(portable_pty::PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = portable_pty::CommandBuilder::new(env!("CARGO_BIN_EXE_forge"));
    cmd.cwd(&cwd);
    cmd.env("HOME", &home);
    cmd.env("TERM", "xterm-256color");
    let child = pair.slave.spawn_command(cmd).unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = pair.master.take_writer().unwrap();
    let master = pair.master;
    // Blocking pty reads would wedge STEP_TIMEOUT while the child
    // idles, so a reader thread forwards bytes; the main thread only
    // ever recv_timeouts, and a dead child fails the wait instead of
    // hanging it.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("e2e-reader".to_string())
        .spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        if tx.send(Some(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    _ => {
                        let _ = tx.send(None);
                        break;
                    }
                }
            }
        })
        .unwrap();
    let mut harness = Harness {
        home,
        cwd,
        master,
        writer,
        rx,
        parser: vt100::Parser::new(ROWS, COLS, 0),
        _child: child,
        raw: Vec::new(),
    };
    // Whatever the first frame shows (setup dialog or not), Esc backs
    // out of it; then the idle screen must settle. The fake agent's
    // name only appears inside the create dialog, asserted there.
    harness.send("\x1b");
    harness.wait_for("Nosessionsyet", "idle screen");
    harness
}

impl Harness {
    fn send(&mut self, bytes: &str) {
        self.writer.write_all(bytes.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    /// Log raw output bytes under the cap.
    fn note_raw(&mut self, bytes: &[u8]) {
        self.raw.extend_from_slice(bytes);
        if self.raw.len() > RAW_CAP {
            let drop = self.raw.len() - RAW_CAP;
            self.raw.drain(..drop);
        }
    }

    /// Drain output until `needle` appears on screen or time out.
    /// Returns the screen text at success for follow-up assertions.
    /// Both sides squash whitespace: the parser drops some blank
    /// cells, so needles never contain spaces.
    fn wait_for(&mut self, needle: &str, what: &str) -> String {
        // Needles are step-specific: each names text only its own
        // transition can produce, so a match is never stale. Steps
        // asserting current state (no transition) use settle instead.
        let end = Instant::now() + STEP_TIMEOUT;
        loop {
            let text = screen_text(&self.parser);
            let squashed: String =
                text.chars().filter(|c| !c.is_whitespace()).collect();
            if squashed.contains(needle) {
                return text;
            }
            if Instant::now() >= end {
                panic!("timed out waiting for {what} ({needle:?}):\n{text}");
            }
            match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Some(bytes)) => {
                    self.note_raw(&bytes);
                    self.parser.process(&bytes);
                }
                Ok(None) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    panic!(
                        "child output ended while waiting for {what} ({needle:?}):\n{}",
                        screen_text(&self.parser)
                    );
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Drain up to a beat of output, then read the screen as-is. For
    /// assertions where the screen may legitimately not change.
    fn settle(&mut self) -> String {
        let end = Instant::now() + Duration::from_millis(500);
        while Instant::now() < end {
            match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Some(bytes)) => {
                    self.note_raw(&bytes);
                    self.parser.process(&bytes);
                }
                Ok(None) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        screen_text(&self.parser)
    }

    /// Whether the raw log contains `needle` (escape sequences).
    fn saw_raw(&self, needle: &str) -> bool {
        let log = String::from_utf8_lossy(&self.raw);
        log.contains(needle)
    }

    /// X10 button-less motion at 0-based cells: plain terminals
    /// report hover as Cb 67 (Moved); Ghostty reports it as Cb 64
    /// (which decodes as Drag(Left)). Neither may select.
    fn motion(&mut self, cb: u8, x: u16, y: u16) {
        let pkt = [0x1b, b'[', b'M', cb, (x + 1) as u8 + 32, (y + 1) as u8 + 32];
        self.writer.write_all(&pkt).unwrap();
        self.writer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(60));
    }

    /// Fast X10 press + release with no sleeps: chains into a
    /// double-click inside the 500 ms window.
    fn press(&mut self, x: u16, y: u16) {
        let press = [0x1b, b'[', b'M', 32, (x + 1) as u8 + 32, (y + 1) as u8 + 32];
        let release = [0x1b, b'[', b'M', 35, (x + 1) as u8 + 32, (y + 1) as u8 + 32];
        self.writer.write_all(&press).unwrap();
        self.writer.write_all(&release).unwrap();
        self.writer.flush().unwrap();
    }

    /// Legacy X10 click (press + release) at 0-based cells: Forge
    /// enables plain mouse capture, not SGR.
    fn click(&mut self, x: u16, y: u16) {
        let press = [0x1b, b'[', b'M', 32, (x + 1) as u8 + 32, (y + 1) as u8 + 32];
        let release = [0x1b, b'[', b'M', 35, (x + 1) as u8 + 32, (y + 1) as u8 + 32];
        self.writer.write_all(&press).unwrap();
        self.writer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        self.writer.write_all(&release).unwrap();
        self.writer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(200));
    }

    /// Owned cell grid plus width: for geometry assertions the
    /// joined text cannot serve (wide glyphs shift char counts).
    fn grid(&mut self) -> (u16, Vec<Vec<String>>) {
        let screen = self.parser.screen();
        let (rows, cols) = (screen.size().0, screen.size().1);
        let mut out = Vec::new();
        for y in 0..rows {
            let mut row = Vec::new();
            for x in 0..cols {
                row.push(
                    screen.cell(y, x).map(|c| c.contents()).unwrap_or_default().to_string(),
                );
            }
            out.push(row);
        }
        (cols, out)
    }

    /// First cell of `needle` on screen, if visible. Cell-based:
    /// wide glyphs make char counts lie about columns.
    fn find(&mut self, needle: &str) -> Option<(u16, u16)> {
        let screen = self.parser.screen();
        let (rows, cols) = (screen.size().0, screen.size().1);
        for y in 0..rows {
            let cells: Vec<String> = (0..cols)
                .map(|x| {
                    screen.cell(y, x).map(|c| c.contents()).unwrap_or_default().to_string()
                })
                .collect();
            for x in 0..cols {
                if cells[x as usize..].concat().starts_with(needle) {
                    return Some((x, y));
                }
            }
        }
        None
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.master
            .resize(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        self.parser = vt100::Parser::new(rows, cols, 0);
        std::thread::sleep(Duration::from_millis(500));
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self._child.kill();
        std::fs::remove_dir_all(&self.home).ok();
        std::fs::remove_dir_all(&self.cwd).ok();
    }
}

/// Full style signature of one screen row: EdTUI paints its Visual
/// selection with the Focus role (yellow bold), so a hover that
/// selects changes this row's signature. Compared before/after.
fn row_signature(parser: &vt100::Parser, y: u16) -> Vec<(String, String, String, bool, bool)> {
    let screen = parser.screen();
    (0..screen.size().1)
        .map(|x| {
            screen
                .cell(y, x)
                .map(|c| {
                    (
                        c.contents(),
                        format!("{:?}", c.fgcolor()),
                        format!("{:?}", c.bgcolor()),
                        c.bold(),
                        c.inverse(),
                    )
                })
                .unwrap_or_default()
        })
        .collect()
}

fn screen_text(parser: &vt100::Parser) -> String {
    let screen = parser.screen();
    let mut out = String::new();
    for y in 0..screen.size().0 {
        for x in 0..screen.size().1 {
            if let Some(cell) = screen.cell(y, x) {
                out.push_str(&cell.contents());
            }
        }
        out.push('\n');
    }
    out
}

/// Create one session through the real create dialog (defaults) and
/// wait until it runs.
fn create_session(h: &mut Harness) {
    h.send("\x02");
    std::thread::sleep(Duration::from_millis(200));
    h.send("c");
    let text = h.wait_for("Create", "create dialog");
    assert!(text.contains("fake"), "scratch registry lists the fake agent");
    std::thread::sleep(Duration::from_millis(300));
    h.send("\r");
    h.wait_for("running", "session running");
}

#[test]
fn writer_opens_types_and_keeps_modal_intact() {
    let mut h = boot(&[("seeded.md", "seed\n")]);
    create_session(&mut h);
    // Ctrl-b d opens Writer through the prefix path.
    h.send("\x02");
    std::thread::sleep(Duration::from_millis(200));
    h.send("d");
    h.wait_for("Markdowneditor", "writer empty state");
    // Open the seeded file through the real prompt.
    h.send("o");
    h.wait_for("Opendocumentin", "open prompt");
    h.send("seeded.md\r");
    // The prompt echoes the name too: wait for the doc-view status.
    let text = h.wait_for("rev0", "doc open");
    assert!(text.contains("seeded.md"), "title names the file");
    // Arrows and typing land in the document. The cursor opens at
    // the doc head: Right moves into the word, End jumps after it.
    h.send("\x1b[C");
    h.send("\x1b[F");
    h.send("!");
    let text = h.wait_for("seed!", "typed bang");
    assert!(text.contains("seed!"), "typing lands after the arrow moves");
    // Tab keeps focus in the editor: the next char lands in the doc.
    // (The hidden panel never paints, so visible text is doc text.)
    h.send("\t");
    h.send("X");
    let text = h.wait_for("seed!X", "tab kept focus");
    assert!(text.contains("seed!X"), "tab does not lose focus");
    // The quit modal paints last over the live editor.
    h.send("\x02");
    std::thread::sleep(Duration::from_millis(200));
    h.send("q");
    let text = h.wait_for("Areyousureyouwanttoquit?", "quit modal");
    assert!(
        text.chars().filter(|c| !c.is_whitespace()).collect::<String>().contains("Areyousureyouwanttoquit?"),
        "modal text intact"
    );
    h.send("\x1b");
    // Dismissal shows no new text: settle, then the modal is gone and
    // the doc is back.
    let text = h.settle();
    assert!(
        !text.chars().filter(|c| !c.is_whitespace()).collect::<String>().contains("Areyousureyouwanttoquit?"),
        "modal dismissed"
    );
    assert!(text.contains("seed!X"), "back in writer after dismiss");
}

#[test]
fn writer_topbar_click_and_narrow_resize_keep_writer() {
    let mut h = boot(&[]);
    create_session(&mut h);
    // Open Writer by clicking its topbar tab through real SGR bytes.
    h.wait_for("Writer", "writer tab in the strip");
    let (x, y) = h.find("Writer").expect("writer tab cell");
    h.click(x, y);
    h.wait_for("Markdowneditor", "writer via topbar click");
    // Narrow resize keeps the Writer view (never evicted) with a
    // reachable tab and the collapsed toolbar.
    h.resize(30, 90);
    let text = h.wait_for("Writer", "writer survives at 90 cols");
    assert!(
        text.contains("Markdowneditor") || text.contains("New"),
        "writer still open narrow"
    );
    let (x, y) = h.find("Writer").expect("writer tab cell when narrow");
    h.click(x, y);
    // Re-selecting the open tab may repaint nothing: settle, then read.
    let text = h.settle();
    assert!(
        text.contains("Markdowneditor") || text.contains("New"),
        "narrow tab click keeps writer"
    );
}

/// Prompt line editing over the wire: type a path with a wrong first
/// char, Home, fix it, Enter opens the right file.
#[test]
fn writer_hover_motion_paints_no_highlight() {
    let mut h = boot(&[("hover.md", "alpha beta gamma\nsecond line\n")]);
    create_session(&mut h);
    h.send("\x02");
    std::thread::sleep(Duration::from_millis(200));
    h.send("d");
    h.wait_for("Markdowneditor", "writer empty state");
    h.send("o");
    h.wait_for("Opendocumentin", "open prompt");
    h.send("hover.md\r");
    let text = h.wait_for("rev0", "doc open");
    assert!(text.contains("hover.md"), "title names the file");
    let (x, y) = h.find("alpha").expect("doc text on screen");
    h.settle();
    let baseline = row_signature(&h.parser, y);
    // Sweep the pointer across the word in both motion encodings.
    for dx in 0..8 {
        h.motion(67, x + dx, y);
        h.motion(64, x + dx, y);
    }
    h.settle();
    assert_eq!(
        row_signature(&h.parser, y),
        baseline,
        "hover motion paints no highlight"
    );
}

#[test]
fn writer_highlights_heading_and_fence() {
    let mut h = boot(&[("md.md", "# Title\n\n```rs\nlet x = 1;\n```\n")]);
    create_session(&mut h);
    h.send("\x02");
    std::thread::sleep(Duration::from_millis(200));
    h.send("d");
    h.wait_for("Markdowneditor", "writer empty state");
    h.send("o");
    h.wait_for("Opendocumentin", "open prompt");
    h.send("md.md\r");
    let text = h.wait_for("rev0", "doc open");
    assert!(text.contains("md.md"), "title names the file");
    let (_, y) = h.find("Title").expect("heading on screen");
    h.settle();
    // The heading row carries styled cells (bold, non-default fg);
    // plain body text would be neither.
    let styled = row_signature(&h.parser, y)
        .into_iter()
        .any(|(_, fg, _, bold, _)| bold && fg != "Default");
    assert!(styled, "heading row is highlighted");
}

#[test]
fn writer_double_click_selects_word_and_typing_replaces() {
    let mut h = boot(&[("dbl.md", "foo bar\n")]);
    create_session(&mut h);
    h.send("\x02");
    std::thread::sleep(Duration::from_millis(200));
    h.send("d");
    h.wait_for("Markdowneditor", "writer empty state");
    h.send("o");
    h.wait_for("Opendocumentin", "open prompt");
    h.send("dbl.md\r");
    let text = h.wait_for("rev0", "doc open");
    assert!(text.contains("dbl.md"), "title names the file");
    let (x, y) = h.find("bar").expect("word on screen");
    // Two fast presses chain into a word selection.
    h.press(x, y);
    h.press(x, y);
    // Typing replaces the selected word through the real app.
    // (The needle squashes whitespace; the screen keeps "foo X".)
    h.send("X");
    let text = h.wait_for("fooX", "typed replacement");
    assert!(text.contains("foo X"), "double-click selects, typing replaces");
}

#[test]
fn writer_copy_paste_round_trip_emits_osc52() {
    let mut h = boot(&[("cp.md", "aaabbb\n")]);
    create_session(&mut h);
    h.send("\x02");
    std::thread::sleep(Duration::from_millis(200));
    h.send("d");
    h.wait_for("Markdowneditor", "writer empty state");
    h.send("o");
    h.wait_for("Opendocumentin", "open prompt");
    h.send("cp.md\r");
    h.wait_for("rev0", "doc open");
    // Select "bbb" with Shift+Left and copy it.
    h.send("\x1b[F");
    for _ in 0..3 {
        h.send("\x1b[1;2D");
    }
    h.send("\x03");
    std::thread::sleep(Duration::from_millis(300));
    h.settle();
    assert!(h.saw_raw("]52;c;"), "copy announces OSC 52");
    // Home, select "aaa", paste over it: "bbb" lands twice.
    h.send("\x1b[H");
    for _ in 0..3 {
        h.send("\x1b[1;2C");
    }
    h.send("\x16");
    let text = h.wait_for("bbbbbb", "pasted round trip");
    assert!(text.contains("bbbbbb"), "paste replaces the selection");
}

#[test]
fn writer_prompt_home_fixes_first_char() {
    let mut h = boot(&[("needed.md", "need\n")]);
    create_session(&mut h);
    h.send("\x02");
    std::thread::sleep(Duration::from_millis(200));
    h.send("d");
    h.wait_for("Markdowneditor", "writer empty state");
    h.send("o");
    h.wait_for("Opendocumentin", "open prompt");
    h.send("eeded.md");
    std::thread::sleep(Duration::from_millis(300));
    h.send("\x1b[H");
    std::thread::sleep(Duration::from_millis(200));
    h.send("n");
    h.send("\r");
    let text = h.wait_for("rev0", "fixed path opens");
    assert!(text.contains("needed.md"), "home-fixed path opens the file");
}

/// The polish items you can see: Alt+A shows the Assistant panel with
/// its pressed check marker, a divider column splits editor and panel,
/// and the status row sits pinned above the error slot.
#[test]
fn writer_panel_shows_divider_and_pinned_status() {
    let mut h = boot(&[("seeded.md", "seed\n")]);
    create_session(&mut h);
    h.send("\x02");
    std::thread::sleep(Duration::from_millis(200));
    h.send("d");
    h.wait_for("Markdowneditor", "writer empty state");
    h.send("o");
    h.wait_for("Opendocumentin", "open prompt");
    h.send("seeded.md\r");
    h.wait_for("rev0", "doc open");
    // Alt+A shows the Assistant panel; the toolbar toggle carries the
    // pressed check (never the default star). Lowercase a: the binding
    // is Char('a'), and uppercase would read as Shift, not Alt.
    h.send("\x1ba");
    h.wait_for("\u{2713}Assistant", "panel pressed marker");
    let (_cols, grid) = h.grid();
    // Locate the Writer frame from its toolbar row: the pressed toggle
    // only paints there, two rows below the frame top.
    let title_y = grid
        .iter()
        .position(|row| row.concat().contains("\u{2713}Assistant"))
        .expect("toolbar row");
    let top = title_y - 2;
    let x0 = grid[top].iter().position(|c| c == "╭").expect("frame left") as u16;
    let x1 = grid[top].iter().rposition(|c| c == "╮").expect("frame right") as u16;
    let width = x1 - x0 + 1;
    let bottom = grid
        .iter()
        .skip(top)
        .position(|row| {
            row.iter().skip(x0 as usize).take(2).any(|c| c == "╰")
        })
        .map(|i| top + i)
        .expect("frame bottom");
    // Divider column mirrors the layout math: 3 cells of chrome, then
    // 65% editor, then the divider.
    let inner = width - 6;
    let div_x = x0 + 3 + inner * 65 / 100;
    for y in (top + 5)..(top + 10) {
        assert_eq!(
            grid[y].get(div_x as usize).map(String::as_str),
            Some("│"),
            "divider column at ({div_x}, {y})"
        );
    }
    // Status sits pinned three rows above the frame bottom
    // (pad, error slot, then status).
    let status_row: String = grid[bottom - 3].concat();
    assert!(status_row.contains("rev"), "pinned status row: {status_row:?}");
}
