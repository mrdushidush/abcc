//! The isometric field: where a unit stands, and what order the units are drawn
//! in.
//!
//! ADR-0012's look is Command & Conquer 1995, which means a **2:1 isometric
//! grid** — a tile twice as wide as it is tall — and units that stand on a cell
//! rather than occupying a rectangle. Two rules do most of the work, and both
//! are the kind that look like decoration until they are wrong:
//!
//! 1. 🚨 **A unit is anchored at the bottom centre of its sprite**, not the top
//!    left. A sprite is mostly empty air above a small pair of feet, so
//!    top-left placement makes a tall unit appear to float a body-length behind
//!    where it is standing — and the corpus is not one height (idle poses are
//!    292x221, everything else 292x181), so the error would differ per pose.
//! 2. 🚨 **Depth order is `cx + cy`, drawn ascending.** Painting in roster order
//!    puts whichever unit was queued last on top, so a unit at the back of the
//!    field occludes one at the front about half the time. There is no z-buffer
//!    here and there does not need to be: on a grid, distance from the camera
//!    *is* the sum of the coordinates.
//!
//! Everything reaches the screen through [`crate::sixel::Canvas`], so ADR-0012
//! §2's composite rule holds here by construction rather than by care.

use crate::sixel::{Canvas, Encoder, SixelError, Sprite};

/// A 2:1 isometric grid, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    /// Tile width. The height is half of it, which is what makes it 2:1.
    pub tile_w: u32,
    /// Where cell (0, 0) sits on the canvas.
    pub origin: (i32, i32),
}

impl Grid {
    /// A grid whose tiles are `tile_w` across, centred horizontally on a canvas.
    #[must_use]
    pub fn centred(tile_w: u32, canvas: &Canvas) -> Grid {
        Grid {
            tile_w,
            // Vertically centred, not a quarter down. Cells run *both* ways from
            // the origin and a unit is drawn a full sprite-height above its
            // ground point, so a high origin clips the back rank's heads —
            // measured at 54 rows lost on a 640x360 field at 100 px sprites.
            origin: (
                i32::try_from(canvas.width() / 2).unwrap_or(i32::MAX),
                i32::try_from(canvas.height() / 2).unwrap_or(i32::MAX),
            ),
        }
    }

    /// Tile height: half the width, because the projection is 2:1.
    #[must_use]
    pub const fn tile_h(self) -> u32 {
        self.tile_w / 2
    }

    /// Where a cell's **ground point** lands on the canvas.
    ///
    /// The classic projection: `x` runs down-right, `y` runs down-left, and both
    /// of them run *down*, which is why the depth key is their sum.
    #[must_use]
    pub fn ground(self, cx: i32, cy: i32) -> (i32, i32) {
        let half_w = i64::from(self.tile_w) / 2;
        let half_h = i64::from(self.tile_h()) / 2;
        let x = i64::from(self.origin.0) + (i64::from(cx) - i64::from(cy)) * half_w;
        let y = i64::from(self.origin.1) + (i64::from(cx) + i64::from(cy)) * half_h;
        (
            i32::try_from(x).unwrap_or(i32::MAX),
            i32::try_from(y).unwrap_or(i32::MAX),
        )
    }
}

/// One thing standing on the field.
#[derive(Debug, Clone)]
pub struct Unit<'a> {
    pub sprite: &'a Sprite,
    pub cell: (i32, i32),
}

impl Unit<'_> {
    /// 🚨 **Distance from the camera**, and the only thing draw order depends on.
    #[must_use]
    pub const fn depth(&self) -> i64 {
        self.cell.0 as i64 + self.cell.1 as i64
    }
}

/// The battlefield: an opaque canvas and the units standing on it.
#[derive(Debug, Clone)]
pub struct Battlefield {
    canvas: Canvas,
    grid: Grid,
}

impl Battlefield {
    /// An empty field of one ground colour.
    ///
    /// # Errors
    ///
    /// [`SixelError::Empty`] if either dimension is zero.
    pub fn new(
        width: u32,
        height: u32,
        ground: [u8; 3],
        tile_w: u32,
    ) -> Result<Battlefield, SixelError> {
        let canvas = Canvas::filled(width, height, ground)?;
        let grid = Grid::centred(tile_w, &canvas);
        Ok(Battlefield { canvas, grid })
    }

    #[must_use]
    pub const fn grid(&self) -> Grid {
        self.grid
    }

