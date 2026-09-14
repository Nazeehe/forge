//! PTY pane: one child process behind a portable-pty pair.
//!
//! A reader thread pumps bytes into an MPSC channel as [`PtyEvent`]s; the
//! session manager translates those into `AppEvent`. Closing the pane drops
//! the master side, which HUPs the child on Unix; the reader then reports
//! the exit. Nothing here touches `AppState`.

use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::session::SessionId;

const READ_BUF: usize = 8192;

/// Raw happenings from one pane's reader thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PtyEvent {
    Output(Vec<u8>),
    Exited(Option<i32>),
}

type ChildCell = Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>;

/// Plain-text color of one terminal cell. Mirrors `vt100::Color` so the
/// view layer never depends on the parser crate's versioning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellColor {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Text attributes of one terminal cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellFormat {
    pub fg: CellColor,
    pub bg: CellColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    /// Faint (SGR 2): agent ghost/prediction text. Needs the vendored
    /// vt100 patch; upstream 0.15.2 drops the attribute at parse time.
    pub dim: bool,
}

impl CellFormat {
    /// No color, no attributes: plain terminal text.
    pub fn plain() -> Self {
        CellFormat {
            fg: CellColor::Default,
            bg: CellColor::Default,
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
            dim: false,
        }
    }
}

/// One non-empty terminal cell with its text and attributes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattedCell {
    pub text: String,
    pub format: CellFormat,
}

/// `TERM` to advertise when the inherited value cannot address the screen.
/// Capable values pass through untouched; only missing or hopeless ones
/// are replaced.
fn advertised_term(current: Option<&str>) -> Option<&'static str> {
    match current {
        None | Some("") | Some("dumb") | Some("unknown") => Some("xterm-256color"),
        _ => None,
    }
}

fn map_color(color: vt100::Color) -> CellColor {
    match color {
        vt100::Color::Default => CellColor::Default,
        vt100::Color::Idx(i) => CellColor::Indexed(i),
        vt100::Color::Rgb(r, g, b) => CellColor::Rgb(r, g, b),
    }
}

/// Shared child-stdin handle: human input (`write_all`) and terminal
/// query replies (reader thread) serialize through one lock.
type WriterCell = Arc<Mutex<Option<Box<dyn Write + Send>>>>;

/// Cursor-position request: the one device query forge answers (Phase 2.5
/// seed). Capability-probing CLIs (muse) block on it at boot; the vendored
/// parser consumes it, so the reader must reply or they die.
const DSR_CPR: &[u8; 4] = b"\x1b[6n";

pub struct PtyPane {
    id: SessionId,
    rows: u16,
    cols: u16,
    writer: WriterCell,
    master: Option<Box<dyn MasterPty + Send>>,
    child: ChildCell,
    screen: std::sync::Arc<std::sync::Mutex<vt100::Parser>>,
    _reader: Option<thread::JoinHandle<()>>,
}

fn lock_child(cell: &ChildCell) -> MutexGuard<'_, Box<dyn portable_pty::Child + Send + Sync>> {
    cell.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock_writer(cell: &WriterCell) -> MutexGuard<'_, Option<Box<dyn Write + Send>>> {
    cell.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock_screen(cell: &std::sync::Arc<std::sync::Mutex<vt100::Parser>>) -> std::sync::MutexGuard<'_, vt100::Parser> {
    cell.lock().unwrap_or_else(|e| e.into_inner())
}

impl PtyPane {
    /// Spawn `shell -c cmd` in `cwd` with the given grid size. Reader events
    /// arrive as `(id, PtyEvent)` on `tx`.
    pub fn spawn(
        id: SessionId,
        shell_cmd: &str,
        cwd: &Path,
        rows: u16,
        cols: u16,
        tx: Sender<(SessionId, PtyEvent)>,
    ) -> io::Result<Self> {
        Self::spawn_with_env(id, shell_cmd, cwd, rows, cols, tx, &[])
    }

