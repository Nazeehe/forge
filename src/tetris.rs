//! Sidebar Tetris: pure game logic, no rendering or I/O.
//!
//! The sidebar owns the toggle and the tick; this module only advances
//! the well. Rendering lives with the sidebar paint so geometry and
//! hit-testing stay in one place.

/// Well width in cells: fits the narrowest usable sidebar.
pub const WIDTH: usize = 10;
/// Well height in cells: the paint clips this to the list region.
pub const HEIGHT: usize = 20;

/// One of the seven tetrominoes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PieceKind {
    I,
    O,
    T,
    S,
    Z,
    J,
    L,
}

impl PieceKind {
    const ALL: [PieceKind; 7] = [
        PieceKind::I,
        PieceKind::O,
        PieceKind::T,
        PieceKind::S,
        PieceKind::Z,
        PieceKind::J,
        PieceKind::L,
    ];

    /// Spawn-orientation cells, origin near the piece center.
    fn cells(self) -> [(i8, i8); 4] {
        match self {
            PieceKind::I => [(-2, 0), (-1, 0), (0, 0), (1, 0)],
            PieceKind::O => [(0, 0), (1, 0), (0, 1), (1, 1)],
            PieceKind::T => [(-1, 0), (0, 0), (1, 0), (0, 1)],
            PieceKind::S => [(0, 0), (1, 0), (-1, 1), (0, 1)],
            PieceKind::Z => [(-1, 0), (0, 0), (0, 1), (1, 1)],
            PieceKind::J => [(-1, 0), (0, 0), (1, 0), (-1, 1)],
            PieceKind::L => [(-1, 0), (0, 0), (1, 0), (1, 1)],
        }
    }
}

/// Falling piece: shape cells relative to `(x, y)`. Cells may sit above
/// the well (`y < 0`); the paint clips them, locking them ends the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Active {
    kind: PieceKind,
    cells: [(i8, i8); 4],
    x: i8,
    y: i8,
}

/// Sidebar Tetris state. Clone is cheap (one 200-cell well) and the
/// sidebar snapshot clones it once per frame like the fleet rows.
#[derive(Clone, Debug)]
pub struct TetrisGame {
    settled: [[Option<PieceKind>; WIDTH]; HEIGHT],
    active: Active,
    bag: Vec<PieceKind>,
    rng: u64,
    score: u64,
    lines: u32,
    paused: bool,
    over: bool,
}

