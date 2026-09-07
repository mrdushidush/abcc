//! 🚨 **F642/F643 — what the four GIFs hold, and what the admission filter says
//! about the frames after the first.**
//!
//! `#[ignore]`d and pointed by `ABCC_SPRITES`, the same shape as this crate's
//! other corpus instruments. Run it with
//!
//! ```text
//! ABCC_SPRITES=<corpus> cargo test --release -p abcc-tui --test films -- --ignored --nocapture
//! ```
//!
//! ⚠ **`--release` is not optional**, for the reason `frame_cost.rs` gives: the
//! scaler is the hot loop here and the debug build says the opposite of the
//! truth about it.
//!
//! # What it measured, 2026-09-07, this box
//!
//! | file | frames | distinct | hold | loop | source | at 150 px | decode |
//! |---|---|---|---|---|---|---|---|
//! | `coder-{E,W}-attacking.gif` | 97 | 97 | 40 ms | 3.88 s | 300x450 | 5.6 MB | 0.27 s |
//! | `building-{E,W}-attacking.gif` | 241 | 241 | 40 ms | 9.64 s | 380x568 | 13.8 MB | 0.88 s |
//!
//! **All four are real animations** — every frame differs from every other, so
//! there is nothing here a still loader was right to drop. Together they are
//! **676 frames, 38.7 MB at 150 px, 2.1 s to decode**, which is the whole reason
//! `Motion` is a choice rather than always the frames.
//!
//! # 🚨 And the filter that admits them is a statement about frame 0
//!
//! Asked of **every** frame at source, the same four files read:
//!
//! | file | coherence | frames under `INTACT` | frames touching the ceiling |
//! |---|---|---|---|
//! | `coder-{E,W}` | 94–99 | 0 of 97 | 8 of 97 |
//! | `building-{E,W}` | **71**–100 | **63** of 241 | **95** of 241 |
//!
//! ▶ **Per frame, F565's filter rejects the only art the project has.** It is
//! not measuring wrong: the ink arrives in runs (frames 152–192 and 196–235 of
//! the buildings) and the coherence dips in runs (60–85, 95–114), which is an
//! attack throwing pieces off the body and pushing an effect out through the top
//! of its own frame. Both are what *attacking* looks like. The admission
//! question is asked of frame 0 and stays there.

use abcc_tui::assets::{self, Film, Motion, Poses};
use abcc_tui::sixel::Sprite;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The height the field draws at (`--px`'s default), so the cost below is the
/// cost the console actually pays.
const PX: u32 = 150;

/// What the corpus asks to be played at: 40 ms a frame, which is 25 FPS.
const HOLD: Duration = Duration::from_millis(40);

/// The four pictures fit to stand on a field (F565), and what each holds.
const GIFS: [(&str, usize); 4] = [
    ("coder-E-attacking.gif", 97),
    ("coder-W-attacking.gif", 97),
    ("building-E-attacking.gif", 241),
    ("building-W-attacking.gif", 241),
];

/// A byte count as megabytes. Through `u32` rather than `as`, which the lints
/// refuse: a silent loss of digits in a number nobody re-derives is how a
/// measurement goes quietly wrong.
fn mb(bytes: usize) -> f64 {
    f64::from(u32::try_from(bytes).unwrap_or(u32::MAX)) / 1_048_576.0
}

fn corpus_root() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var("ABCC_SPRITES").ok()?);
    path.is_dir().then_some(path)
}

/// FNV-1a over a frame's pixels. A duplicate finder and not a signature — the
/// question is only *is this the same picture as that one*.
fn digest(sprite: &Sprite) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in sprite.rgba() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Where in a loop a run of frames starts and stops, as `first-last` pairs. The
/// shape is the finding: scattered frames would be noise in the art, and runs
/// are an effect with a beginning and an end.
fn runs(flags: &[bool]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, on) in flags.iter().enumerate() {
        match (on, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push(format!("{s}-{}", i - 1));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(format!("{s}-{}", flags.len() - 1));
    }
    out.join(",")
}

/// 🚨 **F642 — every frame is a different picture, and the still loader dropped
/// all of them.**
#[test]
#[ignore = "needs the shipped corpus; pass ABCC_SPRITES"]
fn all_four_pictures_are_animations_and_every_frame_differs() {
    let Some(root) = corpus_root() else {
        panic!("ABCC_SPRITES does not point at a directory");
    };

    for (name, frames) in GIFS {
        let path = root.join(name);
        let started = Instant::now();
        let film = Film::open(&path, PX).expect("decode every frame");
        let decode = started.elapsed();

        assert_eq!(film.len(), frames, "{name} changed length");
        assert!(film.is_animated());

        let distinct: BTreeSet<u64> = film.frames().iter().map(digest).collect();
        assert_eq!(
            distinct.len(),
            frames,
            "{name} holds {} distinct picture(s) in {frames} frames",
            distinct.len()
        );

        let holds: BTreeSet<Duration> = (0..film.len()).map(|i| film.hold(i)).collect();
        assert_eq!(
            holds,
            BTreeSet::from([HOLD]),
            "{name} is no longer one rate throughout"
        );
        assert_eq!(film.duration(), HOLD * u32::try_from(frames).unwrap());

        // Scaled by height, and the first frame is what the still loader hands
        // back — the defect this file exists for, in one assertion.
        assert_eq!(film.still().height(), PX);
        assert_eq!(
            film.still(),
            &assets::load_scaled(&path, PX).expect("still")
        );
        println!(
            "{name:<26} {frames:>4} frames, all distinct, {:.2} s loop, {:>5.1} MB at {PX} px, \
             decoded in {:.2} s",
            film.duration().as_secs_f64(),
            mb(film.bytes()),
            decode.as_secs_f64()
        );
    }
}

