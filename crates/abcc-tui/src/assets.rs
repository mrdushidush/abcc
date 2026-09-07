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
use std::time::Duration;

use image::AnimationDecoder;
use image::codecs::gif::GifDecoder;
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
    let bytes = read(path)?;
    let decoded = image::load_from_memory(&bytes).map_err(|e| AssetError::Undecodable {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    sprite_from(&decoded.to_rgba8(), path, target_px)
}

/// Read a file, saying which one when it will not read.
fn read(path: &Path) -> Result<Vec<u8>, AssetError> {
    std::fs::read(path).map_err(|source| AssetError::Unreadable {
        path: path.to_path_buf(),
        source,
    })
}

/// Scale one decoded image and wrap it. The step every frame shares, whether it
/// came out of a still or out of frame 137 of an animation.
fn sprite_from(rgba: &RgbaImage, path: &Path, target_px: u32) -> Result<Sprite, AssetError> {
    let scaled = scale(rgba, target_px);
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

// ---------------------------------------------------------------------------
// 🚨 The frames a still loader throws away
// ---------------------------------------------------------------------------

/// What a frame that declares no delay is held for.
///
/// GIF stores the delay in hundredths of a second and **0 means *as fast as the
/// viewer can manage***, which every browser has read as 100 ms for thirty
/// years. Taking it literally would give a film a zero-length loop, and the play
/// head divides by that.
const NO_DELAY_HOLD: Duration = Duration::from_millis(100);

/// The substitution, in one place: every way a film can be built goes through
/// it, so a still and a decoded frame cannot end up with different rules about
/// what *no delay* means.
const fn hold_or_default(hold: Duration) -> Duration {
    if hold.is_zero() { NO_DELAY_HOLD } else { hold }
}

/// Every frame of one file, scaled, and how long each is held.
///
/// 🚨 **[`load`] takes the first frame and drops the rest**, which is the right
/// answer for a still and the wrong one for this corpus: the four pictures fit
/// to draw (F565) are *all* animations — `coder-{E,W}-attacking.gif` at 97
/// frames and `building-{E,W}-attacking.gif` at 241 — so a field built out of
/// [`load`] is four animations standing perfectly still.
///
/// # What it holds, and what that costs
///
/// **Frames are scaled as they are decoded and the source is never kept.** A
/// building is 380x568 at source, so 241 frames of it is 208 MB held whole and
/// far less at the height the field draws — the difference between a type you
/// can put four of on a battlefield and one you cannot. [`Film::bytes`] answers
/// what an instance actually costs rather than leaving it to arithmetic.
///
/// ⚠ **A film is scaled once, at open, and never rescaled.** `--px` moving is a
/// reload, the same as it is for a still.
#[derive(Clone, PartialEq, Eq)]
pub struct Film {
    frames: Vec<Sprite>,
    /// When each frame stops being the one on screen, measured from the start of
    /// the loop. Cumulative rather than per-frame because the play head asks
    /// *which frame is up at time t*, and over a 241-frame loop that is one
    /// binary search instead of a walk.
    ends: Vec<Duration>,
}

/// ⚠ **A `Film` prints its shape, not its pixels.** The derived form would put
/// every frame's RGBA into a panic message.
impl std::fmt::Debug for Film {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Film {{ {} frame(s), {}x{}, {:.2}s, {} bytes }}",
            self.frames.len(),
            self.still().width(),
            self.still().height(),
            self.duration().as_secs_f64(),
            self.bytes()
        )
    }
}

impl Film {
    /// Decode every frame of `path`, each scaled so its height is `target_px`.
    ///
    /// A still image is a film of one frame — the caller does not have to know
    /// which it opened, and [`Film::is_animated`] answers when it wants to.
    ///
    /// 🚨 **The format comes from the bytes, not the extension.** A `.gif` that
    /// is really a PNG loads as a still here and would be an `Undecodable` if
    /// this trusted the name; [`Corpus::open`] filters by extension because it
    /// is deciding what to *try*, which is a different question.
    ///
    /// # Errors
    ///
    /// [`AssetError`] if the file cannot be read, decoded, or used. A GIF that
    /// decodes to no frames at all is [`AssetError::Undecodable`]: an empty film
    /// has no first frame, and every caller of this expects a picture.
    pub fn open(path: &Path, target_px: u32) -> Result<Film, AssetError> {
        let bytes = read(path)?;
        let undecodable = |detail: String| AssetError::Undecodable {
            path: path.to_path_buf(),
            detail,
        };
        let format = image::guess_format(&bytes).map_err(|e| undecodable(e.to_string()))?;
        if format != ImageFormat::Gif {
            let decoded =
                image::load_from_memory(&bytes).map_err(|e| undecodable(e.to_string()))?;
            let sprite = sprite_from(&decoded.to_rgba8(), path, target_px)?;
            return Ok(Film::of(sprite));
        }

        let decoder = GifDecoder::new(std::io::Cursor::new(&bytes[..]))
            .map_err(|e| undecodable(e.to_string()))?;
        let mut frames = Vec::new();
        for (i, frame) in decoder.into_frames().enumerate() {
            let frame = frame.map_err(|e| undecodable(format!("frame {i}: {e}")))?;
            let hold = Duration::from(frame.delay());
            // `into_frames` hands back a whole canvas per frame, disposal
            // already applied, so scaling one frame is scaling a picture rather
            // than a patch of one. Scale here and the source-sized buffer is
            // dropped at the end of this iteration.
            frames.push((sprite_from(frame.buffer(), path, target_px)?, hold));
        }
        Film::from_frames(frames).ok_or_else(|| undecodable("no frames".to_owned()))
    }

