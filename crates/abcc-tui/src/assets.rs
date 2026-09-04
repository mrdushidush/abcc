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

// ---------------------------------------------------------------------------
// 🚨 The four pictures the roster can ask for, and the filter that admits them
// ---------------------------------------------------------------------------

/// The share of a sprite's visible art that has to be one connected piece
/// before it is worth standing on a battlefield.
///
/// ⚠ **This corpus does not separate cleanly and the constant is a judgement,
/// not a discovered boundary.** Measured at source resolution, 2026-08-31
/// (F565): the four intact GIFs read **92, 92, 99, 99**, and the twelve PNG
/// poses run from **30 to 94** with no gap to put a line in — `cto-E-selected`
/// scores 94, above either building, because it really is a mostly-assembled
/// machine. ▶ **When better art arrives, re-measure before trusting this.**
pub const INTACT: u32 = 90;

/// What a thing standing on the field is a picture of.
///
/// Two designs, because [`Corpus::distinct`] and F565 between them leave two:
/// twelve of the sixteen distinct images are not fit to draw, and the four that
/// survive are `building` and `coder`, east and west. The roster gives each of
/// them a job — a building is a **mission** and a coder is a **task** — so the
/// picture has a subject rather than being a row of identical figures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Design {
    /// A unit: one task.
    Coder,
    /// A structure: one mission.
    Building,
}

impl Design {
    /// The word the corpus spells it with, and the word a legend prints.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Design::Coder => "coder",
            Design::Building => "building",
        }
    }
}

/// Which side a design is drawn from.
///
/// ⚠ **Two of the four compass points, and no idle pose at all.** The corpus is
/// `*-attacking` only, east and west (F565), so a unit standing still and a unit
/// working are the same picture and nothing on the field can tell them apart.
/// The rank a unit stands on carries that; the facing does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Facing {
    East,
    West,
}

impl Facing {
    /// The letter the corpus spells it with.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Facing::East => "E",
            Facing::West => "W",
        }
    }
}

/// One picture: a design seen from one side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pose {
    pub design: Design,
    pub facing: Facing,
}

impl Pose {
    /// Every picture a field can ask for. Four, which is exactly how many the
    /// shipped corpus has fit to draw — a coincidence that stops being one the
    /// moment better art lands, which is why nothing here counts on it.
    pub const ALL: [Pose; 4] = [
        Pose::new(Design::Coder, Facing::East),
        Pose::new(Design::Coder, Facing::West),
        Pose::new(Design::Building, Facing::East),
        Pose::new(Design::Building, Facing::West),
    ];

    #[must_use]
    pub const fn new(design: Design, facing: Facing) -> Pose {
        Pose { design, facing }
    }

    /// The pose a filename says it is: `{design}-{facing}-{anything}`.
    ///
    /// 🚨 **The convention is the contract, not the four filenames.** Naming
    /// `coder-E-attacking.gif` and its three siblings here would be shorter and
    /// would quietly exclude better art the moment it arrives — and better art
    /// is expected. A file that follows the corpus's own naming is admitted
    /// without a code change; a **new design** is a code change, because a
    /// design has to be given something to mean on the field before it can be
    /// drawn on one.
    ///
    /// ⚠ This says nothing about whether the picture is any good. That is
    /// [`Sprite::coherence`] and [`Sprite::top_edge_ink`], and [`Poses::open`]
    /// asks both.
    #[must_use]
    pub fn named(path: &Path) -> Option<Pose> {
        let stem = path.file_stem()?.to_str()?;
        let mut parts = stem.split('-');
        let design = match parts.next()? {
            "coder" => Design::Coder,
            "building" => Design::Building,
            _ => return None,
        };
        let facing = match parts.next()? {
            "E" | "e" => Facing::East,
            "W" | "w" => Facing::West,
            _ => return None,
        };
        Some(Pose { design, facing })
    }
}

impl std::fmt::Display for Pose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.design.name(), self.facing.name())
    }
}

