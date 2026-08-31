//! 🚨 **F562 — what one frame costs to generate**, which is the number ADR-0012's
//! animation case rests on.
//!
//! `#[ignore]`d and pointed by `ABCC_SPRITES`, the same shape as `abcc`'s
//! `ABCC_LOG` reader test: an instrument kept because the claim it checks is
//! about assets this workspace did not synthesise. Run it with
//!
//! ```text
//! ABCC_SPRITES=<corpus> cargo test --release -p abcc-tui --test frame_cost -- --ignored --nocapture
//! ```
//!
//! ⚠ **`--release` is not optional.** The debug build is roughly 6× slower here
//! and a number taken from it would say the opposite of the truth.
//!
//! # ⚠ What this measures, and what it does not
//!
//! **Generation only**: composite plus encode, with the sprites already decoded
//! and scaled, which is how an animation would hold them. It does **not** measure
//! the terminal receiving or drawing anything.
//!
//! That distinction matters because the spike's headline — **25.6–28.6 FPS for a
//! full-viewport composite** — is an *end-to-end* number that includes display,
//! and F144 found **screen area binds before throughput does**. So a fast result
//! here does not settle the animation question; it settles only that the encoder
//! is not the thing in the way.
//!
//! Measured 2026-08-31, release, this box:
//!
//! | field | sprites | ms/frame | frames/s | bytes |
//! |---|---|---|---|---|
//! | 640×360 | 1 | 0.99 | 1012 | 12,006 |
//! | 640×360 | 4 | 1.24 | 805 | 27,037 |
//! | 640×360 | 16 | 1.91 | 523 | 73,101 |
//! | 1280×720 | 1 | 3.84 | 260 | 12,653 |
//! | 1280×720 | 4 | 4.26 | 235 | 27,673 |
//! | 1280×720 | 16 | 5.13 | 195 | 73,736 |
//!
//! ▶ Two things worth reading off it. **Cost tracks screen area, not sprite
//! count** — 16 sprites cost 1.9× one sprite while four times the area costs
//! 3.9× — which is F144's finding arriving again from the generation side. And at
//! a C&C-style 15 FPS the byte rate is **~1.1 MB/s** against the ~23.5 MB/s
//! Windows Terminal was measured ingesting, so the transport has room too.

use abcc_tui::assets;
use abcc_tui::battlefield::{Battlefield, Unit};
use abcc_tui::sixel::{Encoder, Sprite};
use std::path::PathBuf;
use std::time::Instant;

/// A C&C tick. The bar the table above has to clear by a wide margin for the
/// encoder to be off the critical path.
const TARGET_FPS: f64 = 15.0;

fn corpus_root() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var("ABCC_SPRITES").ok()?);
    path.is_dir().then_some(path)
}

/// Build one frame the way a running console would: a fresh field, the units
/// deployed, one encode. The sprites are **not** reloaded — that is the point.
fn frame(sprites: &[Sprite], width: u32, height: u32, count: usize, enc: &mut Encoder) -> usize {
    let mut field = Battlefield::new(width, height, [38, 46, 30], 90).expect("field");
    let mut units: Vec<Unit<'_>> = (0..count)
        .map(|i| Unit {
            sprite: &sprites[i % sprites.len()],
            cell: (
                i32::try_from(i % 5).unwrap_or(0) - 2,
                i32::try_from(i / 5).unwrap_or(0) - 2,
            ),
        })
        .collect();
    field.deploy(&mut units);
    field.encode(enc).len()
}

#[test]
#[ignore = "a timing instrument; needs the corpus and --release. Pass ABCC_SPRITES"]
fn one_frame_costs_far_less_than_a_c_and_c_tick() {
    let Some(root) = corpus_root() else {
        panic!("ABCC_SPRITES does not point at a directory");
    };
    // Four distinct poses, loaded once and scaled once.
    let sprites: Vec<Sprite> = [
        "cto-E-idle.png",
        "cto-W-idle.png",
        "cto-N-idle.png",
        "cto-S-idle.png",
    ]
    .iter()
    .filter_map(|name| assets::load_scaled(&root.join(name), 100).ok())
    .collect();
    assert!(
        !sprites.is_empty(),
        "no sprite loaded from {}",
        root.display()
    );

    let mut worst_fps = f64::MAX;
    for (width, height) in [(640u32, 360u32), (1280u32, 720u32)] {
        for count in [1usize, 4, 16] {
            let mut enc = Encoder::new();
            for _ in 0..3 {
                frame(&sprites, width, height, count, &mut enc);
            }
            let runs = 30;
            let started = Instant::now();
            let mut bytes = 0;
            for _ in 0..runs {
                bytes = frame(&sprites, width, height, count, &mut enc);
            }
            let ms = started.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
            let fps = 1000.0 / ms;
            worst_fps = worst_fps.min(fps);
            eprintln!(
                "{width}x{height}  {count:>2} sprites: {ms:6.2} ms/frame = {fps:6.1} FPS  \
                 ({bytes} bytes, {:.1} MB/s at {TARGET_FPS:.0} FPS)",
                // A frame is kilobytes; the cast is exact well past any size a
                // terminal would accept.
                u32::try_from(bytes).map_or(f64::NAN, f64::from) * TARGET_FPS / 1_000_000.0
            );
        }
    }

    // ⚠ A deliberately loose bar. This is a timing test on a shared desktop and
    // it exists to catch an order-of-magnitude regression — something that turns
    // the encoder back into the bottleneck — not to police a few per cent.
    assert!(
        worst_fps > TARGET_FPS * 4.0,
        "frame generation fell to {worst_fps:.1} FPS, within 4x of the {TARGET_FPS} FPS tick — \
         the encoder is back on the critical path"
    );
}
