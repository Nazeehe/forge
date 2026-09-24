//! Saved session snapshots (`~/.forge/sessions`): quitting forge
//! serializes every live agent session, and startup offers the saved
//! entries back through a picker (or a fresh instance). Plain JSON via
//! `serde_json::Value` — no derive macros — written atomically; a
//! missing or corrupt file reads as empty. Newest entries last, capped.

use std::path::Path;

/// Cap on stored snapshots; oldest entries drop first.
pub const MAX_ENTRIES: usize = 20;

/// One restorable agent session: everything resume argv needs plus the
/// human-facing identity and group memberships.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedSession {
    pub name: String,
    pub cli_tool: String,
    pub cwd: String,
    pub groups: Vec<String>,
    pub harness_session_id: Option<String>,
}

/// One quit-time snapshot of the whole topology.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedEntry {
    pub label: String,
    pub saved_at_unix: u64,
    pub sessions: Vec<SavedSession>,
}

/// The sessions file in memory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionsFile {
    pub entries: Vec<SavedEntry>,
}

/// Seconds since the Unix epoch; saturates to zero before 1970.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Short human age for the picker ("just now", "5m ago").
pub fn age_string(saved_at_unix: u64, now_unix: u64) -> String {
    let ago = now_unix.saturating_sub(saved_at_unix);
    if ago < 60 {
        "just now".to_string()
    } else if ago < 3600 {
        format!("{}m ago", ago / 60)
    } else if ago < 86400 {
        format!("{}h ago", ago / 3600)
    } else {
        format!("{}d ago", ago / 86400)
    }
}

/// Build one snapshot entry: the label names the member sessions (the
/// picker appends the count and age itself).
pub fn make_entry(sessions: Vec<SavedSession>, now: u64) -> SavedEntry {
    let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
    let label = if names.is_empty() {
        "empty".to_string()
    } else {
        names.join(", ")
    };
    SavedEntry {
        label,
        saved_at_unix: now,
        sessions,
    }
}

impl SessionsFile {
    /// Read the file; missing, unreadable, or corrupt content yields an
    /// empty file rather than an error — a bad snapshot must never block
    /// startup.
    pub fn load(path: &Path) -> SessionsFile {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.trim().is_empty() {
            return SessionsFile::default();
        }
        parse(&text).unwrap_or_default()
    }

    /// Append one snapshot, pruning oldest past the cap.
    pub fn push(&mut self, entry: SavedEntry) {
        self.entries.push(entry);
        while self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
    }

    /// Atomic write of the whole file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut entries = Vec::new();
        for entry in &self.entries {
            let mut sessions = Vec::new();
            for session in &entry.sessions {
                sessions.push(serde_json::json!({
                    "name": session.name,
                    "cli_tool": session.cli_tool,
                    "cwd": session.cwd,
                    "groups": session.groups,
                    "harness_session_id": session.harness_session_id,
                }));
            }
            entries.push(serde_json::json!({
                "label": entry.label,
                "saved_at_unix": entry.saved_at_unix,
                "sessions": sessions,
            }));
        }
        let text = serde_json::json!({ "entries": entries }).to_string();
        crate::fs_atomic::write_atomic(path, text.as_bytes())
    }
}

/// Quit-time persist: appends one snapshot when sessions are live,
/// leaves the file alone when none are. An empty quit must never
/// destroy older entries: dismissing the picker (Esc) then quitting
/// fresh would otherwise eat the very offer it skipped. A re-quit with
/// the same topology refreshes the newest entry in place instead of
/// stacking a duplicate picker row; the fresh harness ids still land.
pub fn save_quit_snapshot(path: &Path, sessions: Vec<SavedSession>) -> std::io::Result<()> {
    if sessions.is_empty() {
        return Ok(());
    }
    let mut file = SessionsFile::load(path);
    let entry = make_entry(sessions, now_unix());
    if let Some(last) = file.entries.last_mut() {
        if same_topology(&last.sessions, &entry.sessions) {
            *last = entry;
            return file.save(path);
        }
    }
    file.push(entry);
    file.save(path)
}