    /// Move the grid's origin, for a caller that knows how tall its units are.
    ///
    /// 🚨 **Centring the origin is not centring the picture.** A unit is drawn a
    /// full sprite-height *above* its ground point, so the content reaches
    /// further up from the origin than down, and a grid centred on the canvas
    /// cuts the heads off the back rank the moment the tiles are wide enough to
    /// separate the units — measured at **40 rows at `--px 100`** (F563,
    /// 2026-08-31). [`Grid::centred`] is the right default for a caller that
    /// does not know its sprites; this is for one that does.
    ///
    /// ⚠ Call it **before** [`Battlefield::rule_tiles`], which stamps the marks
    /// at the origin the grid has when it runs.
    pub const fn set_origin(&mut self, origin: (i32, i32)) {
        self.grid.origin = origin;
    }

    #[must_use]
    pub const fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    /// Mark every cell's ground point, so the field reads as ground rather than
    /// as a flat colour behind some figures.
    ///
    /// 🚨 **A mark has to be big enough to see.** The first version of this
    /// stamped a **1x1 pixel** per cell: 119 of them landed on a 640x360 field,
    /// which is **0.05% of the pixels**, and the operator reviewing the picture
    /// reported the tiles as simply absent — correctly, since nothing that small
    /// survives being looked at (F564, 2026-08-31). The mark is now a lozenge on
    /// the same 2:1 projection as the tiles, sized off the tile so it stays in
    /// proportion at every `--px`.
    ///
    /// Cheap and deliberately dim: it is scenery, and anything on it has to stay
    /// more legible than it is.
    pub fn rule_tiles(&mut self, cells: i32, ink: [u8; 3]) {
        let Some(mark) = self.mark(ink) else {
            return;
        };
        let (ox, oy) = (
            i32::try_from(mark.width() / 2).unwrap_or(0),
            i32::try_from(mark.height() / 2).unwrap_or(0),
        );
        for cx in -cells..=cells {
            for cy in -cells..=cells {
                let (x, y) = self.grid.ground(cx, cy);
                self.canvas.blend(&mark, x - ox, y - oy);
            }
        }
    }

    /// The lozenge stamped on a ground point: 2:1 like the tiles themselves, so
    /// the marks read as a grid rather than as scattered dots.
    fn mark(&self, ink: [u8; 3]) -> Option<Sprite> {
        let half_w = (self.grid.tile_w / 16).max(2);
        let half_h = (half_w / 2).max(1);
        let (w, h) = (half_w * 2 + 1, half_h * 2 + 1);
        let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
        for row in 0..h {
            for col in 0..w {
                let dx = i64::from(col) - i64::from(half_w);
                let dy = i64::from(row) - i64::from(half_h);
                // The diamond |dx|/half_w + |dy|/half_h <= 1, multiplied out so
                // it stays in integers.
                let inside = dx.abs() * i64::from(half_h) + dy.abs() * i64::from(half_w)
                    <= i64::from(half_w) * i64::from(half_h);
                if inside {
                    let i = ((row as usize) * (w as usize) + (col as usize)) * 4;
                    rgba[i] = ink[0];
                    rgba[i + 1] = ink[1];
                    rgba[i + 2] = ink[2];
                    rgba[i + 3] = 255;
                }
            }
        }
        Sprite::from_rgba(w, h, rgba).ok()
    }

    /// 🚨 **Place every unit, back to front.**
    ///
    /// The sort is what makes the picture correct, and it is stable so that two
    /// units on the same tile keep the order the caller gave them — a caller that
    /// stacks two things on one cell has an opinion about which is in front, and
    /// this does not overrule it.
    pub fn deploy(&mut self, units: &mut [Unit<'_>]) {
        units.sort_by_key(Unit::depth);
        for unit in units.iter() {
            let (gx, gy) = self.grid.ground(unit.cell.0, unit.cell.1);
            // Bottom centre: the feet are on the ground point.
            let x = gx - i32::try_from(unit.sprite.width() / 2).unwrap_or(0);
            let y = gy - i32::try_from(unit.sprite.height()).unwrap_or(0);
            self.canvas.blend(unit.sprite, x, y);
        }
    }

    /// The field as one sixel image.
    #[must_use]
    pub fn encode(&self, encoder: &mut Encoder) -> Vec<u8> {
        self.canvas.encode(encoder)
    }
}
