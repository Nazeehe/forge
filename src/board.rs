//! Built-in kanban board domain: pure Board/Column/Card/Priority logic.
//!
//! Workspace-global state owned by `AppState`; agents mutate it through MCP
//! tools via the broker. No I/O here — persistence lives with the caller.

/// Maximum boards in one store.
pub const MAX_BOARDS: usize = 32;
/// Maximum columns per board.
pub const MAX_COLUMNS_PER_BOARD: usize = 16;
/// Maximum cards per board.
pub const MAX_CARDS_PER_BOARD: usize = 1024;
/// Maximum card title length in chars.
pub const MAX_TITLE_LEN: usize = 200;
/// Descriptions/tags past their caps are truncated, never rejected.
pub const MAX_DESCRIPTION_LEN: usize = 2000;
/// Maximum tags kept per card.
pub const MAX_TAGS: usize = 16;
/// Maximum tag length in chars.
pub const MAX_TAG_LEN: usize = 40;

/// Default columns for a new board: Backlog, Todo, Doing (WIP 3), Done.
pub fn default_columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec::new("Backlog"),
        ColumnSpec::new("Todo"),
        ColumnSpec::with_wip("Doing", 3),
        ColumnSpec::new("Done"),
    ]
}

/// Card priority. `p` cycles Low -> Normal -> High -> Urgent -> Low.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Low,
    Normal,
    High,
    Urgent,
}

impl Priority {
    pub fn cycle(self) -> Self {
        match self {
            Priority::Low => Priority::Normal,
            Priority::Normal => Priority::High,
            Priority::High => Priority::Urgent,
            Priority::Urgent => Priority::Low,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Urgent => "urgent",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "low" => Priority::Low,
            "high" => Priority::High,
            "urgent" => Priority::Urgent,
            _ => Priority::Normal,
        }
    }
}

/// Reorder direction inside one column (cards) or one board (columns).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shift {
    Up,
    Down,
    Top,
    Bottom,
}

/// Typed domain failure; surfaces to MCP as `isError` results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoardError {
    EmptyName,
    DuplicateBoard(String),
    BoardNotFound(String),
    DuplicateColumn(String),
    ColumnNotFound(String),
    ColumnNotEmpty(String),
    LastColumn,
    CardNotFound(String),
    WipFull(String),
    TooManyBoards,
    TooManyColumns,
    TooManyCards,
    TitleTooLong,
}

impl std::fmt::Display for BoardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoardError::EmptyName => write!(f, "name must not be empty"),
            BoardError::DuplicateBoard(n) => write!(f, "board already exists: {n}"),
            BoardError::BoardNotFound(n) => write!(f, "board not found: {n}"),
            BoardError::DuplicateColumn(n) => write!(f, "column already exists: {n}"),
            BoardError::ColumnNotFound(n) => write!(f, "column not found: {n}"),
            BoardError::ColumnNotEmpty(n) => {
                write!(f, "column still holds cards, move them first: {n}")
            }
            BoardError::LastColumn => write!(f, "cannot delete the last column"),
            BoardError::CardNotFound(id) => write!(f, "card not found: {id}"),
            BoardError::WipFull(c) => write!(f, "column WIP limit reached: {c}"),
            BoardError::TooManyBoards => write!(f, "too many boards (max {MAX_BOARDS})"),
            BoardError::TooManyColumns => {
                write!(f, "too many columns (max {MAX_COLUMNS_PER_BOARD})")
            }
            BoardError::TooManyCards => {
                write!(f, "too many cards (max {MAX_CARDS_PER_BOARD})")
            }
            BoardError::TitleTooLong => write!(f, "title too long (max {MAX_TITLE_LEN} chars)"),
        }
    }
}

impl std::error::Error for BoardError {}

/// Column declaration for `board_create`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnSpec {
    pub name: String,
    pub wip_limit: u32,
}

impl ColumnSpec {
    pub fn new(name: &str) -> Self {
        ColumnSpec { name: name.to_string(), wip_limit: 0 }
    }