/// Resume-relevant identity, pairwise in manager order: name, tool,
/// cwd, groups. The harness session id is excluded on purpose — it
/// churns across restore cycles for the same logical session, and
/// letting it force a new row is what stacked the duplicates.
fn same_topology(a: &[SavedSession], b: &[SavedSession]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(x, y)| {
            x.name == y.name
                && x.cli_tool == y.cli_tool
                && x.cwd == y.cwd
                && x.groups == y.groups
        })
}

fn parse(text: &str) -> Option<SessionsFile> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let mut entries = Vec::new();
    for raw in value.get("entries")?.as_array()? {
        let mut sessions = Vec::new();
        for item in raw.get("sessions")?.as_array()? {
            sessions.push(SavedSession {
                name: item.get("name")?.as_str()?.to_string(),
                cli_tool: item.get("cli_tool")?.as_str()?.to_string(),
                cwd: item.get("cwd")?.as_str()?.to_string(),
                groups: item
                    .get("groups")?
                    .as_array()?
                    .iter()
                    .map(|g| g.as_str().map(str::to_string))
                    .collect::<Option<Vec<String>>>()?,
                harness_session_id: match item.get("harness_session_id") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(v) => Some(v.as_str()?.to_string()),
                },
            });
        }
        entries.push(SavedEntry {
            label: raw.get("label")?.as_str()?.to_string(),
            saved_at_unix: raw.get("saved_at_unix")?.as_u64()?,
            sessions,
        });
    }
    Some(SessionsFile { entries })
}

/// Startup restore picker: newest entry preselected, Enter loads it,
/// Esc starts fresh. Keyboard-first like every other dialog. Full
/// entries ride along so a pick restores without re-reading the file.
pub struct RestorePicker {
    entries: Vec<SavedEntry>,
    selected: usize,
}

/// Dialog result after one input.
#[derive(Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    Pending,
    /// Fresh instance: leave the file alone.
    Fresh,
    /// Load entries[selected].
    Pick(usize),
}

