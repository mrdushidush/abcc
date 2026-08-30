//! Loading the corpus, and the two things measuring it corrected.
//!
//! Most of this runs on images built in memory. The two tests that need the real
//! corpus are `#[ignore]`d and pointed by `ABCC_SPRITES`, the same shape as
//! `abcc`'s `ABCC_LOG` reader test: an instrument kept because the claim it
//! checks is about files this workspace did not synthesise.

use abcc_tui::assets::{self, Corpus};
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
