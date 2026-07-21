//! `sw-world` — the spatial grid and area-of-interest (AoI) logic.
//!
//! Pure and deterministic: no I/O, no networking, no protocol types. The server
//! maps world XZ positions onto a coarse grid, keeps an entity index per cell,
//! and drives per-player [`Subscription`]s that emit an [`AoiUpdate`] (cells
//! added / removed) only when a player crosses a cell boundary.

use std::collections::{HashMap, HashSet};

/// Entity identifier as used on the wire (player id or boat id).
pub type EntityId = u64;

/// Default AoI radius in cells: a radius-2 block is a 5x5 neighbourhood.
pub const AOI_RADIUS_CELLS: i32 = 2;

/// An integer grid cell keyed by its `(cx, cz)` coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cell {
    pub cx: i32,
    pub cz: i32,
}

impl Cell {
    pub fn new(cx: i32, cz: i32) -> Cell {
        Cell { cx, cz }
    }

    /// Chebyshev (king-move) distance — the AoI block is a square of this radius.
    pub fn chebyshev_distance(self, other: Cell) -> i32 {
        (self.cx - other.cx).abs().max((self.cz - other.cz).abs())
    }
}

/// The world grid: a single cell size in metres.
#[derive(Debug, Clone, Copy)]
pub struct Grid {
    pub cell_size_m: f32,
}

impl Grid {
    /// Cell size used by the server and advertised in the capability manifest.
    pub const DEFAULT_CELL_SIZE_M: f32 = 1024.0;

    pub fn new(cell_size_m: f32) -> Grid {
        assert!(cell_size_m > 0.0, "cell size must be positive");
        Grid { cell_size_m }
    }

    /// Map a world XZ position to its containing cell.
    pub fn cell_of(self, x: f32, z: f32) -> Cell {
        Cell {
            cx: (x / self.cell_size_m).floor() as i32,
            cz: (z / self.cell_size_m).floor() as i32,
        }
    }
}

impl Default for Grid {
    fn default() -> Grid {
        Grid {
            cell_size_m: Grid::DEFAULT_CELL_SIZE_M,
        }
    }
}

/// All cells within Chebyshev `radius` of `center` (a `(2*radius+1)` square).
pub fn cells_in_radius(center: Cell, radius: i32) -> Vec<Cell> {
    let side = (2 * radius + 1) as usize;
    let mut cells = Vec::with_capacity(side * side);
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            cells.push(Cell {
                cx: center.cx + dx,
                cz: center.cz + dz,
            });
        }
    }
    cells
}

/// The delta a player's interest set undergoes when it recentres.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AoiUpdate {
    pub added: Vec<Cell>,
    pub removed: Vec<Cell>,
}

impl AoiUpdate {
    /// True when nothing changed (the player stayed in the same cell).
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// A single player's area-of-interest subscription.
#[derive(Debug, Clone)]
pub struct Subscription {
    radius: i32,
    center: Option<Cell>,
    cells: HashSet<Cell>,
}

impl Subscription {
    pub fn new(radius: i32) -> Subscription {
        assert!(radius >= 0, "radius must be non-negative");
        Subscription {
            radius,
            center: None,
            cells: HashSet::new(),
        }
    }

    /// The player's current centre cell, if it has ever been placed.
    pub fn center(&self) -> Option<Cell> {
        self.center
    }

    /// The full set of cells the player is currently subscribed to.
    pub fn cells(&self) -> &HashSet<Cell> {
        &self.cells
    }

    /// True if `cell` is currently in view.
    pub fn contains(&self, cell: Cell) -> bool {
        self.cells.contains(&cell)
    }

