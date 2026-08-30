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

    // The tile is sized off the sprite so the field scales with the units on it
    // rather than needing a second flag that can disagree with the first.
    let tile = (px * 9 / 10).max(8);
    let mut field = Battlefield::new(width, height, GROUND, tile)
        .map_err(|e| AppError::Refused(format!("that is not a field: {e}")))?;
    field.rule_tiles(6, RULE);

    let loaded = roster(&root, px)?;
    if loaded.is_empty() {
        return Err(AppError::Refused(format!(
            "no sprite in {} could be decoded. This build reads PNG and GIF.",
            root.display()
        )));
    }

    // Two ranks facing each other across the field, which is the shape the
    // roster will actually take: a slot's units on one side of the work.
    let mut units: Vec<Unit<'_>> = loaded
        .iter()
        .enumerate()
        .map(|(i, sprite)| {
            let n = i32::try_from(i).unwrap_or(0);
            Unit {
                sprite,
                cell: (n % 3 - 1, n / 3 - 1),
            }
        })
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
    writeln!(
        out,
        "\u{26a0} no picture above? this terminal has no sixel. Windows Terminal \u{2265} 1.22 does; \
         tmux and Zellij strip it."
    )?;
    out.flush()?;
    Ok(())
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

/// Up to nine distinct sprites, scaled.
///
/// 🚨 **Distinct, not the first nine files.** The corpus is 28 files and 16
/// images — every `qa-*.png` is a byte-identical copy of its `cto-*` twin — so
/// taking files in name order paints the same picture twice and calls it two
/// units. Measured 2026-08-30.
fn roster(root: &Path, px: u32) -> Result<Vec<Sprite>, AppError> {
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

    Ok(chosen
        .iter()
        .filter_map(|path| assets::load_scaled(path, px).ok())
        .take(9)
        .collect())
}