impl RestorePicker {
    pub fn new(file: &SessionsFile) -> Option<RestorePicker> {
        if file.entries.is_empty() {
            return None;
        }
        let selected = file.entries.len().saturating_sub(1);
        Some(RestorePicker {
            entries: file.entries.clone(),
            selected,
        })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Clone the selected entry for the restore path.
    pub fn take_selected(&self) -> Option<SavedEntry> {
        self.entries.get(self.selected).cloned()
    }

    pub fn key(&mut self, key: &crossterm::event::KeyEvent) -> RestoreOutcome {
        use crossterm::event::{KeyCode, KeyModifiers};
        if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return RestoreOutcome::Pending;
        }
        match key.code {
            KeyCode::Esc => RestoreOutcome::Fresh,
            KeyCode::Enter => RestoreOutcome::Pick(self.selected),
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                RestoreOutcome::Pending
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.entries.len().saturating_sub(1));
                RestoreOutcome::Pending
            }
            KeyCode::Char('k') if key.modifiers.is_empty() => {
                self.selected = self.selected.saturating_sub(1);
                RestoreOutcome::Pending
            }
            KeyCode::Char('j') if key.modifiers.is_empty() => {
                self.selected = (self.selected + 1).min(self.entries.len().saturating_sub(1));
                RestoreOutcome::Pending
            }
            _ => RestoreOutcome::Pending,
        }
    }

    /// Centered picker box, clamped into tiny terminals.
    pub fn picker_area(term: ratatui::layout::Rect) -> ratatui::layout::Rect {
        let (w, h) = (64.min(term.width), 20.min(term.height));
        ratatui::layout::Rect::new(
            term.x + term.width.saturating_sub(w) / 2,
            term.y + term.height.saturating_sub(h) / 2,
            w,
            h,
        )
    }

    /// Render the box: title, one row per snapshot, footer hints.
    /// Content keeps border padding; roomy boxes pin the hint to the
    /// bottom row, and long labels truncate with an ellipsis so rows
    /// never spill past the frame. The cursor marker is `>`, the same
    /// language the other pickers speak.
    pub fn view(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        use ratatui::widgets::{Block, Borders, Paragraph};
        let block = Block::default()
            .borders(Borders::ALL)
                .border_type(crate::theme::border_type())
            .title(" Restore session ")
            .style(crate::theme::style(crate::theme::Role::BorderFocused));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height < 4 || inner.width < 20 {
            return;
        }
        let roomy = inner.height >= 6;
        let cx = inner.x + 2;
        let cw = inner.width.saturating_sub(4);
        let end = inner.y + inner.height;
        let mut row = inner.y + if roomy { 1 } else { 0 };
        let now = now_unix();
        for (index, entry) in self.entries.iter().enumerate() {
            if row + 1 >= end {
                break;
            }
            let mark = if index == self.selected { ">" } else { " " };
            let noun = if entry.sessions.len() == 1 {
                "session"
            } else {
                "sessions"
            };
            let detail = format!(
                "({} {noun} · {})",
                entry.sessions.len(),
                age_string(entry.saved_at_unix, now)
            );
            // Metadata is the load-bearing part; the label yields.
            let room = (cw as usize).saturating_sub(3 + detail.chars().count());
            let line = format!("{mark} {} {detail}", crate::groups::fit_row(&entry.label, room));
            let style = if index == self.selected {
                crate::theme::style(crate::theme::Role::Focus)
            } else {
                crate::theme::style(crate::theme::Role::Text)
            };
            frame.render_widget(
                Paragraph::new(line).style(style),
                ratatui::layout::Rect::new(cx, row, cw, 1),
            );
            row += 1;
        }
        if roomy {
            row = end.saturating_sub(1);
        }
        if row < end {
            frame.render_widget(
                Paragraph::new("Enter load • Esc fresh start"),
                ratatui::layout::Rect::new(cx, row, cw, 1),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn saved(name: &str) -> SavedSession {
        SavedSession {
            name: name.to_string(),
            cli_tool: "claude".to_string(),
            cwd: "/tmp/proj".to_string(),
            groups: vec!["peers".to_string()],
            harness_session_id: Some("harness-1".to_string()),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn round_trip_preserves_everything() {
        let dir = std::env::temp_dir().join(format!("forge-ckpt-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sessions");
        let mut file = SessionsFile::default();
        file.push(make_entry(vec![saved("a"), saved("b")], 1_700_000_000));
        file.save(&path).unwrap();
        let back = SessionsFile::load(&path);
        assert_eq!(back, file);
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].sessions[0].harness_session_id.as_deref(), Some("harness-1"));
        assert!(back.entries[0].label.contains('a'));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_quit_keeps_older_entries_live_quit_appends() {
        let dir = std::env::temp_dir().join(format!("forge-ckpt-quit-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sessions");
        let mut file = SessionsFile::default();
        file.push(make_entry(vec![saved("old")], 1_700_000_000));
        file.save(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        // Esc then quit-empty: the skipped offer must survive.
        save_quit_snapshot(&path, Vec::new()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before, "empty quit writes nothing");
        assert!(path.exists(), "empty quit deletes nothing");
        // Quitting with live agents appends beside the old entry.
        save_quit_snapshot(&path, vec![saved("new")]).unwrap();
        let back = SessionsFile::load(&path);
        assert_eq!(back.entries.len(), 2);
        assert_eq!(back.entries[0].sessions[0].name, "old");
        assert_eq!(back.entries[1].sessions[0].name, "new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_and_corrupt_read_as_empty() {
        let missing = std::env::temp_dir().join(format!("forge-ckpt-nope-{}", std::process::id()));
        let _ = std::fs::remove_file(&missing);
        assert!(SessionsFile::load(&missing).entries.is_empty());
        let dir = std::env::temp_dir().join(format!("forge-ckpt-bad-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sessions");
        std::fs::write(&path, "{oops").unwrap();
        assert!(SessionsFile::load(&path).entries.is_empty());
        std::fs::write(&path, r#"{"entries":[{"label":"x"}]}"#).unwrap();
        assert!(SessionsFile::load(&path).entries.is_empty(), "shape-checked");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn steady_quits_refresh_newest_instead_of_stacking() {
        let dir = std::env::temp_dir().join(format!("forge-ckpt-dedupe-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sessions");
        save_quit_snapshot(&path, vec![saved("a")]).unwrap();
        // Same topology, churned harness id: refresh in place, no new row.
        let mut again = saved("a");
        again.harness_session_id = Some("harness-2".to_string());
        save_quit_snapshot(&path, vec![again]).unwrap();
        let back = SessionsFile::load(&path);
        assert_eq!(back.entries.len(), 1, "no duplicate row");
        assert_eq!(
            back.entries[0].sessions[0].harness_session_id.as_deref(),
            Some("harness-2"),
            "ids stay fresh"
        );
        // Same names, different groups: a real change, appends.
        let mut regrouped = saved("a");
        regrouped.groups = vec!["other".to_string()];
        save_quit_snapshot(&path, vec![regrouped]).unwrap();
        assert_eq!(SessionsFile::load(&path).entries.len(), 2);
        // ...and re-quitting that topology is stable again.
        let mut regrouped2 = saved("a");
        regrouped2.groups = vec!["other".to_string()];
        save_quit_snapshot(&path, vec![regrouped2]).unwrap();
        assert_eq!(SessionsFile::load(&path).entries.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn push_prunes_oldest_past_the_cap() {
        let mut file = SessionsFile::default();
        for n in 0..MAX_ENTRIES + 3 {
            file.push(make_entry(vec![saved("a")], n as u64));
        }
        assert_eq!(file.entries.len(), MAX_ENTRIES);
        assert_eq!(file.entries[0].saved_at_unix, 3);
        assert_eq!(file.entries[MAX_ENTRIES - 1].saved_at_unix, (MAX_ENTRIES + 2) as u64);
    }

    #[test]
    fn picker_selects_newest_and_navigates() {
        let mut file = SessionsFile::default();
        file.push(make_entry(vec![saved("a")], 100));
        file.push(make_entry(vec![saved("b")], 200));
        let mut picker = RestorePicker::new(&file).unwrap();
        assert_eq!(picker.len(), 2);
        assert_eq!(picker.selected(), 1, "newest preselected");
        assert!(matches!(picker.key(&key(KeyCode::Up)), RestoreOutcome::Pending));
        assert_eq!(picker.selected(), 0);
        assert!(matches!(picker.key(&key(KeyCode::Up)), RestoreOutcome::Pending));
        assert_eq!(picker.selected(), 0, "clamps at top");
        assert_eq!(picker.key(&key(KeyCode::Enter)), RestoreOutcome::Pick(0));
        assert_eq!(picker.key(&key(KeyCode::Esc)), RestoreOutcome::Fresh);
        assert!(RestorePicker::new(&SessionsFile::default()).is_none(), "empty offers nothing");
    }

    #[test]
    fn picker_pads_content_and_pins_hint() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut file = SessionsFile::default();
        file.push(make_entry(vec![saved("a")], 100));
        file.push(make_entry(vec![saved("b")], 200));
        let picker = RestorePicker::new(&file).unwrap();
        // 80x24 centers the 64x20 box at (8, 2): padded content at
        // x11, first entry one row down, hint on the bottom row.
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| {
            picker.view(f, RestorePicker::picker_area(f.area()));
        }).unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(buf[(9, 4)].symbol(), " ", "border padding");
        assert_eq!(buf[(11, 4)].symbol(), " ", "older entry unmarked");
        assert_eq!(buf[(11, 5)].symbol(), ">", "newest preselected");
        let hint: String = (11..70).map(|x| buf[(x, 20)].symbol()).collect();
        assert!(hint.contains("Enter load"), "pinned hint: {hint:?}");
    }

    #[test]
    fn picker_truncates_long_labels_inside_the_box() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut file = SessionsFile::default();
        file.push(make_entry(vec![saved(&"x".repeat(100))], 100));
        let picker = RestorePicker::new(&file).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| {
            picker.view(f, RestorePicker::picker_area(f.area()));
        }).unwrap();
        let buf = terminal.backend().buffer();
        // Padded content width is 58: the row fits exactly, ellipsis
        // marking the cut.
        let row: String = (11..11 + 58).map(|x| buf[(x, 4)].symbol()).collect();
        assert_eq!(row.chars().count(), 58);
        assert!(row.contains("…"), "truncated: {row:?}");
        assert!(!row.contains("xxxxxxxxxx "), "no overflow past the box");
    }

    #[test]
    fn age_strings_cover_units() {
        assert_eq!(age_string(1000, 1010), "just now");
        assert_eq!(age_string(1000, 1300), "5m ago");
        assert_eq!(age_string(1000, 8200), "2h ago");
        assert_eq!(age_string(1000, 90000), "1d ago");
    }
}
