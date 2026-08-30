//! Getting the corpus off the disk and onto a [`Canvas`].
//!
//! The 44 MB of art is **copied, not rewritten** — ADR-0001 exempts assets from
//! *rewrite, do not port*, and the sprites and the 96 voice lines are David's own
//! work carried over from v1 (`CREDITS.md`). What this module owns is the two
//! steps between a file and a battlefield: decode, and scale.
//!
//! # 🚨 Scaling premultiplies, and it is not a nicety
//!
//! Resizing straight (non-premultiplied) RGBA mixes the *colour* of fully
//! transparent pixels into their visible neighbours. A sprite cut out on a
//! transparent background usually carries black there, so a straight resize
//! draws a dark rim around everything — and this corpus is the worst case for
//! it: F145 measured **5,062 semi-transparent pixels** on `cto-E-idle`, soft
//! alpha across a third of the body rather than an anti-aliased edge. Every one
//! of those is a pixel a straight resize would darken.
//!
//! So: multiply each channel by its alpha, resize, divide back out. The cost is
//! two passes over a buffer that is scaled once at load and then composited
//! thousands of times.
//!
//! # What the corpus actually holds — measured 2026-08-30
//!
//! 🚨 **28 files, 16 distinct images.** Every `qa-*.png` is a byte-identical copy
//! of its `cto-*` twin — twelve duplicate pairs, verified by hash. The roster
//! therefore has **one** humanoid design, not two, and the QA unit and the CTO
//! unit cannot be told apart by their art. Colour and label are what separate
//! them. See [`Corpus::distinct`], which answers this by hashing rather than by
//! counting filenames.
//!
//! The rest: `coder-{E,W}-attacking.gif` (97 frames) and
//! `building-{E,W}-attacking.gif` (241 frames) — **attacking only, east and west
//! only**, so there is no idle animation and no north or south facing for
//! either. `Holding` and `Commandeered` have no voice line yet.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use image::{ImageFormat, RgbaImage};

use crate::sixel::{SixelError, Sprite};

