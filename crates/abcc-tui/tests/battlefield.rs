//! Where a unit stands, and what order units are drawn in. Both of these look
//! like decoration until they are wrong.

use abcc_tui::battlefield::{Battlefield, Grid, Unit};
use abcc_tui::sixel::{Canvas, Sprite};

fn solid(w: u32, h: u32, colour: [u8; 4]) -> Sprite {
    Sprite::from_rgba(w, h, colour.repeat((w * h) as usize)).expect("sprite")
}

/// The pixel at a canvas coordinate. `try_from` rather than a cast so that a
/// fixture that walks off the canvas fails here, loudly, instead of indexing
/// somewhere arbitrary and asserting about the wrong pixel.
fn at(field: &Battlefield, x: i32, y: i32) -> [u8; 3] {
    let stride = field.canvas().width() as usize;
    let col = usize::try_from(x).expect("x is on the canvas");
    let row = usize::try_from(y).expect("y is on the canvas");
    let start = (row * stride + col) * 3;
    let pixels = field.canvas().pixels();
    [pixels[start], pixels[start + 1], pixels[start + 2]]
}

/// The projection is 2:1 and both axes run downward.
#[test]
fn the_grid_is_two_to_one_and_both_axes_run_down_the_screen() {
    let grid = Grid {
        tile_w: 40,
        origin: (100, 50),
    };
    assert_eq!(grid.tile_h(), 20);

    assert_eq!(grid.ground(0, 0), (100, 50));
    // +x is down-right, +y is down-left, by half a tile each way.
    assert_eq!(grid.ground(1, 0), (120, 60));
    assert_eq!(grid.ground(0, 1), (80, 60));
    // Opposite corners of a tile share a row: this is what makes it isometric
    // rather than merely slanted.
    assert_eq!(grid.ground(1, 0).1, grid.ground(0, 1).1);
    assert_eq!(grid.ground(1, 1), (100, 70));
}

/// 🚨 **A unit stands on its cell; it does not hang from it.**
///
/// The sprite is anchored bottom-centre. Top-left anchoring would put a 221 px
/// idle pose a whole body-length above where it is standing — and because the
/// corpus is not one height (221 idle, 181 everything else), the error would be
/// different for different poses of the same unit.
#[test]
fn a_unit_is_anchored_at_the_bottom_centre_of_its_sprite() {
    let mut field = Battlefield::new(200, 200, [0, 0, 0], 40).expect("field");
    let grid = field.grid();
    let (gx, gy) = grid.ground(0, 0);

    let sprite = solid(10, 30, [255, 0, 0, 255]);
    field.deploy(&mut [Unit {
        sprite: &sprite,
        cell: (0, 0),
    }]);

    let px = |x: i32, y: i32| at(&field, x, y);

    // The bottom row of the sprite sits on the row just above the ground point,
    // and the sprite is centred on it horizontally.
    assert_eq!(
        px(gx, gy - 1),
        [255, 0, 0],
        "the feet are not on the ground"
    );
    assert_eq!(
        px(gx, gy),
        [0, 0, 0],
        "the sprite spills below the ground point"
    );
    // Rows gy-30 ..= gy-1 are the sprite; gy-31 is the sky above its head.
    assert_eq!(
        px(gx, gy - 30),
        [255, 0, 0],
        "the sprite is shorter than it should be"
    );
    assert_eq!(
        px(gx, gy - 31),
        [0, 0, 0],
        "the sprite is taller than it should be"
    );
    assert_eq!(
        px(gx - 5, gy - 1),
        [255, 0, 0],
        "not centred: left edge missing"
    );
    assert_eq!(px(gx + 5, gy - 1), [0, 0, 0], "not centred: too far right");
}