    /// One picture, held forever: what a still is when everything downstream
    /// takes a film.
    #[must_use]
    pub fn of(sprite: Sprite) -> Film {
        Film {
            frames: vec![sprite],
            ends: vec![hold_or_default(Duration::ZERO)],
        }
    }

    /// A film made from frames already in hand, for a caller that decoded them
    /// itself or a test that would rather not write a GIF to disk.
    ///
    /// `None` when there are no frames, because [`Film::still`] promises one.
    /// A zero hold becomes `NO_DELAY_HOLD`, the same as it does at decode.
    #[must_use]
    pub fn from_frames(frames: Vec<(Sprite, Duration)>) -> Option<Film> {
        if frames.is_empty() {
            return None;
        }
        let mut clock = Duration::ZERO;
        let mut sprites = Vec::with_capacity(frames.len());
        let mut ends = Vec::with_capacity(frames.len());
        for (sprite, hold) in frames {
            clock += hold_or_default(hold);
            sprites.push(sprite);
            ends.push(clock);
        }
        Some(Film {
            frames: sprites,
            ends,
        })
    }

    /// The first frame — what [`load`] would have returned.
    #[must_use]
    pub fn still(&self) -> &Sprite {
        &self.frames[0]
    }

    /// Every frame, in play order.
    #[must_use]
    pub fn frames(&self) -> &[Sprite] {
        &self.frames
    }

    /// 🚨 **The frame that is up `elapsed` after the loop started**, wrapping.
    ///
    /// The play head takes a duration rather than a frame number so that a
    /// dropped frame costs the animation nothing: a console that misses its slot
    /// resumes where the *clock* is, not where the counter is, and the animation
    /// stays the length the art says it is.
    #[must_use]
    pub fn at(&self, elapsed: Duration) -> &Sprite {
        &self.frames[self.index_at(elapsed)]
    }

    /// Which frame [`Film::at`] would give — the same answer, for a caller
    /// measuring rather than drawing.
    #[must_use]
    pub fn index_at(&self, elapsed: Duration) -> usize {
        // A film always holds a frame for something, so this cannot divide by
        // zero: every hold is at least `NO_DELAY_HOLD`.
        let into = elapsed.as_nanos() % self.duration().as_nanos();
        let into = Duration::from_nanos(u64::try_from(into).unwrap_or(u64::MAX));
        // `partition_point` over the cumulative ends: the first frame whose end
        // is past the play head is the one on screen.
        self.ends
            .partition_point(|end| *end <= into)
            .min(self.frames.len() - 1)
    }