impl TetrisGame {
    /// New game with a time-seeded bag.
    pub fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(0x9E37_79B9_7F4A_7C15);
        Self::with_seed(seed)
    }

    fn with_seed(seed: u64) -> Self {
        let mut game = TetrisGame {
            settled: [[None; WIDTH]; HEIGHT],
            active: Active {
                kind: PieceKind::T,
                cells: PieceKind::T.cells(),
                x: 4,
                y: 0,
            },
            bag: Vec::new(),
            rng: seed | 1,
            score: 0,
            lines: 0,
            paused: false,
            over: false,
        };
        game.spawn();
        game
    }

    /// Test hook: replace the falling piece with `kind` at the spawn.
    #[cfg(test)]
    fn force_spawn(kind: PieceKind) -> Self {
        let mut game = Self::with_seed(1);
        game.active = Active {
            kind,
            cells: kind.cells(),
            x: 4,
            y: 0,
        };
        game.over = false;
        game
    }

    /// xorshift64*: tiny deterministic shuffle source, no dependency.
    fn rand_below(&mut self, n: usize) -> usize {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng % n as u64) as usize
    }

    /// 7-bag: every seven spawns contain each piece exactly once.
    fn next_kind(&mut self) -> PieceKind {
        if self.bag.is_empty() {
            self.bag = PieceKind::ALL.to_vec();
            for i in (1..self.bag.len()).rev() {
                let j = self.rand_below(i + 1);
                self.bag.swap(i, j);
            }
        }
        self.bag.pop().expect("shuffled bag is never empty")
    }

    fn spawn(&mut self) {
        let kind = self.next_kind();
        self.active = Active {
            kind,
            cells: kind.cells(),
            x: 4,
            y: 0,
        };
        if self.collides(&self.active) {
            self.over = true;
        }
    }

    fn at_valid(&self, x: i8, y: i8) -> bool {
        if x < 0 || x >= WIDTH as i8 || y >= HEIGHT as i8 {
            return false;
        }
        if y < 0 {
            return true;
        }
        self.settled[y as usize][x as usize].is_none()
    }

    fn collides(&self, piece: &Active) -> bool {
        piece
            .cells
            .iter()
            .any(|(dx, dy)| !self.at_valid(piece.x + dx, piece.y + dy))
    }

    /// Absolute board cells of the falling piece, top-to-bottom.
    pub fn active_cells(&self) -> Vec<(i8, i8)> {
        let mut out: Vec<(i8, i8)> = self
            .active
            .cells
            .iter()
            .map(|(dx, dy)| (self.active.x + dx, self.active.y + dy))
            .collect();
        out.sort_unstable();
        out
    }

    /// Active piece kind plus its absolute cells, for the paint.
    pub fn active(&self) -> (PieceKind, Vec<(i8, i8)>) {
        (self.active.kind, self.active_cells())
    }

    /// Settled well, row 0 at the top.
    pub fn settled(&self) -> &[[Option<PieceKind>; WIDTH]; HEIGHT] {
        &self.settled
    }

    pub fn score(&self) -> u64 {
        self.score
    }

    pub fn lines(&self) -> u32 {
        self.lines
    }

    /// Level 1-based: every ten lines speed gravity up.
    pub fn level(&self) -> u32 {
        1 + self.lines / 10
    }

    pub fn is_over(&self) -> bool {
        self.over
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Gravity interval in milliseconds for the current level.
    pub fn drop_interval_ms(&self) -> u64 {
        800u64.saturating_sub((self.level().saturating_sub(1) as u64) * 70).max(90)
    }

    /// Try shifting the piece; false (frozen or blocked) moves nothing.
    pub fn try_move(&mut self, dx: i8, dy: i8) -> bool {
        if self.over || self.paused {
            return false;
        }
        let next = Active {
            x: self.active.x + dx,
            y: self.active.y + dy,
            ..self.active
        };
        if self.collides(&next) {
            return false;
        }
        self.active = next;
        true
    }

    /// Try a clockwise turn with a small kick table; false moves nothing.
    pub fn rotate_cw(&mut self) -> bool {
        if self.over || self.paused {
            return false;
        }
        let turned: [(i8, i8); 4] = self.active.cells.map(|(x, y)| (-y, x));
        for (kx, ky) in [(0, 0), (-1, 0), (1, 0), (-2, 0), (2, 0), (0, -1)] {
            let next = Active {
                cells: turned,
                x: self.active.x + kx,
                y: self.active.y + ky,
                ..self.active
            };
            if !self.collides(&next) {
                self.active = next;
                return true;
            }
        }
        false
    }

    /// One gravity tick: fall, or lock and respawn at the stack.
    pub fn step(&mut self) {
        if self.over || self.paused {
            return;
        }
        if !self.try_move(0, 1) {
            self.lock();
        }
    }

    /// Instant drop to the stack, then lock and respawn.
    pub fn hard_drop(&mut self) {
        if self.over || self.paused {
            return;
        }
        while self.try_move(0, 1) {}
        self.lock();
    }

    /// Pause or resume; the well is untouched.
    pub fn toggle_pause(&mut self) {
        if !self.over {
            self.paused = !self.paused;
        }
    }

    /// Fresh well, zero score, new bag. A blocked spawn ends it at once.
    pub fn restart(&mut self) {
        *self = Self::with_seed(self.rng ^ 0xD1B5_4E32_2C4E_69B1);
    }

    /// Settle the piece, clear full rows, respawn. Locking at the very
    /// top (or spawning into a stack) ends the game.
    fn lock(&mut self) {
        let mut topped = false;
        for (dx, dy) in self.active.cells {
            let (x, y) = (self.active.x + dx, self.active.y + dy);
            if y < 0 {
                topped = true;
                continue;
            }
            self.settled[y as usize][x as usize] = Some(self.active.kind);
            if y == 0 {
                topped = true;
            }
        }
        let cleared = self.clear_lines();
        self.score += [0, 100, 300, 500, 800][cleared.min(4)] * self.level() as u64;
        self.lines += cleared as u32;
        if topped {
            self.over = true;
            return;
        }
        self.spawn();
    }

    /// Remove full rows, drop the rest down. Returns rows cleared.
    fn clear_lines(&mut self) -> usize {
        let before = self.settled;
        let mut kept: Vec<[Option<PieceKind>; WIDTH]> = before
            .iter()
            .filter(|row| row.iter().any(|c| c.is_none()))
            .copied()
            .collect();
        let cleared = HEIGHT - kept.len();
        while kept.len() < HEIGHT {
            kept.insert(0, [None; WIDTH]);
        }
        kept.into_iter().enumerate().for_each(|(i, row)| self.settled[i] = row);
        cleared
    }
}