    pub fn with_wip(name: &str, wip_limit: u32) -> Self {
        ColumnSpec { name: name.to_string(), wip_limit }
    }
}

/// Card creation arguments; `column` defaults to the first column.
#[derive(Clone, Debug)]
pub struct CardDraft {
    pub title: String,
    pub column: Option<String>,
    pub description: String,
    pub assignee: Option<String>,
    pub priority: Priority,
    pub tags: Vec<String>,
    pub due_date: Option<String>,
}

impl CardDraft {
    pub fn new(title: &str) -> Self {
        CardDraft {
            title: title.to_string(),
            column: None,
            description: String::new(),
            assignee: None,
            priority: Priority::Normal,
            tags: Vec::new(),
            due_date: None,
        }
    }
}

/// Partial card update; `progress` past 100 clamps instead of failing.
#[derive(Clone, Debug, Default)]
pub struct CardPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub assignee: Option<String>,
    pub clear_assignee: bool,
    pub priority: Option<Priority>,
    pub progress: Option<u16>,
    pub tags: Option<Vec<String>>,
    pub due_date: Option<String>,
    pub clear_due_date: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub id: String,
    pub title: String,
    pub description: String,
    pub column: String,
    pub assignee: String,
    pub tags: Vec<String>,
    pub priority: Priority,
    pub created_at: u64,
    pub updated_at: u64,
    pub due_date: Option<String>,
    pub progress: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub position: usize,
    pub wip_limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Board {
    pub id: String,
    pub name: String,
    pub created_at: u64,
    pub columns: Vec<Column>,
    pub cards: Vec<Card>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn new_id(prefix: char) -> String {
    let mut buf = [0u8; 8];
    let mut ok = false;
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if f.read_exact(&mut buf).is_ok() {
            ok = true;
        }
    }
    if !ok {
        let t = now_secs();
        buf.copy_from_slice(&t.to_le_bytes());
    }
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}-{hex}")
}