    /// Recentre on `center`. Returns the added/removed cells; an empty
    /// [`AoiUpdate`] means the centre cell did not change.
    pub fn recenter(&mut self, center: Cell) -> AoiUpdate {
        if self.center == Some(center) {
            return AoiUpdate::default();
        }
        let new_cells: HashSet<Cell> = cells_in_radius(center, self.radius).into_iter().collect();
        let added = new_cells.difference(&self.cells).copied().collect();
        let removed = self.cells.difference(&new_cells).copied().collect();
        self.cells = new_cells;
        self.center = Some(center);
        AoiUpdate { added, removed }
    }
}

/// Spatial index of entities keyed by grid cell.
#[derive(Debug, Default)]
pub struct World {
    grid: Grid,
    cell_of: HashMap<EntityId, Cell>,
    index: HashMap<Cell, HashSet<EntityId>>,
}

impl World {
    pub fn new(grid: Grid) -> World {
        World {
            grid,
            cell_of: HashMap::new(),
            index: HashMap::new(),
        }
    }

    pub fn grid(&self) -> Grid {
        self.grid
    }

    /// Number of tracked entities.
    pub fn len(&self) -> usize {
        self.cell_of.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cell_of.is_empty()
    }

    /// The cell an entity currently occupies.
    pub fn cell_of_entity(&self, id: EntityId) -> Option<Cell> {
        self.cell_of.get(&id).copied()
    }

    /// Place or move `id` to the cell containing world position `(x, z)`.
    /// Returns `true` if the entity crossed into a different cell.
    pub fn place(&mut self, id: EntityId, x: f32, z: f32) -> bool {
        let new_cell = self.grid.cell_of(x, z);
        self.place_in_cell(id, new_cell)
    }

    /// Place or move `id` directly into `cell`. Returns `true` on cell change.
    pub fn place_in_cell(&mut self, id: EntityId, cell: Cell) -> bool {
        match self.cell_of.get(&id).copied() {
            Some(old) if old == cell => false,
            old => {
                if let Some(old) = old {
                    if let Some(set) = self.index.get_mut(&old) {
                        set.remove(&id);
                        if set.is_empty() {
                            self.index.remove(&old);
                        }
                    }
                }
                self.index.entry(cell).or_default().insert(id);
                self.cell_of.insert(id, cell);
                true
            }
        }
    }

    /// Remove an entity from the index entirely.
    pub fn remove(&mut self, id: EntityId) {
        if let Some(cell) = self.cell_of.remove(&id) {
            if let Some(set) = self.index.get_mut(&cell) {
                set.remove(&id);
                if set.is_empty() {
                    self.index.remove(&cell);
                }
            }
        }
    }