/// 🚨 **F643 — the admission filter passes these files because it asks frame 0,
/// and asked of every frame it would refuse the buildings.**
///
/// The guard on a well-meant change: filtering *frames* by the questions that
/// filter *files* empties the corpus.
#[test]
#[ignore = "needs the shipped corpus; pass ABCC_SPRITES"]
fn the_admission_questions_are_about_the_first_frame_only() {
    let Some(root) = corpus_root() else {
        panic!("ABCC_SPRITES does not point at a directory");
    };

    let mut refused_per_frame = 0usize;
    for (name, frames) in GIFS {
        let path = root.join(name);
        // 🚨 F565's first trap: ask the SOURCE. Downscaling spreads soft edges
        // until fragments touch and a shattered sprite reads as a whole one.
        // One film at a time — a building at source is 208 MB of frames.
        let film = Film::open(&path, 0).expect("decode at source");
        let coherence: Vec<u32> = film.frames().iter().map(Sprite::coherence).collect();
        let ink: Vec<usize> = film.frames().iter().map(Sprite::top_edge_ink).collect();

        let under: Vec<bool> = coherence.iter().map(|c| *c < assets::INTACT).collect();
        let inked: Vec<bool> = ink.iter().map(|i| *i > 0).collect();
        refused_per_frame += under.iter().zip(&inked).filter(|(u, i)| **u || **i).count();

        println!(
            "{name:<26} coherence {}-{}, {} under {}, {} touching the ceiling\n  under: [{}]\n  \
             ink:   [{}]",
            coherence.iter().min().copied().unwrap_or(0),
            coherence.iter().max().copied().unwrap_or(0),
            under.iter().filter(|u| **u).count(),
            assets::INTACT,
            inked.iter().filter(|i| **i).count(),
            runs(&under),
            runs(&inked),
        );

        // Frame 0 is admitted, which is why the picture is on the field at all.
        assert!(coherence[0] >= assets::INTACT, "{name} frame 0 came apart");
        assert_eq!(ink[0], 0, "{name} frame 0 touches its own ceiling");
        assert_eq!(coherence.len(), frames);
    }

    assert!(
        refused_per_frame > 0,
        "no frame fails the file filter any more \u{2014} the art changed, and `INTACT`'s \
         frame-0 rule can be re-argued"
    );
    println!("\n{refused_per_frame} of 676 frames would be refused by the file filter\n");

    // And the filter as it stands admits all four, which is the state the field
    // depends on.
    let poses = Poses::open_with(&root, PX, Motion::Playing).expect("open");
    assert_eq!(poses.len(), 4);
    assert!(poses.missing().is_empty());
}

/// What a playing corpus costs against a still one — the number that made
/// `Motion` a choice rather than always the frames.
#[test]
#[ignore = "needs the shipped corpus; pass ABCC_SPRITES"]
fn the_frames_cost_two_seconds_and_thirty_eight_megabytes() {
    let Some(root) = corpus_root() else {
        panic!("ABCC_SPRITES does not point at a directory");
    };

    let started = Instant::now();
    let still = Poses::open(&root, PX).expect("still");
    let still_took = started.elapsed();

    let started = Instant::now();
    let playing = Poses::open_playing(&root, PX).expect("playing");
    let playing_took = started.elapsed();

    println!(
        "\nstill:   {} frame(s), {:.1} MB, {:.2} s\nplaying: {} frame(s), {:.1} MB, {:.2} s\n",
        still.frames(),
        mb(still.bytes()),
        still_took.as_secs_f64(),
        playing.frames(),
        mb(playing.bytes()),
        playing_took.as_secs_f64()
    );

    assert_eq!(still.frames(), 4, "a still is one frame per pose");
    assert_eq!(playing.frames(), 676, "97 + 97 + 241 + 241");
    assert!(
        playing.bytes() > still.bytes() * 100,
        "the frames should cost two orders of magnitude more than the stills"
    );
    assert_eq!(playing.shortest_hold(), Some(HOLD));
    assert_eq!(
        playing.longest_loop(),
        Some(HOLD * 241),
        "the field repeats on the longest loop it holds"
    );
    // Both stand the same units in the same places; only the frames after the
    // first are different.
    assert_eq!(still.extent(), playing.extent());
    assert_eq!(still.missing(), playing.missing());
}
