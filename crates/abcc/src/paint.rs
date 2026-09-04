//! `abcc paint` — one frame of the battlefield, straight to the terminal.
//!
//! 🚨 **The roster comes from the event log.** Until CONSOLE it came from a
//! directory listing: `abcc paint --sprites DIR` decoded up to nine distinct
//! images and stood them in a square, which answers *does this terminal draw our
//! composite* and nothing about the fleet. That question is settled, so the
//! default is now the field — **one unit per live task, positioned by slot** —
//! and the corpus view stays behind `--corpus` because it is still the right
//! diagnostic when the picture itself looks wrong.
//!
//! The fold is [`abcc_tui::View`] and the placement is [`abcc_tui::Roster`];
//! neither is here. What is here are the three things that are only true of a
//! process: reading the whole log, resolving where the art lives, and writing
//! the legend that says which task is which — because **the field cannot name
//! its units** and a picture of six identical coders is not an answer on its own.
//!
//! ⚠ **It does not probe the terminal.** A DA1 query is the thing
//! `ratatui-image` panics on under Windows, and a probe that hangs waiting for
//! an answer is worse than a picture that does not appear — the operator can see
//! whether a picture appeared. The usage line says which terminals have sixel
//! and which strip it; that is the honest interface for a diagnostic.
//!
//! 🚨 **It opens the log without booting it**, for the same reason `abcc fun`
//! does: `Store::boot` sweeps orphans and requeues their tasks, which is right
//! for a process that has just started and catastrophic for one standing beside
//! a live attempt. A command that changed what it draws by drawing it would be
//! the defect `abcc board` avoids by the same means.
//!
//! 🚨 **Everything goes through the opaque composite** (ADR-0012 §2). Not by
//! discipline here: `abcc_tui::sixel::Sprite` has no encoder, so the only way
//! any of this reaches stdout is `Canvas::blend`.

use std::io::Write;
use std::path::{Path, PathBuf};

use abcc_tui::assets::{self, Poses};
use abcc_tui::battlefield::{Battlefield, Unit};
use abcc_tui::roster::{Post, RANK, Roster, Standing};
use abcc_tui::sixel::{Encoder, Sprite};
use abcc_tui::{Card, Theme, View};

use crate::ops::{self, open_log};
use crate::{AppError, Invocation};

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
/// this build can draw, [`AppError::Io`] if the terminal will not take it.
pub fn paint(
    invocation: &Invocation,
    sprites: Option<&str>,
    px: u32,
    size: (u32, u32),
    corpus: bool,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let root = corpus_root(sprites)?;
    if corpus {
        return the_corpus(&root, px, size, out);
    }
    the_field(invocation, &root, px, size, out)
}

// ---------------------------------------------------------------------------
// the field, from the log
// ---------------------------------------------------------------------------

/// The fleet as the log has it: a building per mission, a unit per live task.
fn the_field(
    invocation: &Invocation,
    root: &Path,
    px: u32,
    size: (u32, u32),
    out: &mut impl Write,
) -> Result<(), AppError> {
    let poses = Poses::open(root, px)
        .map_err(|e| AppError::Refused(format!("cannot read the corpus: {e}")))?;
    if poses.is_empty() {
        return Err(AppError::Refused(format!(
            "no picture in {} is fit to stand on a field: {} named a pose and came apart or \
             carried a caption, and {} are named for designs the field has no job for. This build \
             reads PNG and GIF, and the naming is `{{design}}-{{facing}}-{{action}}`.",
            root.display(),
            poses.rejected().len(),
            poses.unnamed()
        )));
    }

    let ground = ops::ground(invocation)?;
    let store = open_log(&ground.home)?;
    let mut view = View::new(Theme::Command);
    view.fold_all(&crate::fun::read_all(&store)?);
    let roster = Roster::muster(&view);

    let (width, height) = size;
    // 🚨 **The layout is measured against the whole corpus, not against what is
    // standing.** A field sized to the units that happen to be on it would
    // re-lay itself out — different tile, different origin — every time a task
    // changed state, and the operator would read that movement as the fleet
    // moving. `--px` and `--size` are the only things that move the camera.
    let (widest, tallest) = poses.extent().unwrap_or((px, px));
    let cells = roster.cells();
    let (tile, origin) = geometry(width, height, widest, tallest, &cells);

    let mut field = Battlefield::new(width, height, GROUND, tile)
        .map_err(|e| AppError::Refused(format!("that is not a field: {e}")))?;
    field.set_origin(origin);
    field.rule_tiles(6, RULE);

    let mut undrawn = 0usize;
    let mut units: Vec<Unit<'_>> = Vec::new();
    for placed in roster.placed() {
        match poses.sprite(placed.pose) {
            Some(sprite) => units.push(Unit {
                sprite,
                cell: placed.cell,
            }),
            // A unit the log put on the field that the corpus has no picture
            // for. Counted rather than substituted: the other facing would be a
            // picture saying something the log did not.
            None => undrawn += 1,
        }
    }
    field.deploy(&mut units);

    let stream = field.encode(&mut Encoder::new());
    out.write_all(&stream)?;
    out.write_all(b"\n")?;
    writeln!(
        out,
        "the field, from the log \u{2014} {} unit(s) on a {width}x{height} field, {} bytes of sixel",
        units.len(),
        stream.len()
    )?;
    legend(out, &view, &roster)?;
    footnotes(out, &roster, &poses, undrawn)?;
    crowding(out, tile, widest, units.len())?;
    caveat(out)?;
    out.flush()?;
    Ok(())
}