/// 🚨🚨 **Depth order is the sum of the coordinates, and a roster order that
/// disagrees does not win.**
///
/// The near unit is passed **first**, which is the order that would draw it
/// behind if the field simply painted the slice as given. Both are placed on the
/// same screen column so that one must cover the other, and the near one has to
/// be the one on top.
#[test]
fn a_nearer_unit_covers_a_further_one_whatever_order_it_arrives_in() {
    // ⚠ A small tile on purpose. At tile 40 two units a cell apart are 44 px
    // apart vertically and a 20 px sprite never reaches the one behind it — the
    // first version of this test asserted a colour in a region only one sprite
    // could occupy, so it would have passed with the sort deleted. Overlap is
    // the whole thing being tested, so the geometry has to produce some.
    let mut field = Battlefield::new(200, 200, [0, 0, 0], 8).expect("field");
    let grid = field.grid();

    let near = solid(20, 20, [255, 0, 0, 255]);
    let far = solid(20, 20, [0, 0, 255, 255]);

    // Equal coordinates put both on the same screen column; (1, 1) has depth 2
    // against (0, 0)'s 0, so (1, 1) is the nearer.
    let mut units = vec![
        Unit {
            sprite: &near,
            cell: (1, 1),
        },
        Unit {
            sprite: &far,
            cell: (0, 0),
        },
    ];
    field.deploy(&mut units);

    // Sorting happened in place, and the far unit is now first.
    assert_eq!(units[0].cell, (0, 0));
    assert_eq!(units[1].cell, (1, 1));

    let (fx, fy) = grid.ground(0, 0);
    let (_, ny) = grid.ground(1, 1);
    let overlap = ny - 20 + 1;
    assert!(
        overlap < fy && overlap > fy - 20,
        "the fixture no longer makes the sprites overlap: rows {} and {}",
        fy - 20,
        ny - 20
    );

    assert_eq!(
        at(&field, fx, overlap),
        [255, 0, 0],
        "a unit at the back of the field painted over one at the front"
    );
}

/// Two units on one tile keep the order the caller gave them.
#[test]
fn the_sort_is_stable_so_a_caller_can_stack_two_things_on_one_cell() {
    let mut field = Battlefield::new(120, 120, [0, 0, 0], 40).expect("field");
    let under = solid(20, 20, [0, 0, 255, 255]);
    let over = solid(20, 20, [255, 0, 0, 255]);
    let mut units = vec![
        Unit {
            sprite: &under,
            cell: (0, 0),
        },
        Unit {
            sprite: &over,
            cell: (0, 0),
        },
    ];
    field.deploy(&mut units);

    let (gx, gy) = field.grid().ground(0, 0);
    assert_eq!(at(&field, gx, gy - 1), [255, 0, 0]);
}

/// A unit off the edge of the field costs nothing and panics on nothing.
#[test]
fn a_unit_deployed_off_the_map_is_clipped() {
    let mut field = Battlefield::new(64, 64, [1, 2, 3], 40).expect("field");
    let before = field.canvas().pixels().to_vec();
    let sprite = solid(20, 20, [255, 0, 0, 255]);
    field.deploy(&mut [
        Unit {
            sprite: &sprite,
            cell: (900, 900),
        },
        Unit {
            sprite: &sprite,
            cell: (-900, -900),
        },
    ]);
    assert_eq!(field.canvas().pixels(), &before[..]);
}

/// The field is opaque before anything is drawn on it — the whole premise of the
/// composite rule.
#[test]
fn an_empty_field_is_one_opaque_colour() {
    let field = Battlefield::new(8, 8, [30, 40, 20], 16).expect("field");
    assert_eq!(field.canvas().pixels().len(), 8 * 8 * 3);
    assert!(
        field
            .canvas()
            .pixels()
            .chunks_exact(3)
            .all(|p| p == [30, 40, 20])
    );
    // And it encodes, which is the only thing a canvas is for.
    let mut encoder = abcc_tui::sixel::Encoder::new();
    let stream = field.encode(&mut encoder);
    assert!(stream.starts_with(&[0x1b, b'P']));
    assert!(stream.ends_with(&[0x1b, 0x5c]));
    // Deliberately compared against the canvas's own encoding: a battlefield is
    // a canvas plus placement, and it must not become a second encoder.
    assert_eq!(
        stream,
        Canvas::filled(8, 8, [30, 40, 20])
            .expect("c")
            .encode(&mut encoder)
    );
}