    /// Entities in a single cell.
    pub fn entities_in(&self, cell: Cell) -> Vec<EntityId> {
        self.index
            .get(&cell)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Entities anywhere within Chebyshev `radius` of `center`.
    pub fn entities_in_radius(&self, center: Cell, radius: i32) -> Vec<EntityId> {
        let mut out = Vec::new();
        for cell in cells_in_radius(center, radius) {
            if let Some(set) = self.index.get(&cell) {
                out.extend(set.iter().copied());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_of_handles_negative_and_boundary() {
        let g = Grid::new(1024.0);
        assert_eq!(g.cell_of(0.0, 0.0), Cell::new(0, 0));
        assert_eq!(g.cell_of(1023.9, 0.0), Cell::new(0, 0));
        assert_eq!(g.cell_of(1024.0, 0.0), Cell::new(1, 0));
        assert_eq!(g.cell_of(-0.1, -0.1), Cell::new(-1, -1));
        assert_eq!(g.cell_of(-1024.0, 2048.0), Cell::new(-1, 2));
    }

    #[test]
    fn radius_block_is_square() {
        let cells = cells_in_radius(Cell::new(0, 0), 2);
        assert_eq!(cells.len(), 25);
        let set: HashSet<Cell> = cells.into_iter().collect();
        assert_eq!(set.len(), 25);
        assert!(set.contains(&Cell::new(-2, -2)));
        assert!(set.contains(&Cell::new(2, 2)));
        assert!(!set.contains(&Cell::new(3, 0)));
    }

    #[test]
    fn first_recenter_from_none_adds_full_block() {
        // A brand-new subscription (center == None) must report every cell in
        // the AoI block as `added` on its first recentre — the server relies on
        // this to hand a joining player its initial interest set without waiting
        // for a cell-boundary crossing.
        let mut sub = Subscription::new(AOI_RADIUS_CELLS);
        assert_eq!(sub.center(), None);
        let first = sub.recenter(Cell::new(0, 0));
        assert!(!first.is_empty());
        assert_eq!(first.added.len(), 25);
        assert!(first.removed.is_empty());
        let added: HashSet<Cell> = first.added.into_iter().collect();
        assert_eq!(
            added,
            cells_in_radius(Cell::new(0, 0), AOI_RADIUS_CELLS)
                .into_iter()
                .collect::<HashSet<Cell>>()
        );
        assert!(added.contains(&Cell::new(0, 0)));
    }

    #[test]
    fn subscription_diffs_on_crossing_only() {
        let mut sub = Subscription::new(2);
        let first = sub.recenter(Cell::new(0, 0));
        assert_eq!(first.added.len(), 25);
        assert!(first.removed.is_empty());

        // Same cell -> no change.
        assert!(sub.recenter(Cell::new(0, 0)).is_empty());

        // Step one cell in +x: a column of 5 leaves, a column of 5 enters.
        let step = sub.recenter(Cell::new(1, 0));
        assert_eq!(step.added.len(), 5);
        assert_eq!(step.removed.len(), 5);
        for c in &step.added {
            assert_eq!(c.cx, 3); // new leading column
        }
        for c in &step.removed {
            assert_eq!(c.cx, -2); // old trailing column
        }
        assert_eq!(sub.cells().len(), 25);
    }

    #[test]
    fn index_moves_entity_between_cells() {
        let mut w = World::new(Grid::default());
        assert!(w.place(1, 10.0, 10.0)); // new -> crossed
        assert_eq!(w.cell_of_entity(1), Some(Cell::new(0, 0)));
        assert!(!w.place(1, 20.0, 20.0)); // same cell -> no crossing
        assert!(w.place(1, 2000.0, 10.0)); // -> cell (1,0)
        assert_eq!(w.entities_in(Cell::new(0, 0)), Vec::<EntityId>::new());
        assert_eq!(w.entities_in(Cell::new(1, 0)), vec![1]);
        w.remove(1);
        assert!(w.is_empty());
        assert!(w.entities_in(Cell::new(1, 0)).is_empty());
    }

    /// Property: while an entity moves, it is visible to a subscriber exactly
    /// when its cell lies within the subscriber's radius — and the subscriber's
    /// tracked cell set always agrees with a fresh radius computation.
    #[test]
    fn moving_entity_stays_in_every_in_range_subscribers_view() {
        let grid = Grid::new(1024.0);
        let mut world = World::new(grid);
        let mover: EntityId = 999;

        // A spread of stationary subscribers, each recentred on its own cell.
        let subscriber_cells = [
            Cell::new(-3, -3),
            Cell::new(0, 0),
            Cell::new(2, 1),
            Cell::new(5, -2),
            Cell::new(7, 4),
        ];
        let mut subs: Vec<Subscription> = subscriber_cells
            .iter()
            .map(|&c| {
                let mut s = Subscription::new(AOI_RADIUS_CELLS);
                s.recenter(c);
                s
            })
            .collect();

        // Deterministic sweep across the grid in both axes.
        let mut x = -3200.0f32;
        while x < 8200.0 {
            let mut z = -3200.0f32;
            while z < 4600.0 {
                world.place(mover, x, z);
                let mover_cell = world.cell_of_entity(mover).unwrap();

                for (i, sub) in subs.iter_mut().enumerate() {
                    let center = subscriber_cells[i];

                    // Subscription bookkeeping must match a from-scratch block.
                    let expected: HashSet<Cell> = cells_in_radius(center, AOI_RADIUS_CELLS)
                        .into_iter()
                        .collect();
                    assert_eq!(sub.cells(), &expected);

                    let in_range = mover_cell.chebyshev_distance(center) <= AOI_RADIUS_CELLS;
                    // Visibility via the subscription's cell set...
                    assert_eq!(sub.contains(mover_cell), in_range);
                    // ...and via the world index queried over the same block.
                    let visible = world
                        .entities_in_radius(center, AOI_RADIUS_CELLS)
                        .contains(&mover);
                    assert_eq!(visible, in_range);
                }
                z += 300.0;
            }
            x += 300.0;
        }
    }
}