impl Default for TetrisGame {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_game_is_live_with_empty_well() {
        let game = TetrisGame::new();
        assert!(!game.is_over(), "fresh game is live");
        assert!(!game.is_paused(), "fresh game is unpaused");
        assert_eq!(game.score(), 0);
        assert_eq!(game.lines(), 0);
        assert_eq!(game.level(), 1);
        assert!(
            game.settled.iter().flatten().all(|c| c.is_none()),
            "well starts empty"
        );
        assert!(!game.active_cells().is_empty(), "a piece is active");
    }

    #[test]
    fn move_blocked_at_walls() {
        let mut game = TetrisGame::new();
        for _ in 0..WIDTH + 2 {
            game.try_move(-1, 0);
        }
        let before = game.active_cells();
        assert!(!game.try_move(-1, 0), "left wall blocks");
        assert_eq!(game.active_cells(), before, "blocked move changes nothing");
        for _ in 0..2 * WIDTH + 4 {
            game.try_move(1, 0);
        }
        let before = game.active_cells();
        assert!(!game.try_move(1, 0), "right wall blocks");
        assert_eq!(game.active_cells(), before, "blocked move changes nothing");
    }

    #[test]
    fn rotation_cycles_back_to_start() {
        let mut game = TetrisGame::force_spawn(PieceKind::T);
        let start = game.active_cells();
        for _ in 0..4 {
            assert!(game.rotate_cw(), "open well allows rotation");
        }
        assert_eq!(game.active_cells(), start, "four turns restore the piece");
    }

    #[test]
    fn gravity_locks_piece_and_respawns() {
        let mut game = TetrisGame::new();
        game.hard_drop();
        assert!(
            game.settled.iter().flatten().any(|c| c.is_some()),
            "dropped piece locks into the well"
        );
        assert!(!game.is_over(), "first lock never ends the game");
        assert!(!game.active_cells().is_empty(), "a new piece spawns");
    }

    #[test]
    fn full_row_clears_and_scores() {
        let mut game = TetrisGame::new();
        game.settled[HEIGHT - 1] = [Some(PieceKind::I); WIDTH];
        game.hard_drop();
        assert_eq!(game.lines(), 1, "one full row clears");
        assert!(game.score() > 0, "clears score");
        assert!(
            game.settled.iter().flatten().filter(|c| c.is_some()).count() < WIDTH + 4,
            "cleared cells leave the well"
        );
    }

    #[test]
    fn paused_game_ignores_gravity() {
        let mut game = TetrisGame::new();
        game.toggle_pause();
        assert!(game.is_paused());
        let before = game.active_cells();
        for _ in 0..HEIGHT + 2 {
            game.step();
        }
        assert_eq!(game.active_cells(), before, "paused pieces never fall");
        game.toggle_pause();
        assert!(!game.is_paused());
    }

    #[test]
    fn spawn_blocked_is_game_over() {
        let mut game = TetrisGame::new();
        for row in game.settled.iter_mut().skip(1) {
            *row = [Some(PieceKind::Z); WIDTH];
        }
        game.hard_drop();
        assert!(game.is_over(), "stack reaching the top ends the game");
        let before = game.active_cells();
        game.step();
        assert_eq!(game.active_cells(), before, "over pieces never fall");
    }

    #[test]
    fn restart_resets_score_and_well() {
        let mut game = TetrisGame::new();
        game.hard_drop();
        game.restart();
        assert_eq!(game.score(), 0);
        assert_eq!(game.lines(), 0);
        assert!(!game.is_over());
        assert!(
            game.settled.iter().flatten().all(|c| c.is_none()),
            "restart empties the well"
        );
    }
}