fn clean_name(raw: &str) -> Result<String, BoardError> {
    let name = raw.trim().to_string();
    if name.is_empty() {
        return Err(BoardError::EmptyName);
    }
    Ok(name)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

impl Board {
    fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    fn card_index(&self, id: &str) -> Option<usize> {
        self.cards.iter().position(|c| c.id == id)
    }

    /// Cards of one column in stored (position) order.
    pub fn cards_in(&self, column: &str) -> Vec<&Card> {
        self.cards.iter().filter(|c| c.column == column).collect()
    }

    pub fn card(&self, id: &str) -> Option<&Card> {
        self.cards.iter().find(|c| c.id == id)
    }

    pub fn is_last_column(&self, name: &str) -> bool {
        self.columns.last().is_some_and(|c| c.name == name)
    }

    fn check_wip(&self, column: &str) -> Result<(), BoardError> {
        let Some(col) = self.columns.iter().find(|c| c.name == column) else {
            return Err(BoardError::ColumnNotFound(column.to_string()));
        };
        if col.wip_limit > 0 && self.cards_in(column).len() >= col.wip_limit as usize {
            return Err(BoardError::WipFull(column.to_string()));
        }
        Ok(())
    }

    pub fn card_create(&mut self, draft: CardDraft) -> Result<String, BoardError> {
        let title = clean_name(&draft.title)?;
        if title.chars().count() > MAX_TITLE_LEN {
            return Err(BoardError::TitleTooLong);
        }
        if self.cards.len() >= MAX_CARDS_PER_BOARD {
            return Err(BoardError::TooManyCards);
        }
        let column = match draft.column {
            Some(c) => clean_name(&c)?,
            None => self.columns.first().map(|c| c.name.clone()).unwrap_or_default(),
        };
        if self.column_index(&column).is_none() {
            return Err(BoardError::ColumnNotFound(column));
        }
        self.check_wip(&column)?;
        let now = now_secs();
        let id = new_id('c');
        let mut tags: Vec<String> =
            draft.tags.iter().take(MAX_TAGS).map(|t| truncate_chars(t.trim(), MAX_TAG_LEN)).collect();
        tags.retain(|t| !t.is_empty());
        self.cards.push(Card {
            id: id.clone(),
            title,
            description: truncate_chars(&draft.description, MAX_DESCRIPTION_LEN),
            column,
            assignee: draft.assignee.unwrap_or_default().trim().to_string(),
            tags,
            priority: draft.priority,
            created_at: now,
            updated_at: now,
            due_date: draft.due_date.map(|d| d.trim().to_string()).filter(|d| !d.is_empty()),
            progress: 0,
        });
        Ok(id)
    }

    pub fn card_move(&mut self, id: &str, column: &str) -> Result<(), BoardError> {
        let target = clean_name(column)?;
        if self.column_index(&target).is_none() {
            return Err(BoardError::ColumnNotFound(target));
        }
        let Some(i) = self.card_index(id) else {
            return Err(BoardError::CardNotFound(id.to_string()));
        };
        if self.cards[i].column == target {
            return Ok(());
        }
        self.check_wip(&target)?;
        let last = self.is_last_column(&target);
        self.cards[i].column = target;
        if last {
            self.cards[i].progress = 100;
        }
        self.cards[i].updated_at = now_secs();
        Ok(())
    }

    pub fn card_update(&mut self, id: &str, patch: CardPatch) -> Result<(), BoardError> {
        let Some(i) = self.card_index(id) else {
            return Err(BoardError::CardNotFound(id.to_string()));
        };
        if let Some(title) = patch.title {
            let title = clean_name(&title)?;
            if title.chars().count() > MAX_TITLE_LEN {
                return Err(BoardError::TitleTooLong);
            }
            self.cards[i].title = title;
        }
        if let Some(d) = patch.description {
            self.cards[i].description = truncate_chars(&d, MAX_DESCRIPTION_LEN);
        }
        if patch.clear_assignee {
            self.cards[i].assignee.clear();
        } else if let Some(a) = patch.assignee {
            self.cards[i].assignee = a.trim().to_string();
        }
        if let Some(p) = patch.priority {
            self.cards[i].priority = p;
        }
        if let Some(p) = patch.progress {
            self.cards[i].progress = p.min(100) as u8;
        }
        if patch.clear_due_date {
            self.cards[i].due_date = None;
        } else if let Some(d) = patch.due_date {
            let d = d.trim().to_string();
            self.cards[i].due_date = if d.is_empty() { None } else { Some(d) };
        }
        if let Some(tags) = patch.tags {
            let mut kept: Vec<String> =
                tags.iter().take(MAX_TAGS).map(|t| truncate_chars(t.trim(), MAX_TAG_LEN)).collect();
            kept.retain(|t| !t.is_empty());
            self.cards[i].tags = kept;
        }
        self.cards[i].updated_at = now_secs();
        Ok(())
    }

    pub fn card_delete(&mut self, id: &str) -> Result<(), BoardError> {
        let Some(i) = self.card_index(id) else {
            return Err(BoardError::CardNotFound(id.to_string()));
        };
        self.cards.remove(i);
        Ok(())
    }

    /// Assign a card; `None` falls back to `FORGE_SESSION_NAME`, then
    /// `unknown`. Moves to Doing when that column exists and has room —
    /// a full Doing keeps the card where it is instead of failing.
    pub fn card_assign(&mut self, id: &str, assignee: Option<&str>) -> Result<(), BoardError> {
        let Some(i) = self.card_index(id) else {
            return Err(BoardError::CardNotFound(id.to_string()));
        };
        let who = match assignee {
            Some(a) if !a.trim().is_empty() => a.trim().to_string(),
            _ => std::env::var("FORGE_SESSION_NAME")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "unknown".to_string()),
        };
        self.cards[i].assignee = who;
        self.cards[i].updated_at = now_secs();
        if self.cards[i].column != "Doing" && self.column_index("Doing").is_some() {
            let _ = self.card_move(id, "Doing");
        }
        Ok(())
    }

    /// Reorder one card inside its own column; edges are no-ops.
    pub fn shift_card(&mut self, id: &str, dir: Shift) -> Result<(), BoardError> {
        let Some(i) = self.card_index(id) else {
            return Err(BoardError::CardNotFound(id.to_string()));
        };
        let column = self.cards[i].column.clone();
        let members: Vec<usize> =
            self.cards.iter().enumerate().filter(|(_, c)| c.column == column).map(|(n, _)| n).collect();
        let Some(pos) = members.iter().position(|&n| n == i) else {
            return Err(BoardError::CardNotFound(id.to_string()));
        };
        let last = members.len() - 1;
        let dest = match dir {
            Shift::Up => pos.saturating_sub(1),
            Shift::Down => (pos + 1).min(last),
            Shift::Top => 0,
            Shift::Bottom => last,
        };
        if dest == pos {
            return Ok(());
        }
        let card = self.cards.remove(i);
        // After removal, later globals shift down by one, so inserting at
        // the destination member's old global index lands past it.
        let at = members[dest];
        self.cards.insert(at, card);
        Ok(())
    }

    pub fn column_add(&mut self, name: &str, wip_limit: u32) -> Result<(), BoardError> {
        let name = clean_name(name)?;
        if self.column_index(&name).is_some() {
            return Err(BoardError::DuplicateColumn(name));
        }
        if self.columns.len() >= MAX_COLUMNS_PER_BOARD {
            return Err(BoardError::TooManyColumns);
        }
        let position = self.columns.len();
        self.columns.push(Column { name, position, wip_limit });
        Ok(())
    }

    pub fn column_rename(&mut self, old: &str, new_name: &str) -> Result<(), BoardError> {
        let new_name = clean_name(new_name)?;
        let Some(i) = self.column_index(old) else {
            return Err(BoardError::ColumnNotFound(old.to_string()));
        };
        if old != new_name && self.column_index(&new_name).is_some() {
            return Err(BoardError::DuplicateColumn(new_name));
        }
        self.columns[i].name = new_name.clone();
        for card in self.cards.iter_mut().filter(|c| c.column == old) {
            card.column = new_name.clone();
        }
        Ok(())
    }

    /// Deleting a column with cards fails; delete or move them first.
    /// The final column cannot be deleted.
    pub fn column_delete(&mut self, name: &str) -> Result<(), BoardError> {
        let Some(i) = self.column_index(name) else {
            return Err(BoardError::ColumnNotFound(name.to_string()));
        };
        if self.columns.len() == 1 {
            return Err(BoardError::LastColumn);
        }
        if self.cards.iter().any(|c| c.column == name) {
            return Err(BoardError::ColumnNotEmpty(name.to_string()));
        }
        self.columns.remove(i);
        for (n, col) in self.columns.iter_mut().enumerate() {
            col.position = n;
        }
        Ok(())
    }

    pub fn column_set_wip(&mut self, name: &str, wip_limit: u32) -> Result<(), BoardError> {
        let Some(i) = self.column_index(name) else {
            return Err(BoardError::ColumnNotFound(name.to_string()));
        };
        self.columns[i].wip_limit = wip_limit;
        Ok(())
    }

    pub fn column_shift(&mut self, name: &str, dir: Shift) -> Result<(), BoardError> {
        let Some(i) = self.column_index(name) else {
            return Err(BoardError::ColumnNotFound(name.to_string()));
        };
        let last = self.columns.len() - 1;
        let dest = match dir {
            Shift::Up => i.saturating_sub(1),
            Shift::Down => (i + 1).min(last),
            Shift::Top => 0,
            Shift::Bottom => last,
        };
        if dest == i {
            return Ok(());
        }
        let col = self.columns.remove(i);
        self.columns.insert(dest, col);
        for (n, col) in self.columns.iter_mut().enumerate() {
            col.position = n;
        }
        Ok(())
    }

    /// Per-column card counts in column order, for `board_list`.
    pub fn column_counts(&self) -> Vec<(String, usize)> {
        self.columns
            .iter()
            .map(|c| (c.name.clone(), self.cards_in(&c.name).len()))
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoardStore {
    pub boards: Vec<Board>,
}

impl BoardStore {
    pub fn new() -> Self {
        BoardStore { boards: Vec::new() }
    }

    pub fn board(&self, name_or_id: &str) -> Option<&Board> {
        self.boards.iter().find(|b| b.name == name_or_id || b.id == name_or_id)
    }

    pub fn board_mut(&mut self, name_or_id: &str) -> Option<&mut Board> {
        self.boards.iter_mut().find(|b| b.name == name_or_id || b.id == name_or_id)
    }

    pub fn board_names(&self) -> Vec<String> {
        self.boards.iter().map(|b| b.name.clone()).collect()
    }

    pub fn board_create(
        &mut self,
        name: &str,
        columns: Option<Vec<ColumnSpec>>,
    ) -> Result<(), BoardError> {
        let name = clean_name(name)?;
        if self.boards.iter().any(|b| b.name == name) {
            return Err(BoardError::DuplicateBoard(name));
        }
        if self.boards.len() >= MAX_BOARDS {
            return Err(BoardError::TooManyBoards);
        }
        let specs = match columns {
            Some(specs) if !specs.is_empty() => specs,
            _ => default_columns(),
        };
        if specs.len() > MAX_COLUMNS_PER_BOARD {
            return Err(BoardError::TooManyColumns);
        }
        let mut columns = Vec::with_capacity(specs.len());
        for (n, spec) in specs.iter().enumerate() {
            let spec_name = clean_name(&spec.name)?;
            if columns.iter().any(|c: &Column| c.name == spec_name) {
                return Err(BoardError::DuplicateColumn(spec_name));
            }
            columns.push(Column { name: spec_name, position: n, wip_limit: spec.wip_limit });
        }
        let now = now_secs();
        self.boards.push(Board {
            id: new_id('b'),
            name,
            created_at: now,
            columns,
            cards: Vec::new(),
        });
        Ok(())
    }

    pub fn board_rename(&mut self, old: &str, new_name: &str) -> Result<(), BoardError> {
        let new_name = clean_name(new_name)?;
        let Some(i) = self.boards.iter().position(|b| b.name == old || b.id == old) else {
            return Err(BoardError::BoardNotFound(old.to_string()));
        };
        if self.boards.iter().enumerate().any(|(n, b)| n != i && b.name == new_name) {
            return Err(BoardError::DuplicateBoard(new_name));
        }
        self.boards[i].name = new_name;
        Ok(())
    }

    pub fn board_delete(&mut self, name_or_id: &str) -> Result<(), BoardError> {
        let Some(i) =
            self.boards.iter().position(|b| b.name == name_or_id || b.id == name_or_id)
        else {
            return Err(BoardError::BoardNotFound(name_or_id.to_string()));
        };
        self.boards.remove(i);
        Ok(())
    }
}

/// Save schema version written by [`BoardStore::to_json_string`].
pub const SAVE_VERSION: u64 = 1;

fn json_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn json_u64(v: &serde_json::Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn parse_card(raw: &serde_json::Value) -> Option<Card> {
    let id = json_str(raw, "id").filter(|s| !s.trim().is_empty())?;
    let title = json_str(raw, "title").filter(|s| !s.trim().is_empty())?;
    let column = json_str(raw, "column").filter(|s| !s.trim().is_empty())?;
    let tags = raw
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str())
                .map(|t| truncate_chars(t.trim(), MAX_TAG_LEN))
                .filter(|t| !t.is_empty())
                .take(MAX_TAGS)
                .collect()
        })
        .unwrap_or_default();
    Some(Card {
        id,
        title: truncate_chars(&title, MAX_TITLE_LEN),
        description: json_str(raw, "description")
            .map(|d| truncate_chars(&d, MAX_DESCRIPTION_LEN))
            .unwrap_or_default(),
        column,
        assignee: json_str(raw, "assignee").unwrap_or_default(),
        tags,
        priority: raw.get("priority").and_then(|p| p.as_str()).map(Priority::parse).unwrap_or(Priority::Normal),
        created_at: json_u64(raw, "created_at"),
        updated_at: json_u64(raw, "updated_at"),
        due_date: json_str(raw, "due_date").filter(|d| !d.trim().is_empty()),
        progress: raw.get("progress").and_then(|p| p.as_u64()).unwrap_or(0).min(100) as u8,
    })
}

