//! Loading the corpus, and the two things measuring it corrected.
//!
//! Most of this runs on images built in memory. The two tests that need the real
//! corpus are `#[ignore]`d and pointed by `ABCC_SPRITES`, the same shape as
//! `abcc`'s `ABCC_LOG` reader test: an instrument kept because the claim it
//! checks is about files this workspace did not synthesise.

use abcc_tui::assets::{self, Corpus, Design, Facing, Pose, Poses};
use abcc_tui::sixel::Sprite;
use std::path::PathBuf;

/// The shipped corpus, when a run points at one.
fn corpus_root() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var("ABCC_SPRITES").ok()?);
    path.is_dir().then_some(path)
}

// ---------------------------------------------------------------------------
// scaling, which is where the alpha goes wrong if it goes wrong
// ---------------------------------------------------------------------------

/// 🚨 **A transparent pixel's colour must not leak into its neighbours.**
///
/// The test image is the failure in miniature: one opaque white pixel beside one
/// fully transparent **black** pixel. A straight (non-premultiplied) resize
/// averages the two colours and hands back a grey — the dark rim that would
/// appear around every sprite in this corpus. Premultiplied, the transparent
/// pixel contributes nothing but its alpha, and the surviving colour stays
/// white.
///
/// ⚠ The assertion is on the **colour** and not on the alpha. Both methods
/// produce the same alpha; it is only the colour that separates them, which is
/// why this bug is easy to ship.
#[test]
fn scaling_does_not_drag_the_colour_of_transparent_pixels_into_view() {
    // 2x2 -> 1x1 forces all four pixels into one: a left column of opaque white
    // and a right column of fully transparent black.
    let src = Sprite::from_rgba(
        2,
        2,
        vec![
            255, 255, 255, 255, 0, 0, 0, 0, // row 0
            255, 255, 255, 255, 0, 0, 0, 0, // row 1
        ],
    )
    .expect("sprite");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("pair.png");
    write_png(&path, &src);

    let scaled = assets::load_scaled(&path, 1).expect("scale");
    assert_eq!((scaled.width(), scaled.height()), (1, 1));

    let px = scaled.rgba();
    assert!(
        px[0] > 200,
        "the transparent neighbour's black bled into the visible pixel: {px:?}"
    );
}

/// Height is what is asked for, and the width follows the aspect ratio.
#[test]
fn a_sprite_is_scaled_by_its_height_and_keeps_its_shape() {
    let src = Sprite::from_rgba(292, 221, [9, 9, 9, 255].repeat(292 * 221)).expect("sprite");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("block.png");
    write_png(&path, &src);

    // 120 px is the top of the band David judged reads as C&C at arm's length
    // (F143): 292x221 -> 159x120.
    let scaled = assets::load_scaled(&path, 120).expect("scale");
    assert_eq!((scaled.width(), scaled.height()), (159, 120));

    // A target of 0 means "as stored", and is how `load` is spelled.
    let same = assets::load_scaled(&path, 0).expect("load");
    assert_eq!((same.width(), same.height()), (292, 221));
}

#[test]
fn a_missing_file_is_named_rather_than_panicked_on() {
    let err = assets::load(&PathBuf::from("no-such-sprite.png")).expect_err("should fail");
    assert!(err.to_string().contains("no-such-sprite.png"), "{err}");
}

#[test]
fn bytes_that_are_not_an_image_are_a_different_error_from_a_missing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("not.png");
    std::fs::write(&path, b"this is not a png").expect("write");
    let err = assets::load(&path).expect_err("should fail");
    assert!(
        matches!(err, assets::AssetError::Undecodable { .. }),
        "a corrupt file read as a missing one: {err}"
    );
}

// ---------------------------------------------------------------------------
// 🚨 the corpus, and the two numbers reading it corrected
// ---------------------------------------------------------------------------

