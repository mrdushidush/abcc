//! `abcc paint` — one frame of the battlefield, straight to the terminal.
//!
//! ADR-0012's flagship made visible before it is wired to the log. It is a
//! **still**, on purpose: what it answers is *does this terminal draw our
//! composite*, and that question has to be settled before a screen is built on
//! top of the answer. The W5 spike settled it for the spike's encoder; this one
//! is a different encoder, written from the format (`abcc_tui::sixel`), so it
//! owes its own demonstration.
//!
//! ⚠ **It does not probe the terminal.** A DA1 query is the thing
//! `ratatui-image` panics on under Windows, and a probe that hangs waiting for
//! an answer is worse than a picture that does not appear — the operator can see
//! whether a picture appeared. The usage line says which terminals have sixel
//! and which strip it; that is the honest interface for a diagnostic.
//!
//! 🚨 **Everything goes through the opaque composite** (ADR-0012 §2). Not by
//! discipline here: `abcc_tui::sixel::Sprite` has no encoder, so the only way
//! any of this reaches stdout is `Canvas::blend`.

use std::io::Write;
use std::path::{Path, PathBuf};

use abcc_tui::assets;
use abcc_tui::battlefield::{Battlefield, Unit};
use abcc_tui::sixel::{Encoder, Sprite};

use crate::AppError;

/// Where the corpus is, when the operator has not said.
const SPRITES_ENV: &str = "ABCC_SPRITES";

/// A dark olive field. C&C's temperate theatre, cut dark enough that a sprite
/// sits on it rather than in it.
const GROUND: [u8; 3] = [38, 46, 30];
/// The tile rule: a shade up from the ground and no more. It is scenery.
const RULE: [u8; 3] = [56, 66, 44];

/// Draw one frame and write it to `out`.
///
/// # Errors
///
/// [`AppError::Refused`] if no sprite directory was named or it holds nothing
/// this build can decode, [`AppError::Io`] if the terminal will not take it.
pub fn paint(
    sprites: Option<&str>,
    px: u32,
    size: (u32, u32),
    out: &mut impl Write,
) -> Result<(), AppError> {
    let root = corpus_root(sprites)?;
    let (width, height) = size;

    let roster = roster(&root, px)?;
    if roster.intact.is_empty() {
        return Err(AppError::Refused(format!(
            "no sprite in {} is a picture of one thing: {} decoded and every one came \
             apart (see `Sprite::coherence`). This build reads PNG and GIF.",
            root.display(),
            roster.rejected
        )));
    }

    // 🚨 **The tile is sized off the sprite's WIDTH, not its height.** `--px`
    // sets a sprite's *height*, and this corpus is wider than it is tall
    // (292x181 for most poses, so 161 px across at `--px 100`). A tile of nine
    // tenths of the height put adjacent ground points **45 px apart under a
    // 132 px figure**: two thirds of every unit stood behind its neighbour and
    // the nine read as one stack, which is what the operator reviewing the
    // picture reported (F563, 2026-08-31).
    //
    // Three halves of the widest sprite leaves about a quarter of an overlap —
    // enough that the depth sort is *visible*, which is half of what this
    // diagnostic is for, and little enough that each figure is too.
    let cells = cells(roster.intact.len());
    let widest = roster.intact.iter().map(Sprite::width).max().unwrap_or(px);
    let tallest = roster.intact.iter().map(Sprite::height).max().unwrap_or(px);
    let (tile, origin) = geometry(width, height, widest, tallest, &cells);

    let mut field = Battlefield::new(width, height, GROUND, tile)
        .map_err(|e| AppError::Refused(format!("that is not a field: {e}")))?;
    field.set_origin(origin);
    field.rule_tiles(6, RULE);

    let mut units: Vec<Unit<'_>> = roster
        .intact
        .iter()
        .zip(&cells)
        .map(|(sprite, &cell)| Unit { sprite, cell })
        .collect();
    field.deploy(&mut units);

    let stream = field.encode(&mut Encoder::new());
    out.write_all(&stream)?;
    out.write_all(b"\n")?;
    writeln!(
        out,
        "{} sprite(s) at {px} px on a {width}x{height} field \u{2014} {} bytes of sixel",
        units.len(),
        stream.len()
    )?;
    if roster.rejected > 0 {
        writeln!(
            out,
            "{} more distinct image(s) decoded and were not fit to draw (F565).",
            roster.rejected
        )?;
    }
    writeln!(
        out,
        "\u{26a0} no picture above? this terminal has no sixel. Windows Terminal \u{2265} 1.22 does; \
         tmux and Zellij strip it."
    )?;
    out.flush()?;
    Ok(())
}

