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
    _reader: Option<thread::JoinHandle<()>>,
}

fn lock_child(cell: &ChildCell) -> MutexGuard<'_, Box<dyn portable_pty::Child + Send + Sync>> {
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
        // Reap promptly so short-lived commands never linger as zombies: the
        // reader observes EOF first, then waits for the status.
        let handle = Self::spawn_reader(id, reader, Arc::clone(&child), tx);
        Ok(PtyPane {
            id,
            rows,
            cols,
            writer: Some(writer),
            master: Some(pair.master),
            child,
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
                }),
            None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "pane is closed")),
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
        tx: Sender<(SessionId, PtyEvent)>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let mut buf = [0u8; READ_BUF];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
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