/// 🚨🚨 **28 files, 16 distinct images.**
///
/// Every `qa-*.png` is byte-identical to its `cto-*` twin — twelve duplicate
/// pairs. The battlefield therefore has **one** humanoid design and not two, and
/// the QA unit and the CTO unit cannot be told apart by their art. Whatever
/// distinguishes them on screen has to be colour, position or label.
///
/// ⚠ The brief that opened this milestone said *"sprites/ 28 files 35 MB"*, and
/// it is right about the files. A file count is not a capability count.
#[test]
#[ignore = "needs the shipped corpus; pass ABCC_SPRITES"]
fn the_corpus_holds_fewer_images_than_it_holds_files() {
    let Some(root) = corpus_root() else {
        panic!("ABCC_SPRITES does not point at a directory");
    };
    let corpus = Corpus::open(&root).expect("open");
    let distinct = corpus.distinct().expect("hash");

    assert_eq!(corpus.files().len(), 28, "the corpus changed size");
    assert_eq!(distinct.len(), 16, "the number of distinct images changed");

    let duplicated: usize = distinct.values().filter(|v| v.len() > 1).count();
    assert_eq!(duplicated, 12, "the twelve cto/qa duplicate pairs changed");
}

/// 🚨 **F145's 5,062, reproduced — and its companion number corrected.**
///
/// The measurement the composite rule rests on, taken again through a decoder
/// this repository wrote rather than the spike's. It lands exactly, which is
/// what makes the rest of the row trustworthy:
///
/// | band | pixels |
/// |---|---|
/// | clear (alpha 0) | 49,466 |
/// | **alpha 1–127** | **5,062** |
/// | alpha 128–254 | 3,983 |
/// | opaque (alpha 255) | 6,021 |
///
/// ⚠ ADR-0012 §2 says *"5,062 … against ~10,000 opaque"*. The 5,062 is exact.
/// The ~10,000 is the set a threshold-128 rule **keeps** (10,004), not the set
/// that is opaque (6,021). The conclusion is unchanged and the picture is
/// slightly starker than the label: of the 15,066 pixels that are visible at
/// all, **9,045 carry partial alpha**.
#[test]
#[ignore = "needs the shipped corpus; pass ABCC_SPRITES"]
fn f145_reproduces_and_the_threshold_would_erase_a_third_of_the_visible_body() {
    let Some(root) = corpus_root() else {
        panic!("ABCC_SPRITES does not point at a directory");
    };
    let sprite = assets::load(&root.join("cto-E-idle.png")).expect("load");
    assert_eq!((sprite.width(), sprite.height()), (292, 221));

    assert_eq!(
        sprite.dropped_by_threshold(128),
        5_062,
        "F145 did not reproduce"
    );
    assert_eq!(sprite.feathered(), 9_045);

    let hist = sprite.alpha_histogram();
    assert_eq!(hist[0], 49_466, "fully transparent");
    assert_eq!(
        hist[255], 6_021,
        "fully opaque — ADR-0012 §2 calls this ~10,000"
    );

    let visible: usize = hist[1..].iter().sum();
    assert_eq!(visible, 15_066);
    assert!(
        sprite.dropped_by_threshold(128) * 3 > visible,
        "the threshold no longer erases about a third of the visible body"
    );
}

// ---------------------------------------------------------------------------

/// Write a sprite out as a PNG so the loader has something real to read.
fn write_png(path: &std::path::Path, sprite: &Sprite) {
    let img = image::RgbaImage::from_raw(sprite.width(), sprite.height(), sprite.rgba().to_vec())
        .expect("buffer");
    img.save_with_format(path, image::ImageFormat::Png)
        .expect("write png");
}