/// The cells the demo stands its units on: the squarest block that holds them.
///
/// No centring here — [`geometry`] centres the *content*, which it has to do
/// anyway because a unit is drawn above its ground point rather than on it.
fn cells(n: usize) -> Vec<(i32, i32)> {
    let root = n.isqrt().max(1);
    let cols = i32::try_from(if root * root < n { root + 1 } else { root }).unwrap_or(1);
    (0..i32::try_from(n).unwrap_or(0))
        .map(|i| (i % cols, i / cols))
        .collect()
}

/// The tile width, and where cell (0, 0) goes.
///
/// A unit stands at `(cx - cy)` half-tiles across and `(cx + cy)` half-tile-
/// heights down, so those two sums — not the row and column counts — are what
/// the picture is measured in. The block is `(dmax - dmin) * tile/2 + sprite_w`
/// across and `(smax - smin) * tile/4 + sprite_h` tall.
///
/// 🚨 **Centring the origin is not centring the picture.** A unit is drawn a
/// full sprite-height *above* its ground point, so the content reaches further
/// up from the origin than down; a grid centred on the canvas cuts the heads
/// off the back rank the moment the tiles are wide enough to separate the units
/// — 40 rows at `--px 100` (F563). What gets centred here is the block.
///
/// ⚠ **The clamp is not a formality.** At `--px 120` with the full corpus, nine
/// sprites of 194x120 are 209,520 px of art on 230,400 px of ground: they
/// cannot all be separated, and the tile shrinks until they fit rather than
/// letting anything off the edge. That is F144 arriving from the layout side —
/// screen area binds, not sprite count — and `--size` is the answer for an
/// operator who wants more room.
fn geometry(
    width: u32,
    height: u32,
    sprite_w: u32,
    sprite_h: u32,
    cells: &[(i32, i32)],
) -> (u32, (i32, i32)) {
    let (dmin, dmax) = span(cells, |cx, cy| cx - cy);
    let (smin, smax) = span(cells, |cx, cy| cx + cy);
    let (dspan, sspan) = (
        u32::try_from(dmax - dmin).unwrap_or(0),
        u32::try_from(smax - smin).unwrap_or(0),
    );

    // Half a tile across per step in `cx - cy`, a quarter of one down per step
    // in `cx + cy`, because the projection is 2:1.
    let wanted = sprite_w * 3 / 2;
    // A single unit spans nothing in one or both axes, and the only limit on
    // its tile is the one the other axis sets.
    let by_width = (2 * width.saturating_sub(sprite_w))
        .checked_div(dspan)
        .unwrap_or(u32::MAX);
    let by_height = (4 * height.saturating_sub(sprite_h))
        .checked_div(sspan)
        .unwrap_or(u32::MAX);
    let tile = wanted.min(by_width).min(by_height).max(8);

    let (half_w, half_h) = (
        i32::try_from(tile / 2).unwrap_or(0),
        i32::try_from(tile / 4).unwrap_or(0),
    );
    let (sw, sh) = (
        i32::try_from(sprite_w).unwrap_or(0),
        i32::try_from(sprite_h).unwrap_or(0),
    );
    let left = (i32::try_from(width).unwrap_or(0) - ((dmax - dmin) * half_w + sw)) / 2;
    let top = (i32::try_from(height).unwrap_or(0) - ((smax - smin) * half_h + sh)) / 2;
    (
        tile,
        (left + sw / 2 - dmin * half_w, top + sh - smin * half_h),
    )
}

/// The lowest and highest a cell sum reaches over the block.
fn span(cells: &[(i32, i32)], of: impl Fn(i32, i32) -> i32) -> (i32, i32) {
    cells
        .iter()
        .map(|&(cx, cy)| of(cx, cy))
        .fold((0, 0), |(lo, hi), v| (lo.min(v), hi.max(v)))
}