impl BoardStore {
    /// Serialize the whole store; atomic writers persist the text.
    pub fn to_json_string(&self) -> String {
        let boards: Vec<serde_json::Value> = self
            .boards
            .iter()
            .map(|b| {
                serde_json::json!({
                    "id": b.id,
                    "name": b.name,
                    "created_at": b.created_at,
                    "columns": b.columns.iter().map(|c| {
                        serde_json::json!({
                            "name": c.name,
                            "position": c.position,
                            "wip_limit": c.wip_limit,
                        })
                    }).collect::<Vec<_>>(),
                    "cards": b.cards.iter().map(|c| {
                        serde_json::json!({
                            "id": c.id,
                            "title": c.title,
                            "description": c.description,
                            "column": c.column,
                            "assignee": c.assignee,
                            "tags": c.tags,
                            "priority": c.priority.as_str(),
                            "created_at": c.created_at,
                            "updated_at": c.updated_at,
                            "due_date": c.due_date,
                            "progress": c.progress,
                        })
                    }).collect::<Vec<_>>(),
                })
            })
            .collect();
        serde_json::json!({ "v": SAVE_VERSION, "boards": boards }).to_string()
    }

    /// Parse a save file. Structural damage (bad JSON, wrong version,
    /// missing boards, nameless board) is an error so the caller can
    /// quarantine and start fresh; per-card oddities clamp or drop the
    /// card instead of failing the whole file.
    pub fn from_json_str(text: &str) -> Result<BoardStore, String> {
        let root: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("kanban save is not JSON: {e}"))?;
        let version = root.get("v").and_then(|v| v.as_u64()).ok_or("kanban save missing version")?;
        if version != SAVE_VERSION {
            return Err(format!("unsupported kanban save version: {version}"));
        }
        let raw_boards =
            root.get("boards").and_then(|b| b.as_array()).ok_or("kanban save missing boards")?;
        let mut store = BoardStore::new();
        for raw in raw_boards.iter().take(MAX_BOARDS) {
            let id = json_str(raw, "id").filter(|s| !s.trim().is_empty()).ok_or("kanban board missing id")?;
            let name = json_str(raw, "name").filter(|s| !s.trim().is_empty()).ok_or("kanban board missing name")?;
            let raw_cols =
                raw.get("columns").and_then(|c| c.as_array()).ok_or("kanban board missing columns")?;
            if raw_cols.is_empty() {
                return Err(format!("kanban board has no columns: {name}"));
            }
            let mut columns = Vec::new();
            for (n, rc) in raw_cols.iter().take(MAX_COLUMNS_PER_BOARD).enumerate() {
                let Some(col_name) =
                    json_str(rc, "name").filter(|s| !s.trim().is_empty())
                else {
                    continue;
                };
                if columns.iter().any(|c: &Column| c.name == col_name) {
                    continue;
                }
                columns.push(Column {
                    name: col_name,
                    position: n,
                    wip_limit: rc.get("wip_limit").and_then(|w| w.as_u64()).unwrap_or(0) as u32,
                });
            }
            if columns.is_empty() {
                return Err(format!("kanban board has no usable columns: {name}"));
            }
            let mut board = Board {
                id,
                name,
                created_at: json_u64(raw, "created_at"),
                columns,
                cards: Vec::new(),
            };
            if let Some(raw_cards) = raw.get("cards").and_then(|c| c.as_array()) {
                for rc in raw_cards.iter().take(MAX_CARDS_PER_BOARD) {
                    let Some(card) = parse_card(rc) else { continue };
                    if board.column_index(&card.column).is_none() {
                        continue;
                    }
                    if board.card_index(&card.id).is_some() {
                        continue;
                    }
                    board.cards.push(card);
                }
            }
            store.boards.push(board);
        }
        Ok(store)
    }
}