/// 🚨 **F565: of the sixteen distinct images in the shipped corpus, four are
/// fit to stand on a battlefield** — and they are the four GIFs.
///
/// Measured 2026-08-31 at source resolution. Twelve `cto-*`/`qa-*` PNG poses
/// fail on one of two counts: the art is in pieces, or a caption is burnt into
/// the top of the frame. `cto-E-selected` is the one that needs both questions
/// asked — it scores **94** for coherence, above either building, and is still
/// unusable because of the caption.
///
/// ⚠ **Both questions are asked of the source, never of a scaled copy.**
/// Downscaling spreads soft edges until fragments touch: `cto-W-idle` reads 30
/// at `292x221` and 62 at `--px 100`. The assertion below is on `load`, and it
/// is the reason `abcc paint` decodes twice.
#[test]
#[ignore = "needs the shipped corpus; pass ABCC_SPRITES"]
fn only_the_gifs_are_fit_to_draw() {
    let Ok(root) = std::env::var("ABCC_SPRITES") else {
        panic!("point ABCC_SPRITES at the corpus");
    };
    let corpus = Corpus::open(std::path::Path::new(&root)).unwrap();
    let distinct = corpus.distinct().unwrap();
    let mut paths: Vec<_> = distinct
        .values()
        .filter_map(|names| names.first().cloned())
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 16, "the corpus is 16 distinct images (F560)");

    let mut fit: Vec<String> = Vec::new();
    for path in &paths {
        let sprite = assets::load(path).unwrap();
        if sprite.coherence() >= 90 && sprite.top_edge_ink() == 0 {
            fit.push(path.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    assert_eq!(
        fit,
        vec![
            "building-E-attacking.gif",
            "building-W-attacking.gif",
            "coder-E-attacking.gif",
            "coder-W-attacking.gif",
        ]
    );

    // The one that needs the second question asked.
    let selected = paths
        .iter()
        .find(|p| p.file_name().unwrap() == "cto-E-selected.png")
        .unwrap();
    let selected = assets::load(selected).unwrap();
    assert!(selected.coherence() >= 90, "coherence alone would admit it");
    assert!(
        selected.top_edge_ink() > 0,
        "and the caption is what excludes it"
    );
}

// ---------------------------------------------------------------------------
// 🚨 the four pictures the roster asks for, and how a corpus is indexed
// ---------------------------------------------------------------------------

/// A figure: one connected opaque block with a clear top row, which is what the
/// two questions in [`Poses::open`] are asking for.
fn figure(w: u32, h: u32) -> Sprite {
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for row in 1..h {
        for col in 0..w {
            let i = ((row * w + col) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[200, 100, 50, 255]);
        }
    }
    Sprite::from_rgba(w, h, rgba).expect("sprite")
}

/// The same figure in pieces: two blocks with a column of nothing between them,
/// so the largest connected piece is half of it.
fn shattered(w: u32, h: u32) -> Sprite {
    let mut rgba = figure(w, h).rgba().to_vec();
    let gap = w / 2;
    for row in 0..h {
        let i = ((row * w + gap) * 4) as usize;
        rgba[i + 3] = 0;
    }
    Sprite::from_rgba(w, h, rgba).expect("sprite")
}

/// A figure with a caption burnt across the top of the frame, which is what
/// disqualifies all four `*-selected.png` poses (F565).
fn captioned(w: u32, h: u32) -> Sprite {
    let mut rgba = figure(w, h).rgba().to_vec();
    for col in 0..w / 2 {
        let i = (col * 4) as usize;
        rgba[i + 3] = 255;
    }
    Sprite::from_rgba(w, h, rgba).expect("sprite")
}

/// 🚨 **A filename says which pose it is, and the convention is the contract.**
///
/// Naming the four good files in the code would be shorter and would exclude
/// better art the day it arrives. What is pinned here is the *shape* of a name —
/// so `coder-E-anything` is admitted and a design nobody has given a job on the
/// field is not.
#[test]
fn a_filename_says_which_pose_it_is_and_an_unknown_design_says_nothing() {
    let pose = |name: &str| Pose::named(&PathBuf::from(name));

    assert_eq!(
        pose("coder-E-attacking.gif"),
        Some(Pose::new(Design::Coder, Facing::East))
    );
    assert_eq!(
        pose("building-W-attacking.gif"),
        Some(Pose::new(Design::Building, Facing::West))
    );
    // Art that follows the convention with a pose nobody has drawn yet.
    assert_eq!(
        pose("coder-W-idle.png"),
        Some(Pose::new(Design::Coder, Facing::West))
    );

    // A design with no job on the field, the two facings the corpus does not
    // have, and a name that is not the shape at all.
    assert_eq!(pose("cto-E-idle.png"), None);
    assert_eq!(pose("qa-N-selected.png"), None);
    assert_eq!(pose("coder-N-attacking.gif"), None);
    assert_eq!(pose("coder.png"), None);
}

/// 🚨 **The index admits art by measuring it, never by its name**, and keeps the
/// three reasons a pose can be undrawable apart.
///
/// *No coder was drawn* has three causes and they are three different sentences
/// to tell an operator: nothing in the corpus is named for it, the file will not
/// decode, or the art is broken. A single count would flatten them.
#[test]
fn the_index_measures_the_art_and_says_which_reason_kept_it_off() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_png(&dir.path().join("coder-E-attacking.png"), &figure(40, 60));
    write_png(
        &dir.path().join("coder-W-attacking.png"),
        &shattered(40, 60),
    );
    write_png(
        &dir.path().join("building-E-attacking.png"),
        &captioned(40, 60),
    );
    // Named for a design with no job on the field: not a fault, just not ours.
    write_png(&dir.path().join("cto-E-idle.png"), &figure(40, 60));

    let poses = Poses::open(dir.path(), 30).expect("open");

    assert_eq!(poses.len(), 1, "only the whole figure is fit to draw");
    assert_eq!(poses.rejected().len(), 2, "the shattered and the captioned");
    assert_eq!(poses.unnamed(), 1, "the cto pose");
    assert_eq!(poses.unreadable(), 0);

    // 🚨 And there is no substitute: a west-facing coder is missing even though
    // an east-facing one is right there. Handing back the mirror would put a
    // picture on the field saying something the log did not.
    assert!(
        poses
            .sprite(Pose::new(Design::Coder, Facing::East))
            .is_some()
    );
    assert!(
        poses
            .sprite(Pose::new(Design::Coder, Facing::West))
            .is_none()
    );
    assert_eq!(poses.missing().len(), 3);

    // Scaled on the way in, by height, the way `--px` is spelled.
    let drawn = poses
        .sprite(Pose::new(Design::Coder, Facing::East))
        .expect("the coder");
    assert_eq!(drawn.height(), 30);
}

/// 🚨 **Both questions are asked of the source, never of the scaled copy.**
///
/// Downscaling spreads soft edges until fragments touch, and a shattered sprite
/// silently becomes a coherent one — `cto-W-idle` reads 30 at 292x221 and 62 at
/// `--px 100` (F565). The fixture is a figure whose gap is one pixel wide: at
/// full size it is in two pieces, and scaled down far enough it would not be.
#[test]
fn a_broken_figure_is_judged_before_it_is_scaled() {
    let broken = shattered(40, 60);
    assert!(
        broken.coherence() < 90,
        "the fixture is not broken: {}",
        broken.coherence()
    );

    let dir = tempfile::tempdir().expect("tempdir");
    write_png(&dir.path().join("coder-E-attacking.png"), &broken);
    // Scaled to a tenth, where the gap cannot survive.
    let poses = Poses::open(dir.path(), 6).expect("open");
    assert!(
        poses.is_empty(),
        "the resize made a broken figure look whole"
    );
}

/// The shipped corpus, indexed: four pictures, one per pose, and the other
/// twenty-four files are named for designs that have no job on the field.
#[test]
#[ignore = "needs the shipped corpus; pass ABCC_SPRITES"]
fn the_shipped_corpus_fills_every_pose_and_nothing_it_names_is_broken() {
    let Some(root) = corpus_root() else {
        panic!("ABCC_SPRITES does not point at a directory");
    };
    let poses = Poses::open(&root, 150).expect("open");

    assert_eq!(poses.len(), 4, "the four GIFs");
    assert!(poses.missing().is_empty(), "{:?}", poses.missing());
    assert_eq!(poses.unnamed(), 24, "every cto and qa file");
    assert_eq!(
        poses.rejected().len(),
        0,
        "a file named for a pose failed the filter: {:?}",
        poses.rejected()
    );
    assert_eq!(poses.unreadable(), 0);

    // ⚠ The building is drawn at the same height as the coder, so the corpus's
    // own relative sizes (380x568 against 300x450) are not preserved on the
    // field. That is `--px`'s meaning, and whether it is right is an eye
    // question — this pins what the code does so a change to it is deliberate.
    let coder = poses
        .sprite(Pose::new(Design::Coder, Facing::East))
        .expect("coder");
    let building = poses
        .sprite(Pose::new(Design::Building, Facing::East))
        .expect("building");
    assert_eq!((coder.height(), building.height()), (150, 150));
    assert_eq!((coder.width(), building.width()), (100, 100));
}