    /// Spawn with extra child environment entries. The parent environment
    /// (including `FORGE_IPC_ENDPOINT`) is always inherited.
    pub fn spawn_with_env(
        id: SessionId,
        shell_cmd: &str,
        cwd: &Path,
        rows: u16,
        cols: u16,
        tx: Sender<(SessionId, PtyEvent)>,
        extra_env: &[(&str, &str)],
    ) -> io::Result<Self> {
        let system = native_pty_system();
        let pair = system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(pty_error)?;
        let mut cmd = CommandBuilder::new("sh");
        cmd.arg("-c");
        cmd.arg(shell_cmd);
        cmd.cwd(cwd);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        if let Some(term) = advertised_term(std::env::var("TERM").ok().as_deref()) {
            cmd.env("TERM", term);
        }
        let child = pair.slave.spawn_command(cmd).map_err(pty_error)?;
        // The slave side must be closed in this process so EOF propagates.
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(pty_error)?;
        let writer: WriterCell = Arc::new(Mutex::new(Some(
            pair.master.take_writer().map_err(pty_error)?,
        )));
        let child: ChildCell = Arc::new(Mutex::new(child));
        let screen = std::sync::Arc::new(std::sync::Mutex::new(vt100::Parser::new(
            rows, cols, 1000,
        )));
        // Reap promptly so short-lived commands never linger as zombies: the
        // reader observes EOF first, then waits for the status.
        let handle = Self::spawn_reader(
            id,
            reader,
            Arc::clone(&child),
            Arc::clone(&screen),
            tx,
            Arc::clone(&writer),
        );
        Ok(PtyPane {
            id,
            rows,
            cols,
            writer,
            master: Some(pair.master),
            child,
            screen,
            _reader: Some(handle),
        })
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        match lock_writer(&self.writer).as_mut() {
            Some(w) => w.write_all(bytes),
            None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "pane is closed")),
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        match self.master.as_ref() {
            Some(m) => m
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(pty_error)
                .map(|_| {
                    self.rows = rows;
                    self.cols = cols;
                    lock_screen(&self.screen).set_size(rows, cols);
                }),
            None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "pane is closed")),
        }
    }

    /// Current visible screen contents, for grid rendering.
    pub fn screen_text(&self) -> String {
        lock_screen(&self.screen).screen().contents()
    }

    /// Visible screen as styled rows. Every cell is emitted: never-written
    /// cells become padding spaces carrying their own format (backgrounds
    /// included). Skipping them would shift every later column left and
    /// shred fullscreen layouts. Wide-char continuations are skipped (the
    /// lead cell carries the glyph); trailing plain padding and blank rows
    /// are trimmed.
    pub fn styled_rows(&self) -> Vec<Vec<FormattedCell>> {
        let parser = lock_screen(&self.screen);
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let mut out = Vec::new();
        for r in 0..rows {
            let mut line = Vec::new();
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let text = cell.contents();
                let format = CellFormat {
                    fg: map_color(cell.fgcolor()),
                    bg: map_color(cell.bgcolor()),
                    bold: cell.bold(),
                    italic: cell.italic(),
                    underline: cell.underline(),
                    inverse: cell.inverse(),
                    dim: cell.dim(),
                };
                line.push(FormattedCell {
                    text: if text.is_empty() { " ".to_string() } else { text },
                    format,
                });
            }
            while line.last().is_some_and(|cell| {
                cell.text == " " && cell.format == CellFormat::plain()
            }) {
                line.pop();
            }
            out.push(line);
        }
        while out.last().is_some_and(|line| line.is_empty()) {
            out.pop();
        }
        out
    }

    /// Whether the application is using the alternate screen (`DECSET 1049`).
    pub fn alternate_screen(&self) -> bool {
        lock_screen(&self.screen).screen().alternate_screen()
    }

    /// Whether the application requested bracketed paste (`DECSET 2004`).
    pub fn bracketed_paste(&self) -> bool {
        lock_screen(&self.screen).screen().bracketed_paste()
    }

    /// The application's requested mouse protocol mode and encoding
    /// (`DECSET 1000/1002/1003` + `1005/1006`).
    pub fn mouse_mode(&self) -> vt100::MouseProtocolMode {
        lock_screen(&self.screen).screen().mouse_protocol_mode()
    }

    /// See [`PtyPane::mouse_mode`].
    pub fn mouse_encoding(&self) -> vt100::MouseProtocolEncoding {
        lock_screen(&self.screen).screen().mouse_protocol_encoding()
    }

    /// Lines moved per wheel notch when the pane owns the wheel.
    pub const SCROLL_LINES_PER_NOTCH: i32 = 3;

    /// Move the scrollback viewport: positive climbs into history,
    /// negative returns toward live; the grid clamps to buffered
    /// history. No-op on the alternate screen, which has no scrollback.
    pub fn scroll_viewport(&self, lines: i32) {
        let mut parser = lock_screen(&self.screen);
        let screen = parser.screen_mut();
        if screen.alternate_screen() {
            return;
        }
        let pos = screen.scrollback() as i32;
        screen.set_scrollback(pos.saturating_add(lines).max(0) as usize);
    }

    /// Return the viewport to the live tail.
    pub fn reset_viewport(&self) {
        lock_screen(&self.screen).screen_mut().set_scrollback(0);
    }

    /// Whether the application requested application-cursor keys (`DECCKM`):
    /// arrows must arrive as SS3 (`\x1bOA`) rather than CSI (`\x1b[A`).
    pub fn application_cursor(&self) -> bool {
        lock_screen(&self.screen).screen().application_cursor()
    }

    /// Visible cursor as 0-based (row, col), or `None` when the application
    /// hid it (`DECSET 25`).
    pub fn cursor(&self) -> Option<(u16, u16)> {
        let parser = lock_screen(&self.screen);
        let screen = parser.screen();
        if screen.hide_cursor() {
            None
        } else {
            Some(screen.cursor_position())
        }
    }

    /// Poll interval while waiting for a SIGTERM'd child to exit.
    const TERMINATE_POLL: Duration = Duration::from_millis(25);

    /// Ask the child to exit on its own (SIGTERM), without waiting.
    /// True when the signal was sent or the child is already gone; false
    /// only when signaling itself failed. The reader thread keeps sole
    /// `waitpid` ownership, so exit reporting is unaffected.
    pub fn terminate(&self) -> bool {
        let Some(pid) = lock_child(&self.child).process_id() else {
            return true;
        };
        // SAFETY: kill with a signal number only delivers; it never
        // touches memory.
        let sent = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0;
        sent || !Self::pid_alive(pid)
    }

    /// Whether a pid still names a live process. A zombie awaiting reap
    /// counts as alive; an unknown pid counts as gone. EPERM (exists but
    /// unsignalable) counts as alive.
    fn pid_alive(pid: u32) -> bool {
        // SAFETY: signal 0 performs no delivery; it only probes.
        if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    /// Whether this pane's child is still alive. A missing pid counts as
    /// gone.
    pub fn child_alive(&self) -> bool {
        match lock_child(&self.child).process_id() {
            Some(pid) => Self::pid_alive(pid),
            None => false,
        }
    }

    /// SIGTERM the child and wait up to `grace` for it to exit on its own
    /// (agents flush state on TERM). True when the child is gone before
    /// the deadline; false when the caller should `close()` it (SIGKILL)
    /// instead. Never blocks longer than `grace`, and never reaps: the
    /// reader thread reports the real exit code as usual.
    pub fn terminate_gracefully(&self, grace: Duration) -> bool {
        self.terminate();
        let deadline = Instant::now() + grace;
        loop {
            if !self.child_alive() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Self::TERMINATE_POLL);
        }
    }

    /// Kill the child and release the master side. The reader thread reports
    /// the exit afterwards. A plain master hangup is not enough: the reader's
    /// own cloned fd keeps the PTY open, so an output-less child (e.g. a
    /// sleeping harness) would never see EOF without the kill.
    /// Immediate SIGKILL: shutdown paths should `terminate_gracefully`
    /// first so agents can save state.
    pub fn close(&mut self) {
        let _ = lock_child(&self.child).kill();
        lock_writer(&self.writer).take();
        self.master = None;
    }

    fn spawn_reader(
        id: SessionId,
        mut reader: Box<dyn Read + Send>,
        child: ChildCell,
        screen: std::sync::Arc<std::sync::Mutex<vt100::Parser>>,
        tx: Sender<(SessionId, PtyEvent)>,
        writer: WriterCell,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let mut buf = [0u8; READ_BUF];
            // Tail of the previous chunk: a query split across reads must
            // still match.
            let mut carry = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut parser = lock_screen(&screen);
                        parser.process(&buf[..n]);
                        // Follow-tail: a viewport sitting at the live tail
                        // stays there on its own, while a scrolled-up view
                        // holds its position instead of being yanked back
                        // by every chunk — scrolling a busy pane must not
                        // fight the refresh. Typing and injections still
                        // return to live explicitly; scrolling back down
                        // to offset 0 re-engages the tail.
                        drop(parser);
                        Self::answer_device_queries(&screen, &writer, &mut carry, &buf[..n]);
                        if tx.send((id, PtyEvent::Output(buf[..n].to_vec()))).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let code = lock_child(&child)
                .wait()
                .ok()
                .map(|status| status.exit_code() as i32);
            let _ = tx.send((id, PtyEvent::Exited(code)));
        })
    }

    /// Answer cursor-position requests seen in child output. The reply goes
    /// to child stdin (never to the event channel); positions are 1-based
    /// per ECMA-48 while the parser tracks 0-based.
    fn answer_device_queries(
        screen: &std::sync::Arc<std::sync::Mutex<vt100::Parser>>,
        writer: &WriterCell,
        carry: &mut Vec<u8>,
        chunk: &[u8],
    ) {
        carry.extend_from_slice(chunk);
        let queries = carry.windows(DSR_CPR.len()).filter(|w| *w == DSR_CPR).count();
        let keep = carry.len().min(DSR_CPR.len() - 1);
        carry.drain(..carry.len() - keep);
        if queries == 0 {
            return;
        }
        let (row, col) = lock_screen(screen).screen().cursor_position();
        let reply = format!("\x1b[{};{}R", row as u32 + 1, col as u32 + 1);
        for _ in 0..queries {
            let mut guard = lock_writer(writer);
            if let Some(w) = guard.as_mut() {
                let _ = w.write_all(reply.as_bytes());
                let _ = w.flush();
            } else {
                break;
            }
        }
    }
}