/// The corpus directory: the flag, then the environment, then a refusal.
///
/// ⚠ There is no default path. The corpus lives outside this repository, so a
/// path compiled in here would be right on one machine and wrong on every other
/// one — the same reason `ABCC_MODEL` has no default (`ops::model_name`).
fn corpus_root(explicit: Option<&str>) -> Result<PathBuf, AppError> {
    let named = explicit
        .map(str::to_owned)
        .or_else(|| std::env::var(SPRITES_ENV).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Refused(format!(
                "no sprite directory named. Pass --sprites, or set {SPRITES_ENV}. The corpus is \
                 not in this repository: it is v1's `packages/ui/public/sprites`, copied rather \
                 than rewritten (ADR-0001)."
            ))
        })?;
    let path = PathBuf::from(named);
    if !path.is_dir() {
        return Err(AppError::Refused(format!(
            "{} is not a directory",
            path.display()
        )));
    }
    Ok(path)
}

/// The share of a sprite's visible art that has to be one connected piece
/// before it is worth standing on a battlefield.
///
/// ⚠ **This corpus does not separate cleanly and the constant is a judgement,
/// not a discovered boundary.** Measured at source resolution, 2026-08-31
/// (F565): the four intact GIFs read **92, 92, 99, 99**, and the twelve PNG
/// poses run from **30 to 94** with no gap to put a line in — `cto-E-selected`
/// scores 94, above either building, because it really is a mostly-assembled
/// machine. ▶ **When better art arrives, re-measure before trusting this.**
const INTACT: u32 = 90;

/// What the corpus offered, and what was usable.
struct Roster {
    /// Up to nine distinct, usable sprites, scaled.
    intact: Vec<Sprite>,
    /// How many distinct images decoded but were not fit to draw.
    rejected: usize,
}

/// The sprites worth drawing.
///
/// 🚨 **Distinct, not the first nine files.** The corpus is 28 files and 16
/// images — every `qa-*.png` is a byte-identical copy of its `cto-*` twin — so
/// taking files in name order paints the same picture twice and calls it two
/// units (F560, 2026-08-30).
///
/// 🚨 **And whole, not merely distinct**, which takes *two* questions because
/// neither one alone is enough (F565, 2026-08-31):
///
/// 1. [`Sprite::coherence`] — is it one thing? Twelve of the sixteen are
///    shattered; the largest connected piece of `cto-N-idle` holds 36% of its
///    visible pixels, and on the field it reads as a heap of parts rather than
///    a machine, which is what the operator reviewing the picture reported.
/// 2. [`Sprite::top_edge_ink`] — is all of it inside the frame? The four
///    `*-selected.png` poses have a caption burnt into the art. `cto-E-selected`
///    passes the first question at **94** and fails this one.
///
/// 🚨 **Both are asked of the SOURCE, never of the scaled copy.** Downscaling
/// spreads soft edges until neighbouring fragments touch, and a shattered
/// sprite silently becomes a coherent one: `cto-W-idle` reads **30** at
/// 292x221 and **62** at `--px 100`, `cto-N-idle` **36** and **64**. Measuring
/// after the resize is measuring the resize.
///
/// ⚠ **The filter is a measurement, not a list of filenames.** Naming the four
/// good files here would be shorter and would quietly exclude better art the
/// moment it is added — and better art is expected.
fn roster(root: &Path, px: u32) -> Result<Roster, AppError> {
    let corpus = assets::Corpus::open(root)
        .map_err(|e| AppError::Refused(format!("cannot read the corpus: {e}")))?;
    let distinct = corpus
        .distinct()
        .map_err(|e| AppError::Refused(format!("cannot read the corpus: {e}")))?;

    let mut chosen: Vec<PathBuf> = distinct
        .values()
        .filter_map(|names| names.first().cloned())
        .collect();
    chosen.sort();

    let (mut intact, mut rejected) = (Vec::new(), 0usize);
    for path in &chosen {
        let Ok(source) = assets::load(path) else {
            continue;
        };
        if source.coherence() < INTACT || source.top_edge_ink() > 0 {
            rejected += 1;
            continue;
        }
        if intact.len() < 9
            && let Ok(sprite) = assets::load_scaled(path, px)
        {
            intact.push(sprite);
        }
    }
    Ok(Roster { intact, rejected })
}

#[cfg(test)]
mod tests {
    use super::{cells, geometry};

    /// The corpus at the sizes the review used: `(px, width, height)` of the
    /// widest pose, `292x181` scaled to `px` tall, nine of them.
    const MIXED: [(u32, u32, u32); 3] = [(75, 121, 75), (100, 161, 100), (120, 194, 120)];
    /// The intact corpus: the GIFs are `380x568`, so tall and narrow.
    const GIFS: [(u32, u32, u32); 3] = [(100, 67, 100), (150, 100, 150), (200, 134, 200)];

