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

    #[must_use]
    pub const fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    /// Draw the grid's tile edges, so the field reads as ground rather than as a
    /// flat colour behind some figures.
    ///
    /// Cheap and deliberately dim: it is scenery, and anything on it has to stay
    /// more legible than it is.
    pub fn rule_tiles(&mut self, cells: i32, ink: [u8; 3]) {
        for cx in -cells..=cells {
            for cy in -cells..=cells {
                let (x, y) = self.grid.ground(cx, cy);
                self.dot(x, y, ink);
            }
        }
    }

    fn dot(&mut self, x: i32, y: i32, ink: [u8; 3]) {
        let pixel = Sprite::from_rgba(1, 1, vec![ink[0], ink[1], ink[2], 255]);
        if let Ok(pixel) = pixel {
            self.canvas.blend(&pixel, x, y);
        }
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