/// Clamp a list selection after deletion: empty stays 0, an index past
/// the end drops to the last row instead of pointing nowhere.
pub fn clamp_index(len: usize, selected: usize) -> usize {
    if len == 0 {
        0
    } else {
        selected.min(len - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> BoardStore {
        BoardStore::new()
    }

    fn three_in_doing(s: &mut BoardStore) {
        s.board_create("team", None).unwrap();
        for title in ["one", "two", "three"] {
            let id = s
                .board_mut("team")
                .unwrap()
                .card_create(CardDraft::new(title))
                .unwrap();
            s.board_mut("team")
                .unwrap()
                .card_move(&id, "Doing")
                .unwrap();
        }
    }

    #[test]
    fn new_board_has_default_columns() {
        let mut s = store();
        s.board_create("team", None).unwrap();
        let b = s.board("team").unwrap();
        let names: Vec<&str> = b.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Backlog", "Todo", "Doing", "Done"]);
        assert_eq!(b.columns[2].wip_limit, 3);
    }

    #[test]
    fn board_create_rejects_duplicate_name() {
        let mut s = store();
        s.board_create("team", None).unwrap();
        let err = s.board_create("team", None).unwrap_err();
        assert_eq!(err, BoardError::DuplicateBoard("team".to_string()));
    }

    #[test]
    fn board_create_rejects_empty_name() {
        let mut s = store();
        let err = s.board_create("  ", None).unwrap_err();
        assert_eq!(err, BoardError::EmptyName);
    }

    #[test]
    fn board_create_honors_custom_columns() {
        let mut s = store();
        s.board_create(
            "flow",
            Some(vec![
                ColumnSpec::new("Inbox"),
                ColumnSpec::with_wip("Active", 2),
                ColumnSpec::new("Shipped"),
            ]),
        )
        .unwrap();
        let b = s.board("flow").unwrap();
        let names: Vec<&str> = b.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Inbox", "Active", "Shipped"]);
        assert_eq!(b.columns[1].wip_limit, 2);
    }

    #[test]
    fn card_move_into_full_wip_column_fails() {
        let mut s = store();
        three_in_doing(&mut s);
        let extra = s
            .board_mut("team")
            .unwrap()
            .card_create(CardDraft::new("fourth"))
            .unwrap();
        let err = s
            .board_mut("team")
            .unwrap()
            .card_move(&extra, "Doing")
            .unwrap_err();
        assert_eq!(err, BoardError::WipFull("Doing".to_string()));
        // The card stays where it was.
        assert_eq!(s.board("team").unwrap().card(&extra).unwrap().column, "Backlog");
    }

    #[test]
    fn card_move_into_last_column_sets_progress_100() {
        let mut s = store();
        s.board_create("team", None).unwrap();
        let id = s
            .board_mut("team")
            .unwrap()
            .card_create(CardDraft::new("ship it"))
            .unwrap();
        s.board_mut("team").unwrap().card_move(&id, "Done").unwrap();
        assert_eq!(s.board("team").unwrap().card(&id).unwrap().progress, 100);
    }

    #[test]
    fn card_update_progress_clamped() {
        let mut s = store();
        s.board_create("team", None).unwrap();
        let id = s
            .board_mut("team")
            .unwrap()
            .card_create(CardDraft::new("half"))
            .unwrap();
        s.board_mut("team")
            .unwrap()
            .card_update(&id, CardPatch { progress: Some(250), ..Default::default() })
            .unwrap();
        assert_eq!(s.board("team").unwrap().card(&id).unwrap().progress, 100);
    }

    #[test]
    fn card_assign_defaults_and_moves_to_doing() {
        let mut s = store();
        s.board_create("team", None).unwrap();
        let id = s
            .board_mut("team")
            .unwrap()
            .card_create(CardDraft::new("pick up"))
            .unwrap();
        s.board_mut("team").unwrap().card_assign(&id, None).unwrap();
        let card = s.board("team").unwrap().card(&id).unwrap();
        assert!(!card.assignee.is_empty(), "assignee must default, not stay empty");
        assert_eq!(card.column, "Doing");
    }

    #[test]
    fn shift_card_reorders_within_column() {
        let mut s = store();
        s.board_create("team", None).unwrap();
        let mut ids = Vec::new();
        for title in ["a", "b", "c"] {
            ids.push(
                s.board_mut("team")
                    .unwrap()
                    .card_create(CardDraft::new(title))
                    .unwrap(),
            );
        }
        let order = |s: &BoardStore| {
            s.board("team")
                .unwrap()
                .cards_in("Backlog")
                .iter()
                .map(|c| c.title.clone())
                .collect::<Vec<_>>()
        };
        s.board_mut("team").unwrap().shift_card(&ids[2], Shift::Up).unwrap();
        assert_eq!(order(&s), vec!["a", "c", "b"]);
        s.board_mut("team").unwrap().shift_card(&ids[2], Shift::Top).unwrap();
        assert_eq!(order(&s), vec!["c", "a", "b"]);
        s.board_mut("team").unwrap().shift_card(&ids[2], Shift::Bottom).unwrap();
        assert_eq!(order(&s), vec!["a", "b", "c"]);
    }

    #[test]
    fn clamp_index_handles_deletion_under_selection() {
        assert_eq!(clamp_index(0, 0), 0);
        assert_eq!(clamp_index(0, 5), 0);
        assert_eq!(clamp_index(3, 1), 1);
        assert_eq!(clamp_index(3, 7), 2);
    }

    #[test]
    fn column_delete_with_cards_fails() {
        let mut s = store();
        s.board_create("team", None).unwrap();
        s.board_mut("team")
            .unwrap()
            .card_create(CardDraft::new("stuck"))
            .unwrap();
        let err = s.board_mut("team").unwrap().column_delete("Backlog").unwrap_err();
        assert_eq!(err, BoardError::ColumnNotEmpty("Backlog".to_string()));
    }

    #[test]
    fn priority_cycles() {
        assert_eq!(Priority::Normal.cycle(), Priority::High);
        assert_eq!(Priority::Urgent.cycle(), Priority::Low);
    }

    fn sample_store() -> BoardStore {
        let mut s = store();
        s.board_create(
            "team",
            Some(vec![ColumnSpec::new("Inbox"), ColumnSpec::with_wip("Active", 2), ColumnSpec::new("Shipped")]),
        )
        .unwrap();
        let mut draft = CardDraft::new("fix leak");
        draft.column = Some("Active".to_string());
        draft.description = "torn write <&>".to_string();
        draft.assignee = Some("kins".to_string());
        draft.priority = Priority::High;
        draft.tags = vec!["urgent".to_string(), "pty".to_string()];
        draft.due_date = Some("2026-10-01".to_string());
        let id = s.board_mut("team").unwrap().card_create(draft).unwrap();
        s.board_mut("team")
            .unwrap()
            .card_update(&id, CardPatch { progress: Some(40), ..Default::default() })
            .unwrap();
        s
    }

    #[test]
    fn json_round_trip_preserves_everything_in_order() {
        let s = sample_store();
        let text = s.to_json_string();
        let back = BoardStore::from_json_str(&text).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn from_json_rejects_garbage() {
        assert!(BoardStore::from_json_str("not json{{{").is_err());
        assert!(BoardStore::from_json_str("").is_err());
        assert!(BoardStore::from_json_str(r#"{"v":1}"#).is_err());
    }

    #[test]
    fn from_json_clamps_and_defaults_fail_soft() {
        let text = serde_json::json!({
            "v": 1,
            "boards": [{
                "id": "b-1", "name": "t", "created_at": 1,
                "columns": [{"name": "A", "position": 0, "wip_limit": 0}],
                "cards": [{
                    "id": "c-1", "title": "x", "description": "",
                    "column": "A", "assignee": "", "tags": [],
                    "priority": "weird", "created_at": 1, "updated_at": 1,
                    "due_date": serde_json::Value::Null, "progress": 250
                }]
            }]
        })
        .to_string();
        let back = BoardStore::from_json_str(&text).unwrap();
        let card = back.board("t").unwrap().card("c-1").unwrap();
        assert_eq!(card.progress, 100);
        assert_eq!(card.priority, Priority::Normal);
    }
}