/// The corpus, filtered and indexed by the pose each picture is of.
///
/// 🚨 **Both questions are asked of the SOURCE, never of the scaled copy**, so
/// this decodes every candidate twice on purpose. Downscaling spreads soft edges
/// until neighbouring fragments touch and a shattered sprite silently becomes a
/// coherent one: `cto-W-idle` reads **30** at 292x221 and **62** at `--px 100`
/// (F565). Measuring after the resize is measuring the resize.
///
/// Everything it could not use is **counted and kept apart by reason**, because
/// *no coder was drawn* has three different causes and they are three different
/// sentences to tell an operator: the corpus has no file for it, the file will
/// not decode, or the art is broken.
#[derive(Debug)]
pub struct Poses {
    by_pose: BTreeMap<Pose, Sprite>,
    rejected: Vec<PathBuf>,
    unreadable: usize,
    unnamed: usize,
}

impl Poses {
    /// Read a corpus and keep the pictures fit to stand on a field, each scaled
    /// to `px` tall.
    ///
    /// Where two files claim one pose, the first in name order that passes wins
    /// and the rest are skipped rather than counted against anything — the
    /// corpus already holds twelve byte-identical duplicate pairs (F560) and a
    /// duplicate is not a defect.
    ///
    /// # Errors
    ///
    /// [`AssetError::Unreadable`] if the directory cannot be listed. A single
    /// file that will not read is counted, not fatal: one bad file must not cost
    /// the operator the whole field.
    pub fn open(root: &Path, px: u32) -> Result<Poses, AssetError> {
        let corpus = Corpus::open(root)?;
        let mut poses = Poses {
            by_pose: BTreeMap::new(),
            rejected: Vec::new(),
            unreadable: 0,
            unnamed: 0,
        };
        for path in corpus.files() {
            let Some(pose) = Pose::named(path) else {
                poses.unnamed += 1;
                continue;
            };
            if poses.by_pose.contains_key(&pose) {
                continue;
            }
            let Ok(source) = load(path) else {
                poses.unreadable += 1;
                continue;
            };
            if source.coherence() < INTACT || source.top_edge_ink() > 0 {
                poses.rejected.push(path.clone());
                continue;
            }
            match load_scaled(path, px) {
                Ok(sprite) => {
                    poses.by_pose.insert(pose, sprite);
                }
                Err(_) => poses.unreadable += 1,
            }
        }
        Ok(poses)
    }

    /// The picture for a pose, or `None` if the corpus has none fit to draw.
    ///
    /// ⚠ **There is deliberately no substitute.** Handing back the other facing,
    /// or the other design, would put a picture on the field that says something
    /// the log did not — and the operator has no way to tell it apart from one
    /// that does.
    #[must_use]
    pub fn sprite(&self, pose: Pose) -> Option<&Sprite> {
        self.by_pose.get(&pose)
    }

    /// The poses this corpus cannot draw.
    #[must_use]
    pub fn missing(&self) -> Vec<Pose> {
        Pose::ALL
            .into_iter()
            .filter(|p| !self.by_pose.contains_key(p))
            .collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_pose.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_pose.is_empty()
    }

    /// Files that named a pose, decoded, and were not fit to draw.
    #[must_use]
    pub fn rejected(&self) -> &[PathBuf] {
        &self.rejected
    }

    /// Files that named a pose and would not decode.
    #[must_use]
    pub const fn unreadable(&self) -> usize {
        self.unreadable
    }

    /// Files whose name does not say which pose they are. On the shipped corpus
    /// that is 24 of the 28 — every `cto-*` and `qa-*` — and it is not a fault.
    #[must_use]
    pub const fn unnamed(&self) -> usize {
        self.unnamed
    }

    /// The widest and tallest picture held, which is what a layout is measured
    /// in. `None` when nothing is held.
    #[must_use]
    pub fn extent(&self) -> Option<(u32, u32)> {
        let widest = self.by_pose.values().map(Sprite::width).max()?;
        let tallest = self.by_pose.values().map(Sprite::height).max()?;
        Some((widest, tallest))
    }
}