fn pty_error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl Drop for PtyPane {
    fn drop(&mut self) {
        // Never orphan the child if the pane is discarded without close().
        let _ = lock_child(&self.child).kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    const TIMEOUT: Duration = Duration::from_secs(10);

    fn workdir() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    /// Drain events until the pane exits; return (bytes seen, exit code).
    fn run_until_exit(pane: &mut PtyPane, rx: &std::sync::mpsc::Receiver<(SessionId, PtyEvent)>) -> (Vec<u8>, Option<i32>) {
        let mut out = Vec::new();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok((_, PtyEvent::Output(b))) => out.extend_from_slice(&b),
                Ok((_, PtyEvent::Exited(code))) => return (out, code),
                Err(_) => panic!("timed out waiting for pane exit"),
            }
        }
    }

    #[test]
    fn echo_and_clean_exit() {
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        let mut pane = PtyPane::spawn(id, "printf hello-pty", &workdir(), 24, 80, tx).unwrap();
        let (out, code) = run_until_exit(&mut pane, &rx);
        assert!(out.windows(9).any(|w| w == b"hello-pty"), "got: {out:?}");
        assert_eq!(code, Some(0));
    }

    #[test]
    fn exit_code_propagates() {
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        let mut pane = PtyPane::spawn(id, "exit 42", &workdir(), 24, 80, tx).unwrap();
        let (_, code) = run_until_exit(&mut pane, &rx);
        assert_eq!(code, Some(42));
    }


    #[test]
    fn write_reaches_child() {
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        let mut pane = PtyPane::spawn(id, "exec cat", &workdir(), 24, 80, tx).unwrap();
        pane.write_all(b"ping-pty\n").unwrap();
        let deadline = Instant::now() + TIMEOUT;
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok((_, PtyEvent::Output(b))) => {
                    seen.extend_from_slice(&b);
                    if seen.windows(8).any(|w| w == b"ping-pty") {
                        break;
                    }
                }
                Ok((_, PtyEvent::Exited(_))) => panic!("cat exited early, got: {seen:?}"),
                Err(_) => panic!("timed out waiting for echo, got: {seen:?}"),
            }
        }
        pane.close();
    }

    #[test]
    fn cursor_position_query_is_answered() {
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        // Raw mode: the reply must arrive as input bytes, not line-buffered.
        // Cursor starts at the origin, so the report is exactly 6 bytes.
        let mut pane = PtyPane::spawn(
            id,
            "stty raw -echo; printf '\\033[6n'; head -c 6; printf 'DONE\\n'",
            &workdir(),
            24,
            80,
            tx,
        )
        .unwrap();
        let deadline = Instant::now() + TIMEOUT;
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok((_, PtyEvent::Output(b))) => {
                    seen.extend_from_slice(&b);
                    if seen.windows(4).any(|w| w == b"DONE") {
                        break;
                    }
                }
                Ok((_, PtyEvent::Exited(_))) => panic!("query reader exited early: {seen:?}"),
                Err(_) => panic!("cursor report never arrived: {seen:?}"),
            }
        }
        assert!(
            seen.windows(6).any(|w| w == b"\x1b[1;1R"),
            "origin report in output: {seen:?}"
        );
        pane.close();
    }

    /// Drain output until `needle` appears; return all bytes seen. A
    /// child that exits first (or never prints) fails the test: without
    /// the marker there is no proof the trap below is installed, and a
    /// SIGTERM sent earlier would race trap installation (default
    /// disposition kills instantly, trap never runs).
    fn drain_until_marker(
        rx: &std::sync::mpsc::Receiver<(SessionId, PtyEvent)>,
        needle: &[u8],
    ) -> Vec<u8> {
        let mut seen = Vec::new();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok((_, PtyEvent::Output(b))) => {
                    seen.extend_from_slice(&b);
                    if seen.windows(needle.len()).any(|w| w == needle) {
                        return seen;
                    }
                }
                Ok((_, PtyEvent::Exited(code))) => {
                    panic!("child exited {code:?} before marker: {seen:?}")
                }
                Err(_) => panic!("marker never arrived: {seen:?}"),
            }
        }
    }

    #[test]
    fn terminate_gracefully_delivers_sigterm_for_clean_child_exit() {
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        // Builtin-only loop: no forked child can swallow the signal, so
        // the trap runs the moment SIGTERM lands. READY is printed after
        // the trap installs, so the SIGTERM below deterministically runs
        // it. Exit code 3 proves the trap ran (signal-death reports 1).
        let mut pane = PtyPane::spawn(
            id,
            "trap 'echo TERM-SAVED; exit 3' TERM; echo READY; while :; do :; done",
            &workdir(),
            24,
            80,
            tx,
        )
        .unwrap();
        let mut all = drain_until_marker(&rx, b"READY");
        assert!(pane.terminate_gracefully(Duration::from_secs(10)));
        let (out, code) = run_until_exit(&mut pane, &rx);
        all.extend_from_slice(&out);
        assert!(all.windows(10).any(|w| w == b"TERM-SAVED"), "got: {all:?}");
        assert_eq!(code, Some(3));
    }

    #[test]
    fn terminate_gracefully_times_out_when_child_ignores_sigterm() {
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        let mut pane = PtyPane::spawn(
            id,
            "trap '' TERM; echo READY; exec sleep 30",
            &workdir(),
            24,
            80,
            tx,
        )
        .unwrap();
        drain_until_marker(&rx, b"READY");
        let start = Instant::now();
        assert!(!pane.terminate_gracefully(Duration::from_millis(300)));
        let waited = start.elapsed();
        assert!(waited >= Duration::from_millis(300), "gave up early: {waited:?}");
        assert!(waited < TIMEOUT, "hung past the grace window: {waited:?}");
        // The ignorer is still alive: only the SIGKILL fallback ends it.
        // portable-pty reports signal-death as code 1 (`with_signal`), so
        // Some(1) here proves the SIGKILL fallback did it — the graceful
        // path would have carried the trap's own code instead.
        pane.close();
        let (_, code) = run_until_exit(&mut pane, &rx);
        assert_eq!(code, Some(1));
    }

    #[test]
    fn screen_shows_output() {
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        let mut pane = PtyPane::spawn(id, "printf hello-screen", &workdir(), 24, 80, tx).unwrap();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if pane.screen_text().contains("hello-screen") {
                break;
            }
            if Instant::now() > deadline {
                panic!("screen never showed output: {:?}", pane.screen_text());
            }
            // Drain so the reader never blocks on a full channel.
            while rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(5));
        }
        let (_, _) = run_until_exit(&mut pane, &rx);
    }

    fn wait_cursor(
        pane: &PtyPane,
        rx: &std::sync::mpsc::Receiver<(SessionId, PtyEvent)>,
        want: Option<(u16, u16)>,
        what: &str,
    ) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if pane.cursor() == want {
                break;
            }
            if Instant::now() > deadline {
                panic!("{what}: want {want:?}, got {:?}", pane.cursor());
            }
            while rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn cursor_tracks_position_and_visibility() {
        // Drive the parser from child stdout: stdin writes would only echo
        // back through the line discipline (ECHOCTL mangles ESC to `^[`),
        // so they can never faithfully carry escape sequences.
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        let mut pane = PtyPane::spawn(
            id,
            "printf '\\033[3;7H'; sleep 2; printf '\\033[?25l'; sleep 2; printf '\\033[?25h'; sleep 30",
            &workdir(),
            24,
            80,
            tx,
        )
        .unwrap();
        wait_cursor(&pane, &rx, Some((2, 6)), "move");
        wait_cursor(&pane, &rx, None, "hide");
        wait_cursor(&pane, &rx, Some((2, 6)), "reshow");
        pane.close();
    }

    fn row_text(row: &[FormattedCell]) -> String {
        row.iter().map(|c| c.text.clone()).collect()
    }

    #[test]
    fn styled_rows_carry_sgr_colors() {
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        let mut pane = PtyPane::spawn(
            id,
            "printf '\\033[31mRED\\033[0m\\nA\\033[2CB'; sleep 30",
            &workdir(),
            24,
            80,
            tx,
        )
        .unwrap();
        let deadline = Instant::now() + TIMEOUT;
        let rows = loop {
            let rows = pane.styled_rows();
            let first = rows.first().map(|row| row_text(row)).unwrap_or_default();
            let second = rows.get(1).map(|row| row_text(row)).unwrap_or_default();
            if first.starts_with("RED") && second.starts_with("A  B") {
                break rows;
            }
            if Instant::now() > deadline {
                panic!("styled rows never arrived: {rows:?}");
            }
            while rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(rows[0].iter().take(3).all(|c| c.format.fg == CellColor::Indexed(1)));
        // Interior gaps must survive as padding: skipping them would shift
        // every later column left and shred fullscreen layouts.
        assert_eq!(&row_text(&rows[1])[..4], "A  B");
        assert!(rows[1].iter().all(|c| c.format == CellFormat::plain()));
        pane.close();
    }

    #[test]
    fn styled_rows_carry_dim() {
        // Ghost/prediction text arrives as SGR 2 (faint). Dropping it
        // renders predictions full-bright white; the dim bit must survive
        // the parser so the view layer can faint it.
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        let mut pane = PtyPane::spawn(
            id,
            "printf 'A\\033[2mDIM\\033[22mB'; sleep 30",
            &workdir(),
            24,
            80,
            tx,
        )
        .unwrap();
        let deadline = Instant::now() + TIMEOUT;
        let rows = loop {
            let rows = pane.styled_rows();
            if row_text(rows.first().unwrap_or(&Vec::new())).starts_with("ADIMB") {
                break rows;
            }
            if Instant::now() > deadline {
                panic!("styled rows never arrived: {rows:?}");
            }
            while rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(!rows[0][0].format.dim, "plain stays plain");
        assert!(
            rows[0][1..4].iter().all(|c| c.format.dim),
            "SGR 2 marks faint: {rows:?}"
        );
        assert!(!rows[0][4].format.dim, "SGR 22 clears faint");
        pane.close();
    }

    #[test]
    fn app_cursor_tracks_decckm() {
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        let mut pane = PtyPane::spawn(
            id,
            "printf '\\033[?1h'; sleep 2; printf '\\033[?1l'; sleep 30",
            &workdir(),
            24,
            80,
            tx,
        )
        .unwrap();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if pane.application_cursor() {
                break;
            }
            if Instant::now() > deadline {
                panic!("DECCKM never engaged");
            }
            while rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(5));
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if !pane.application_cursor() {
                break;
            }
            if Instant::now() > deadline {
                panic!("DECCKM never released");
            }
            while rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(5));
        }
        pane.close();
    }

    #[test]
    fn advertised_term_fills_only_unusable_values() {
        assert_eq!(advertised_term(None), Some("xterm-256color"));
        assert_eq!(advertised_term(Some("")), Some("xterm-256color"));
        assert_eq!(advertised_term(Some("dumb")), Some("xterm-256color"));
        assert_eq!(advertised_term(Some("unknown")), Some("xterm-256color"));
        assert_eq!(advertised_term(Some("xterm-256color")), None);
        assert_eq!(advertised_term(Some("tmux-256color")), None);
    }

    #[test]
    fn alternate_screen_is_visible_and_isolated() {
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        let mut pane = PtyPane::spawn(
            id,
            "printf 'MAIN'; printf '\\033[?1049hALTPAGE'; sleep 2; printf '\\033[?1049l'; sleep 30",
            &workdir(),
            24,
            80,
            tx,
        )
        .unwrap();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if pane.alternate_screen() && pane.screen_text().contains("ALTPAGE") {
                break;
            }
            if Instant::now() > deadline {
                panic!(
                    "alt screen never engaged: {} / {:?}",
                    pane.alternate_screen(),
                    pane.screen_text()
                );
            }
            while rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(5));
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if !pane.alternate_screen() && pane.screen_text().contains("MAIN") {
                break;
            }
            if Instant::now() > deadline {
                panic!(
                    "main screen never restored: {} / {:?}",
                    pane.alternate_screen(),
                    pane.screen_text()
                );
            }
            while rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(5));
        }
        pane.close();
    }

    #[test]
    fn wheel_scrolls_scrollback_and_output_holds_scrolled_view() {
        // Mouse-less panes (mode None) drop wheel events at the router;
        // the viewport below is what the router drives instead, so every
        // session scrolls whether or not its app reports mouse.
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        let mut pane = PtyPane::spawn(
            id,
            "for i in $(seq 1 40); do echo line-$i; done; exec cat",
            &workdir(),
            24,
            80,
            tx,
        )
        .unwrap();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let rows = pane.styled_rows();
            if row_text(rows.last().unwrap_or(&Vec::new())).contains("line-40") {
                break;
            }
            assert!(Instant::now() < deadline, "history never arrived");
            while rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(pane.mouse_mode(), vt100::MouseProtocolMode::None);
        pane.scroll_viewport(6);
        let rows = pane.styled_rows();
        let text: String = rows.iter().map(|r| row_text(r)).collect::<Vec<String>>().concat();
        assert!(!text.contains("line-40"), "scrolled off the live tail");
        assert!(text.contains("line-17"), "older history in view: {text:?}");
        // Down past the bottom clamps back to live.
        pane.scroll_viewport(-100);
        let rows = pane.styled_rows();
        let text: String = rows.iter().map(|r| row_text(r)).collect::<Vec<String>>().concat();
        assert!(text.contains("line-40"), "clamped to live: {text:?}");
        // Fresh output holds a scrolled view instead of yanking it to
        // live: scrolling a busy pane must not fight the refresh.
        pane.scroll_viewport(6);
        pane.write_all(b"echo back-to-live\n").unwrap();
        let deadline = Instant::now() + TIMEOUT;
        // PTY echo splits the line across chunks: accumulate until the
        // full line has passed through the reader (which is what proves
        // the viewport survived fresh output).
        let mut seen = Vec::new();
        loop {
            while let Ok((_, PtyEvent::Output(bytes))) = rx.try_recv() {
                seen.extend_from_slice(&bytes);
            }
            if seen.windows(12).any(|w| w == b"back-to-live") {
                break;
            }
            assert!(Instant::now() < deadline, "output never processed");
            std::thread::sleep(Duration::from_millis(5));
        }
        let rows = pane.styled_rows();
        let text: String = rows.iter().map(|r| row_text(r)).collect::<Vec<String>>().concat();
        assert!(!text.contains("line-40"), "viewport held off the tail: {text:?}");
        assert!(!text.contains("back-to-live"), "fresh line stays at live: {text:?}");
        // Scrolling back down re-engages the live tail, new line included.
        pane.scroll_viewport(-100);
        let rows = pane.styled_rows();
        let text: String = rows.iter().map(|r| row_text(r)).collect::<Vec<String>>().concat();
        assert!(text.contains("back-to-live") && text.contains("line-40"), "live again: {text:?}");
        pane.close();
    }

    #[test]
    fn mouse_mode_tracks_private_modes() {
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        let mut pane = PtyPane::spawn(
            id,
            "printf '\\033[?1000h\\033[?1006h'; sleep 30",
            &workdir(),
            24,
            80,
            tx,
        )
        .unwrap();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            // `?1000h` is press+release (VT200-style), not press-only.
            if pane.mouse_mode() == vt100::MouseProtocolMode::PressRelease
                && pane.mouse_encoding() == vt100::MouseProtocolEncoding::Sgr
            {
                break;
            }
            if Instant::now() > deadline {
                panic!(
                    "mouse mode never engaged: {:?}/{:?}",
                    pane.mouse_mode(),
                    pane.mouse_encoding()
                );
            }
            while rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(5));
        }
        pane.close();
    }

    #[test]
    fn resize_and_close() {
        let (tx, rx) = channel();
        let id = SessionId::fresh();
        let mut pane = PtyPane::spawn(id, "exec sleep 30", &workdir(), 24, 80, tx).unwrap();
        pane.resize(40, 120).unwrap();
        assert_eq!(pane.size(), (40, 120));
        pane.close();
        let (_, _) = run_until_exit(&mut pane, &rx);
    }
}