/// 🚨 **Which task is which, because the picture cannot say.**
///
/// Two units in one rank are the same picture — the corpus has four pictures and
/// nine states (F565), and there is no text on the canvas. So the field shows
/// the *shape* of the fleet and this shows the names, rank by rank, left to
/// right, in the order they stand.
///
/// An empty rank prints a sentence rather than nothing. *No slot is engaged* is
/// a fact about the fleet; a missing line is a fact about the console.
fn legend(out: &mut impl Write, view: &View, roster: &Roster) -> Result<(), AppError> {
    writeln!(out)?;
    for post in Post::ALL {
        let mut standing = roster.rank(post).peekable();
        if standing.peek().is_none() {
            writeln!(out, "  {:<8} {}", post.name(), nobody(post))?;
            continue;
        }
        for (i, placed) in standing.enumerate() {
            let rank = if i == 0 { post.name() } else { "" };
            match placed.what {
                Standing::Mission(id) => writeln!(
                    out,
                    "  {:<8} {:<6} {}",
                    rank,
                    id.to_string(),
                    view.mission(id).unwrap_or("(before this window)")
                )?,
                Standing::Task(id) => {
                    let card = view.card_of(id);
                    let state = card.map_or("", |c| Theme::Command.state(&c.state));
                    let title = card.map(Card::label).unwrap_or_default();
                    writeln!(
                        out,
                        "  {:<8} {:<6} {:<8} {:<22} {title}",
                        rank,
                        id.to_string(),
                        slot_of(placed.post),
                        state
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// What an empty rank says. Each one is a statement about the fleet.
const fn nobody(post: Post) -> &'static str {
    match post {
        Post::Base => "no mission has a task on the field",
        Post::Reserve => "nothing standing by",
        Post::Line { .. } => "no slot is engaged",
        Post::Waiting => "nothing is waiting on you",
    }
}

/// The slot column: a number for the line, nothing for the ranks that hold none.
///
/// ⚠ `slot ?` is a task the log says holds a slot without this window having
/// seen which — it stands to the left of slot zero rather than on top of
/// whatever really is in it.
fn slot_of(post: Post) -> String {
    match post {
        Post::Line { slot: Some(unit) } => format!("slot {}", unit.0),
        Post::Line { slot: None } => "slot ?".to_owned(),
        _ => String::new(),
    }
}

/// Everything the picture left out, and why. A field with fewer units than the
/// board has is only honest if it says so.
fn footnotes(
    out: &mut impl Write,
    roster: &Roster,
    poses: &Poses,
    undrawn: usize,
) -> Result<(), AppError> {
    writeln!(out)?;
    if roster.is_empty() {
        if roster.off() == 0 {
            writeln!(
                out,
                "nothing is on the field: the board is empty. `abcc task \"<what to do>\"`."
            )?;
        } else {
            writeln!(
                out,
                "nothing is on the field: all {} task(s) have finished. The fleet is quiet, not \
                 broken.",
                roster.off()
            )?;
        }
    } else if roster.off() > 0 {
        writeln!(out, "{} finished task(s) are off the field.", roster.off())?;
    }
    if roster.crowded() > 0 {
        writeln!(
            out,
            "{} more are on the board than a rank of {RANK} draws \u{2014} `abcc board` has all of \
             them.",
            roster.crowded()
        )?;
    }
    if roster.contested() > 0 {
        writeln!(
            out,
            "\u{1f6a8} {} unit(s) stand on a slot another unit already holds, so they are drawn \
             stacked. Only `abcc run` reconciles the log; a console opened beside a dead run reads \
             both as engaged.",
            roster.contested()
        )?;
    }
    if undrawn > 0 {
        let missing: Vec<String> = poses.missing().iter().map(ToString::to_string).collect();
        writeln!(
            out,
            "{undrawn} unit(s) have no picture: the corpus is missing {}.",
            missing.join(", ")
        )?;
    }
    Ok(())
}

/// 🚨 **Say when the art does not fit, because the picture cannot.**
///
/// The clamp keeps every unit on the canvas by shrinking the tile (F144: screen
/// area binds, not sprite count), and past a point that means figures on one
/// rank standing through one another. Nothing is lost and nothing is wrong;
/// there is simply not enough ground — but an operator who is not told reads it
/// as broken art.
///
/// ⚠ **This is the within-rank measure, and it is not the whole of the
/// crowding.** Ranks are half a tile apart and a figure is a whole tile tall, so
/// a unit always covers part of what is behind it — at the default size a
/// building and the two units in front of it decode as one region, and no field
/// this shape avoids that. That one is inherent and is written up on
/// `Post::depth`; this one moves with `--size`, which is why only this one is
/// worth a line every time.
fn crowding(out: &mut impl Write, tile: u32, widest: u32, units: usize) -> Result<(), AppError> {
    if units < 2 || tile >= widest || widest == 0 {
        return Ok(());
    }
    writeln!(
        out,
        "\u{26a0} the field is narrower than the art standing on it \u{2014} about {}% of a unit \
         is behind its neighbour. `--size 1280x720` gives them room, `--px` makes them smaller.",
        (widest - tile) * 100 / widest
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// the corpus, which is the diagnostic the field was built on top of
// ---------------------------------------------------------------------------

/// Every distinct image fit to draw, in a square. The pre-log demo, kept because
/// it is still how to answer *is the terminal drawing our composite* and *is
/// this art usable* with no log in the way.
fn the_corpus(
    root: &Path,
    px: u32,
    size: (u32, u32),
    out: &mut impl Write,
) -> Result<(), AppError> {
    let (width, height) = size;
    let stock = stock(root, px)?;
    if stock.intact.is_empty() {
        return Err(AppError::Refused(format!(
            "no sprite in {} is a picture of one thing: {} decoded and every one came \
             apart (see `Sprite::coherence`). This build reads PNG and GIF.",
            root.display(),
            stock.rejected
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
    let cells = cells(stock.intact.len());
    let widest = stock.intact.iter().map(Sprite::width).max().unwrap_or(px);
    let tallest = stock.intact.iter().map(Sprite::height).max().unwrap_or(px);
    let (tile, origin) = geometry(width, height, widest, tallest, &cells);

    let mut field = Battlefield::new(width, height, GROUND, tile)
        .map_err(|e| AppError::Refused(format!("that is not a field: {e}")))?;
    field.set_origin(origin);
    field.rule_tiles(6, RULE);

    let mut units: Vec<Unit<'_>> = stock
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
        "the corpus \u{2014} {} sprite(s) at {px} px on a {width}x{height} field, {} bytes of sixel",
        units.len(),
        stream.len()
    )?;
    if stock.rejected > 0 {
        writeln!(
            out,
            "{} more distinct image(s) decoded and were not fit to draw (F565).",
            stock.rejected
        )?;
    }
    writeln!(
        out,
        "\nthis is the art, not the fleet. `abcc paint` with no flag is the field."
    )?;
    caveat(out)?;
    out.flush()?;
    Ok(())
}

/// The line that is the honest interface for a diagnostic that does not probe.
fn caveat(out: &mut impl Write) -> Result<(), AppError> {
    writeln!(
        out,
        "\u{26a0} no picture above? this terminal has no sixel. Windows Terminal \u{2265} 1.22 does; \
         tmux and Zellij strip it."
    )?;
    Ok(())
}

/// The cells the corpus view stands its units on: the squarest block that holds
/// them.
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

/// What the corpus offered, and what was usable.
struct Stock {
    /// Up to nine distinct, usable sprites, scaled.
    intact: Vec<Sprite>,
    /// How many distinct images decoded but were not fit to draw.
    rejected: usize,
}

/// The sprites worth drawing, for the corpus view.
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
/// ⚠ **The filter is a measurement, not a list of filenames.** This asks by
/// *content*, where [`Poses`] asks by the naming convention; the two answer
/// different questions, and the corpus view wants this one because it admits
/// art the field has no job for yet — which is exactly what a person looking at
/// new art needs to see.
fn stock(root: &Path, px: u32) -> Result<Stock, AppError> {
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
        if source.coherence() < assets::INTACT || source.top_edge_ink() > 0 {
            rejected += 1;
            continue;
        }
        if intact.len() < 9
            && let Ok(sprite) = assets::load_scaled(path, px)
        {
            intact.push(sprite);
        }
    }
    Ok(Stock { intact, rejected })
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
        over(width, height, sw, sh, &cells(units))
    }

    /// The same, over cells a caller already has.
    fn over(
        width: u32,
        height: u32,
        sw: u32,
        sh: u32,
        cells: &[(i32, i32)],
    ) -> (i32, i32, i32, i32) {
        let (tile, origin) = geometry(width, height, sw, sh, cells);
        let (half_w, half_h) = (
            i32::try_from(tile / 2).unwrap(),
            i32::try_from(tile / 4).unwrap(),
        );
        let (sw, sh) = (i32::try_from(sw).unwrap(), i32::try_from(sh).unwrap());
        let (mut left, mut top, mut right, mut bottom) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &(cx, cy) in cells {
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

    /// A shape to lay out, and the share of a unit that may stand behind its
    /// neighbour once it is laid out.
    struct Case(&'static str, Vec<(i32, i32)>, u32);

    /// The shape the roster actually produces: four ranks of `RANK`, each a
    /// line of constant depth at `Post::depth`.
    fn a_full_field() -> Vec<(i32, i32)> {
        // The base is at depth -1 and the other three at 2, 4 and 6.
        [-1, 2, 4, 6]
            .into_iter()
            .flat_map(|depth| (0..6).map(move |i| (i, depth - i)))
            .collect()
    }

    /// 🚨 **The field's own worst case fits too, and it is not the square one.**
    ///
    /// A layout checked only against the corpus view would leave the real
    /// picture clipped: nine sprites in a 3x3 reach 4 in `cx + cy` and a full
    /// field reaches 6, while spreading twice as far across. This is the shape
    /// the roster produces, at every `--px` in the corpus's range.
    #[test]
    fn a_full_field_of_four_ranks_fits_at_every_size() {
        for (px, sw, sh) in GIFS {
            let (l, t, r, b) = over(640, 360, sw, sh, &a_full_field());
            assert!(
                l >= 0 && t >= 0 && r <= 640 && b <= 360,
                "--px {px}: a full field reaches ({l},{t})-({r},{b}) on 640x360"
            );
        }
    }

    /// 🚨 **And on the real shape, nobody is mostly hidden either.**
    ///
    /// This is what a rank of constant depth buys and it is measurable: units
    /// one rank apart differ by **two** in `cx - cy`, so they spread by a whole
    /// tile where a row of one `cy` spreads by half of one. Measured on this
    /// project's own log — a base and five waiting — the row layout put
    /// neighbours 60 px apart under a 100 px figure and this one puts them 134,
    /// which is clear ground between them. A full field is the worst case at
    /// 34%, against the row layout's 48%.
    #[test]
    fn the_roster_shape_does_not_stack_its_units() {
        let (sw, sh) = (100, 150);
        let cases = [
            // The log this project actually has: one mission, five waiting.
            Case(
                "a base and five waiting",
                std::iter::once((0, -1))
                    .chain((0..5).map(|i| (i, 6 - i)))
                    .collect(),
                0,
            ),
            Case("four full ranks", a_full_field(), 40),
        ];
        for Case(label, cells, bar) in cases {
            let (tile, _) = geometry(640, 360, sw, sh, &cells);
            // Two steps in `cx - cy` per unit on a rank, half a tile each.
            let apart = tile;
            let hidden = sw.saturating_sub(apart) * 100 / sw;
            assert!(
                hidden <= bar,
                "{label}: {hidden}% of a unit stands behind its neighbour, over {bar}%"
            );
        }
    }

    /// A unit whose slot this window never saw stands to the left of slot zero,
    /// which puts a cell at a negative column — and the layout has to centre a
    /// block that does not start at the origin.
    #[test]
    fn a_block_that_starts_left_of_the_origin_is_still_centred() {
        let cells = [(-2, 6), (-1, 5), (0, 4), (3, 1)];
        let (l, t, r, b) = over(640, 360, 100, 150, &cells);
        assert!(
            l >= 0 && t >= 0 && r <= 640 && b <= 360,
            "({l},{t})-({r},{b})"
        );
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

    /// An empty field has no cells at all, and the layout must not divide by
    /// zero or invent a block — a quiet fleet is a legitimate picture.
    #[test]
    fn an_empty_field_still_has_a_tile_and_an_origin() {
        let (tile, origin) = geometry(640, 360, 100, 150, &[]);
        assert!(tile >= 8, "tile collapsed to {tile}");
        assert!(
            origin.0 > 0 && origin.1 > 0,
            "origin off the canvas: {origin:?}"
        );
    }
}