/// What can go wrong between a path and a sprite.
#[derive(Debug)]
pub enum AssetError {
    /// The file is not there, or cannot be read.
    Unreadable { path: PathBuf, source: io::Error },
    /// The bytes are there and are not an image this build can decode.
    ///
    /// ⚠ A separate variant from [`AssetError::Unreadable`] because they are two
    /// different things to tell an operator: one is a missing asset, the other
    /// is a decoder that was compiled out.
    Undecodable { path: PathBuf, detail: String },
    /// The decoded image will not fit the sprite type.
    Malformed { path: PathBuf, source: SixelError },
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssetError::Unreadable { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            AssetError::Undecodable { path, detail } => {
                write!(f, "cannot decode {}: {detail}", path.display())
            }
            AssetError::Malformed { path, source } => {
                write!(
                    f,
                    "{} decoded to something unusable: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for AssetError {}

/// Load one still image and scale it so its **height** is `target_px`.
///
/// Height rather than width, because a unit's height is what sets how it reads
/// against the others on an isometric field and the corpus is not one aspect
/// ratio — the idle poses are 292x221 and everything else is 292x181.
///
/// ⚠ `target_px` of 0, or an image that scales to nothing, is refused by
/// [`Sprite::from_rgba`] rather than silently producing an empty sprite.
///
/// # Errors
///
/// [`AssetError`] if the file cannot be read, decoded, or used.
pub fn load_scaled(path: &Path, target_px: u32) -> Result<Sprite, AssetError> {
    let bytes = std::fs::read(path).map_err(|source| AssetError::Unreadable {
        path: path.to_path_buf(),
        source,
    })?;
    let decoded = image::load_from_memory(&bytes).map_err(|e| AssetError::Undecodable {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    let rgba = decoded.to_rgba8();
    let scaled = scale(&rgba, target_px);
    let (w, h) = (scaled.width(), scaled.height());
    Sprite::from_rgba(w, h, scaled.into_raw()).map_err(|source| AssetError::Malformed {
        path: path.to_path_buf(),
        source,
    })
}

/// The sprite at its stored size.
///
/// # Errors
///
/// [`AssetError`] if the file cannot be read, decoded, or used.
pub fn load(path: &Path) -> Result<Sprite, AssetError> {
    load_scaled(path, 0)
}

/// Premultiply, resize, unpremultiply. See the module docs for why the first and
/// third steps are there.
///
/// A `target_px` of 0, or one that already matches, returns the image unscaled.
fn scale(src: &RgbaImage, target_px: u32) -> RgbaImage {
    if target_px == 0 || target_px == src.height() || src.height() == 0 {
        return src.clone();
    }
    // Integer arithmetic, rounded: `width * target / height`. Floating point
    // would be the obvious way and it needs a cast back that can truncate or go
    // negative, which is a lot of ceremony for a ratio of two small integers.
    let width = ((u64::from(src.width()) * u64::from(target_px) + u64::from(src.height()) / 2)
        / u64::from(src.height()))
    .max(1);
    let width = u32::try_from(width).unwrap_or(u32::MAX);

    let mut premultiplied = src.clone();
    for px in premultiplied.pixels_mut() {
        let a = u32::from(px.0[3]);
        for c in 0..3 {
            px.0[c] = u8::try_from((u32::from(px.0[c]) * a + 127) / 255).unwrap_or(u8::MAX);
        }
    }

    // Triangle rather than nearest: this corpus is AI-rendered with soft edges
    // rather than hand-placed pixels, so preserving the feathering reads better
    // at the 75-120 px David picked (F143) than preserving hard pixel edges.
    let mut out = image::imageops::resize(&premultiplied, width, target_px, FilterType::Triangle);

    for px in out.pixels_mut() {
        let a = u32::from(px.0[3]);
        if a == 0 {
            // Nothing to recover, and dividing would be a divide by zero. The
            // colour under a fully transparent pixel is not a fact.
            px.0 = [0, 0, 0, 0];
            continue;
        }
        for c in 0..3 {
            let v = (u32::from(px.0[c]) * 255 + a / 2) / a;
            px.0[c] = u8::try_from(v.min(255)).unwrap_or(u8::MAX);
        }
    }
    out
}

/// A directory of sprite files, answered by reading it rather than by a list
/// somebody wrote down.
///
/// 🚨 The reason this exists rather than a `const` array of names: the brief
/// said *28 sprites* and the directory holds **16 distinct images**. A filename
/// is not a specification, and a file count is not a capability count.
#[derive(Debug, Clone)]
pub struct Corpus {
    root: PathBuf,
    files: Vec<PathBuf>,
}

impl Corpus {
    /// Read a sprite directory.
    ///
    /// # Errors
    ///
    /// [`AssetError::Unreadable`] if the directory cannot be listed.
    pub fn open(root: &Path) -> Result<Corpus, AssetError> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(root)
            .map_err(|source| AssetError::Unreadable {
                path: root.to_path_buf(),
                source,
            })?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| ImageFormat::from_extension(e).is_some())
            })
            .collect();
        files.sort();
        Ok(Corpus {
            root: root.to_path_buf(),
            files,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every image file, in name order.
    #[must_use]
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// 🚨 **Distinct images, keyed by content**, with every filename that carries
    /// each one.
    ///
    /// Measured on the shipped corpus 2026-08-30: **28 files, 16 entries here**,
    /// because every `qa-*.png` is byte-identical to its `cto-*` twin. A caller
    /// building a roster needs this rather than the file list, or it will believe
    /// it has two unit designs when it has one.
    ///
    /// The key is the file's own bytes, so this says *the same file twice* and
    /// deliberately not *the same picture twice*: two encodings of one image
    /// would count as two, which is the honest answer for a function that has
    /// not decoded anything.
    ///
    /// # Errors
    ///
    /// [`AssetError::Unreadable`] if any file will not read.
    pub fn distinct(&self) -> Result<BTreeMap<u64, Vec<PathBuf>>, AssetError> {
        let mut by_content: BTreeMap<u64, Vec<PathBuf>> = BTreeMap::new();
        for path in &self.files {
            let bytes = std::fs::read(path).map_err(|source| AssetError::Unreadable {
                path: path.clone(),
                source,
            })?;
            by_content
                .entry(digest(&bytes))
                .or_default()
                .push(path.clone());
        }
        Ok(by_content)
    }
}

/// FNV-1a. Enough to bucket identical files and no more — this is a duplicate
/// finder, not a signature, and saying so keeps anyone from treating it as one.
fn digest(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
