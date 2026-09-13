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

pub struct PtyPane {
    id: SessionId,
    rows: u16,
    cols: u16,
    writer: Option<Box<dyn Write + Send>>,
    master: Option<Box<dyn MasterPty + Send>>,
    child: ChildCell,
    screen: std::sync::Arc<std::sync::Mutex<vt100::Parser>>,
    _reader: Option<thread::JoinHandle<()>>,
}

fn lock_child(cell: &ChildCell) -> MutexGuard<'_, Box<dyn portable_pty::Child + Send + Sync>> {
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
        let child = pair.slave.spawn_command(cmd).map_err(pty_error)?;
        // The slave side must be closed in this process so EOF propagates.
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(pty_error)?;
        let writer = pair.master.take_writer().map_err(pty_error)?;
        let child: ChildCell = Arc::new(Mutex::new(child));
        let screen = std::sync::Arc::new(std::sync::Mutex::new(vt100::Parser::new(
            rows, cols, 1000,
        )));
        // Reap promptly so short-lived commands never linger as zombies: the
        // reader observes EOF first, then waits for the status.
        let handle = Self::spawn_reader(id, reader, Arc::clone(&child), Arc::clone(&screen), tx);
        Ok(PtyPane {
            id,
            rows,
            cols,
            writer: Some(writer),
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
        match self.writer.as_mut() {
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

    /// Kill the child and release the master side. The reader thread reports
    /// the exit afterwards. A plain master hangup is not enough: the reader's
    /// own cloned fd keeps the PTY open, so an output-less child (e.g. a
    /// sleeping harness) would never see EOF without the kill.
    pub fn close(&mut self) {
        let _ = lock_child(&self.child).kill();
        self.writer = None;
        self.master = None;
    }

    fn spawn_reader(
        id: SessionId,
        mut reader: Box<dyn Read + Send>,
        child: ChildCell,
        screen: std::sync::Arc<std::sync::Mutex<vt100::Parser>>,
        tx: Sender<(SessionId, PtyEvent)>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let mut buf = [0u8; READ_BUF];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        lock_screen(&screen).process(&buf[..n]);
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