    /// How long one loop lasts.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.ends[self.ends.len() - 1]
    }

    /// How long the frame at `index` is held. An index past the end is the last
    /// frame's, so a caller counting frames cannot trip over the boundary.
    #[must_use]
    pub fn hold(&self, index: usize) -> Duration {
        let index = index.min(self.ends.len() - 1);
        match index.checked_sub(1) {
            // `ends` is cumulative and every hold is at least `NO_DELAY_HOLD`,
            // so it is non-decreasing and this never saturates. Spelled
            // saturating rather than `-` because a `Duration` subtraction that
            // could go negative panics, and no picture is worth that.
            Some(prev) => self.ends[index].saturating_sub(self.ends[prev]),
            None => self.ends[0],
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Always false — a film is refused at construction rather than allowed to
    /// exist empty. Here because [`Film::len`] is, and a `len` without an
    /// `is_empty` is a trap.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Whether there is anything to play. One frame is a picture; two are an
    /// animation.
    #[must_use]
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }

    /// What holding this costs in RGBA, which is what a field of them is
    /// budgeted in.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.frames.iter().map(|s| s.rgba().len()).sum()
    }

    /// The shortest time any frame is held.
    ///
    /// 🚨 **This is the rate a player has to sample at**, not the average and
    /// not the first frame's hold: sample any slower and the shortest frame is
    /// the one that gets skipped. On the shipped corpus every hold is 40 ms
    /// (F642), so it answers 40 ms — but a corpus with one fast frame in a slow
    /// loop would answer that fast frame, which is the point.
    #[must_use]
    pub fn shortest_hold(&self) -> Duration {
        (0..self.frames.len())
            .map(|i| self.hold(i))
            .min()
            .unwrap_or(NO_DELAY_HOLD)
    }
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
///
/// # 🚨 It is a question about ONE FRAME, and it has to stay one
///
/// F565 measured the four survivors at 92/92/99/99 — and those are frame 0.
/// Asked of every frame at source, 2026-09-07 (F643), the same four files read:
///
/// | file | frames | coherence | frames under 90 | frames touching the ceiling |
/// |---|---|---|---|---|
/// | `coder-{E,W}-attacking.gif` | 97 | 94–99 | 0 | 8 |
/// | `building-{E,W}-attacking.gif` | 241 | **71**–100 | **63** | **95** |
///
/// **So this filter, applied per frame, rejects the only art the project has.**
/// It is not measuring wrong: an attack animation throws pieces off the body
/// (coherence 71 at frame 124) and pushes an effect out through the top of its
/// own frame for two runs of ~40 frames. Both are what *attacking* looks like.
/// ▶ **The admission question is asked of frame 0 and stays there**, and
/// anything that later filters frames has to be a different question with a
/// different name.
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
    by_pose: BTreeMap<Pose, Film>,
    rejected: Vec<PathBuf>,
    unreadable: usize,
    unnamed: usize,
}

/// Whether a field wants the frames or only the first one.
///
/// 🚨 **Measured on the shipped corpus at 150 px: `Playing` costs 2.1 s and
/// 38.7 MB where `Still` costs neither** (F642). That is the whole reason this
/// is a choice rather than always the frames — `abcc paint` draws one picture
/// and would be paying two seconds for 38 MB it never looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// The first frame of each file, which is what a still is.
    Still,
    /// Every frame, ready to play.
    Playing,
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
        Poses::open_with(root, px, Motion::Still)
    }

    /// The same, playing: every frame of every picture admitted.
    ///
    /// # Errors
    ///
    /// As [`Poses::open`].
    pub fn open_playing(root: &Path, px: u32) -> Result<Poses, AssetError> {
        Poses::open_with(root, px, Motion::Playing)
    }

    /// Read a corpus, taking one frame per picture or all of them.
    ///
    /// # Errors
    ///
    /// As [`Poses::open`].
    pub fn open_with(root: &Path, px: u32, motion: Motion) -> Result<Poses, AssetError> {
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
            // 🚨 **One recipe for a film**, whichever of the two ways it was
            // asked for: a still is a film of one frame built by the same
            // constructor, so there is nowhere for the two to drift apart.
            let loaded = match motion {
                Motion::Still => load_scaled(path, px).map(Film::of),
                Motion::Playing => Film::open(path, px),
            };
            match loaded {
                Ok(film) => {
                    poses.by_pose.insert(pose, film);
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
        self.by_pose.get(&pose).map(Film::still)
    }

    /// Every frame for a pose. One frame when the corpus was opened
    /// [`Motion::Still`], so a caller that draws films works either way — it
    /// draws a still fleet rather than no fleet.
    #[must_use]
    pub fn film(&self, pose: Pose) -> Option<&Film> {
        self.by_pose.get(&pose)
    }

    /// Whether anything held has a second frame. False for a still corpus, and
    /// false for a playing one whose art is all stills.
    #[must_use]
    pub fn is_animated(&self) -> bool {
        self.by_pose.values().any(Film::is_animated)
    }

    /// What every frame held costs in RGBA.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.by_pose.values().map(Film::bytes).sum()
    }

    /// The longest loop held, which is how long the field takes to repeat
    /// itself. `None` when nothing is held.
    #[must_use]
    pub fn longest_loop(&self) -> Option<Duration> {
        self.by_pose.values().map(Film::duration).max()
    }

    /// The rate a player has to sample this field at: the shortest hold any
    /// film asks for. `None` when nothing is held.
    #[must_use]
    pub fn shortest_hold(&self) -> Option<Duration> {
        self.by_pose.values().map(Film::shortest_hold).min()
    }

    /// How many frames are held across every pose.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.by_pose.values().map(Film::len).sum()
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
        // Every frame of one film is the same size — a GIF frame is a whole
        // canvas, disposal already applied — so the first frame measures the
        // film. `films_are_one_size` pins that rather than trusting it.
        let widest = self.by_pose.values().map(|f| f.still().width()).max()?;
        let tallest = self.by_pose.values().map(|f| f.still().height()).max()?;
        Some((widest, tallest))
    }
}