    /// Where the block reaches, in canvas pixels: `(left, top, right, bottom)`.
    /// Walks the cells rather than assuming their shape.
    fn extents(width: u32, height: u32, sw: u32, sh: u32, units: usize) -> (i32, i32, i32, i32) {
        let cells = cells(units);
        let (tile, origin) = geometry(width, height, sw, sh, &cells);
        let (half_w, half_h) = (
            i32::try_from(tile / 2).unwrap(),
            i32::try_from(tile / 4).unwrap(),
        );
        let (sw, sh) = (i32::try_from(sw).unwrap(), i32::try_from(sh).unwrap());
        let (mut left, mut top, mut right, mut bottom) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for (cx, cy) in cells {
            let gx = origin.0 + (cx - cy) * half_w;
            let gy = origin.1 + (cx + cy) * half_h;
            left = left.min(gx - sw / 2);
            right = right.max(gx - sw / 2 + sw);
            top = top.min(gy - sh);
            bottom = bottom.max(gy);
        }
        (left, top, right, bottom)
    }

    /// 🚨 **Nobody loses their head.**
    ///
    /// A unit is drawn a full sprite-height above its ground point, so widening
    /// the tile to separate the units pushes the back rank *up* and off the
    /// canvas — 40 rows at `--px 100` with the origin merely centred (F563).
    /// This is the guard on [`geometry`]'s clamp and on its centring.
    #[test]
    fn the_block_fits_the_field_whatever_it_holds() {
        for (label, set, n) in [("mixed", MIXED, 9), ("intact", GIFS, 4)] {
            for (px, sw, sh) in set {
                let (l, t, r, b) = extents(640, 360, sw, sh, n);
                assert!(
                    t >= 0,
                    "{label} --px {px}: the back rank's heads are at {t}"
                );
                assert!(
                    b <= 360,
                    "{label} --px {px}: the front rank's feet are at {b}"
                );
                assert!(l >= 0, "{label} --px {px}: the left flank is at {l}");
                assert!(r <= 640, "{label} --px {px}: the right flank is at {r}");
            }
        }
    }

    /// 🚨 **And nobody is a silhouette behind somebody else.**
    ///
    /// The tile used to come off the sprite's *height*, which hid two thirds of
    /// every unit (F563). Half is the line: past it the picture stops showing
    /// units and starts showing a stack.
    #[test]
    fn a_unit_is_not_mostly_hidden_behind_its_neighbour() {
        for (label, set, n) in [("mixed", MIXED, 9), ("intact", GIFS, 4)] {
            for (px, sw, sh) in set {
                let (tile, _) = geometry(640, 360, sw, sh, &cells(n));
                let hidden = sw.saturating_sub(tile / 2) * 100 / sw;
                assert!(
                    hidden < 50,
                    "{label} --px {px}: {hidden}% of a unit stands behind its neighbour"
                );
            }
        }
    }

    /// The clamp binds before the canvas does, on a field too small to hold the
    /// art — F144 from the layout side, and the reason `--size` exists.
    #[test]
    fn a_cramped_field_shrinks_the_tile_rather_than_clipping() {
        let (l, t, r, b) = extents(320, 200, 194, 120, 9);
        assert!(t >= 0 && l >= 0, "clipped at ({l}, {t})");
        assert!(b <= 200 && r <= 320, "clipped at ({r}, {b})");
    }

    /// The block is as square as the count allows, so four units are a 2x2 and
    /// not a row of four across a field with nothing above them.
    #[test]
    fn the_block_is_the_squarest_one_that_holds_them() {
        let cols = |n: usize| cells(n).iter().map(|c| c.0).max().unwrap() + 1;
        assert_eq!((cols(1), cols(4), cols(5), cols(9)), (1, 2, 3, 3));
        assert_eq!(cells(4), vec![(0, 0), (1, 0), (0, 1), (1, 1)]);
    }

    /// One unit is a degenerate block, and a zero span must not divide by zero.
    #[test]
    fn a_single_unit_still_gets_a_tile_and_a_place_to_stand() {
        let (tile, _) = geometry(640, 360, 67, 100, &cells(1));
        assert!(tile >= 8, "tile collapsed to {tile}");
        let (l, t, r, b) = extents(640, 360, 67, 100, 1);
        assert!(
            l >= 0 && t >= 0 && r <= 640 && b <= 360,
            "({l},{t})-({r},{b})"
        );
    }
}
