//! The sixel encoder, and the rule that makes ADR-0012 §2 a type rather than a
//! comment.
//!
//! # Why this is written here and not lifted from the spike
//!
//! The W5 spike proved sixel works in David's Windows Terminal and its encoder
//! is sitting in `research/spikes/w5-sixel/src/sixel.rs`, passing. It is not
//! copied, and the reason is in its own first line: *"modeled on `OpenAI` Codex
//! CLI's `codex-rs/tui/src/pets/sixel.rs`… its test vectors are ported below and
//! pass verbatim."* That is a derived work with ported tests, and this
//! repository has ruled twice about exactly that — **ADR-0001's *rewrite, do not
//! port*** (donors are a specification and a test corpus, never a source tree;
//! assets are the stated exception and sprites still get copied), and W12's
//! *reimplement from scratch*. `CREDITS.md` states in its own words that this is
//! a clean-room rewrite in which *"no copyright question arises"*. Porting the
//! encoder would have made that sentence false for the flagship component.
//!
//! So this is written from the **format**, which is public: DEC's sixel graphics
//! protocol, as it has been implemented by terminals for forty years. What the
//! spike contributes is what a spike is for — the *measurements* (ADR-0012):
//! 25.6–28.6 FPS end to end for a full-viewport composite, near-flat in sprite
//! count, and a ~235-sprite ceiling at 15 FPS where **screen area binds before
//! throughput does** (F144).
//!
//! # 🚨 The composite-only rule, as a type
//!
//! ADR-0012 §2: **sprites go through the composite, always.** It is measured
//! rather than stylistic — F145 found `cto-E-idle` carrying **5,062
//! semi-transparent pixels (alpha 1–127) against ~10,000 opaque**, soft alpha
//! across a third of the body rather than an anti-aliased rim. Sixel has no
//! alpha channel, so a threshold rule punches visible holes in a *floating*
//! sprite; blended onto an opaque composite the same feathering renders
//! correctly.
//!
//! ▶ That ruling is held here by the types and not by a reviewer:
//!
//! * [`Canvas`] is **opaque** — three bytes a pixel, no alpha to lose — and it
//!   is the only thing with an [`Canvas::encode`].
//! * [`Sprite`] is **RGBA** and has no path to a sixel at all. The only way its
//!   pixels reach a terminal is [`Canvas::blend`].
//!
//! A floating sprite is not a thing you can write with this module. That is the
//! same technique `line::describe` uses for event coverage and `theme` uses for
//! labels: make the rule a compile failure rather than a sentence.
//!
//! # The palette is 256 colours, and that is not a compromise
//!
//! Quantisation is **RGB332** — three bits of red, three of green, two of blue.
//! The original Command & Conquer ran in VGA mode 13h, 320×200 at 256 colours,
//! so the ceiling this imposes is the one the look is imitating.

use std::fmt;

/// `ESC`. Written as a byte rather than an escape so that nothing in this file
/// depends on a backslash surviving a copy, an editor or a heredoc.
const ESC: u8 = 0x1b;
/// `\` — the second byte of the string terminator, same reasoning as [`ESC`].
const BACKSLASH: u8 = 0x5c;

/// A sixel data byte carries six vertical pixels, offset into printable ASCII.
const SIXEL_ZERO: u8 = 0x3f;
/// Rows per band. The format's name comes from this number.
const BAND: u32 = 6;
/// Below this, `!<n><char>` costs more than repeating the character.
const RUN_WORTH_ENCODING: u32 = 4;

/// What can go wrong building an image, which is only ever a caller handing over
/// a buffer that does not match the size it also handed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SixelError {
    /// Zero in either dimension. A terminal is entitled to reject it and the
    /// encoder would emit a header describing nothing.
    Empty,
    /// The buffer length does not match `width * height * channels`.
    Mismatched {
        width: u32,
        height: u32,
        expected: usize,
        got: usize,
    },
}

impl fmt::Display for SixelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SixelError::Empty => {
                f.write_str("an image with a zero dimension has nothing to encode")
            }
            SixelError::Mismatched {
                width,
                height,
                expected,
                got,
            } => write!(
                f,
                "a {width}x{height} image needs {expected} bytes and {got} were given"
            ),
        }
    }
}

impl std::error::Error for SixelError {}

/// One sprite's pixels, with their alpha kept.
///
/// 🚨 **There is deliberately no way to turn this into a sixel.** See the module
/// docs: F145 measured the feathering that makes a floating sprite wrong, and
/// the only exit from this type is [`Canvas::blend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sprite {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Sprite {
    /// Wrap a decoded RGBA buffer.
    ///
    /// # Errors
    ///
    /// [`SixelError`] if the buffer does not match the dimensions given.
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Sprite, SixelError> {
        check(width, height, rgba.len(), 4)?;
        Ok(Sprite {
            width,
            height,
            rgba,
        })
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// How many pixels are neither fully opaque nor fully transparent.
    ///
    /// This is F145's instrument, kept because the finding it produced is the
    /// reason for the whole composite rule and a new sprite in the corpus should
    /// be able to be asked the same question rather than assumed to be like the
    /// last one. On `cto-E-idle` the answer was **5,062**.
    #[must_use]
    pub fn feathered(&self) -> usize {
        self.rgba
            .chunks_exact(4)
            .filter(|px| px[3] > 0 && px[3] < 255)
            .count()
    }
}

/// The battlefield: opaque, three bytes a pixel, and the only thing that becomes
/// a sixel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canvas {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl Canvas {
    /// A canvas of one colour.
    ///
    /// # Errors
    ///
    /// [`SixelError::Empty`] if either dimension is zero.
    pub fn filled(width: u32, height: u32, colour: [u8; 3]) -> Result<Canvas, SixelError> {
        if width == 0 || height == 0 {
            return Err(SixelError::Empty);
        }
        let n = width as usize * height as usize;
        let mut rgb = Vec::with_capacity(n * 3);
        for _ in 0..n {
            rgb.extend_from_slice(&colour);
        }
        Ok(Canvas { width, height, rgb })
    }

    /// Wrap an existing opaque buffer — a decoded backdrop, typically.
    ///
    /// # Errors
    ///
    /// [`SixelError`] if the buffer does not match the dimensions given.
    pub fn from_rgb(width: u32, height: u32, rgb: Vec<u8>) -> Result<Canvas, SixelError> {
        check(width, height, rgb.len(), 3)?;
        Ok(Canvas { width, height, rgb })
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The pixels, for a test or a screenshot comparison.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.rgb
    }

    /// 🚨 **The only way a sprite's pixels reach a terminal**, and the whole of
    /// ADR-0012 §2 in one method.
    ///
    /// Source-over alpha compositing at 8 bits, rounded rather than truncated:
    /// `(src·a + dst·(255−a) + 127) / 255`. The rounding matters at the scale
    /// this runs at — a sprite is composited every frame, and truncation biases
    /// every feathered pixel one step dark, which over 5,062 of them on a single
    /// sprite is a visible rim.
    ///
    /// `x` and `y` are the top-left corner in canvas pixels and may be negative;
    /// anything outside the canvas is clipped rather than wrapped, because a
    /// unit walking off the left edge is a normal thing on a battlefield and a
    /// panic is not.
    pub fn blend(&mut self, sprite: &Sprite, x: i32, y: i32) {
        // ⚠ The arithmetic is done in `i64` and narrowed by `try_from` rather
        // than cast. A sprite at `i32::MIN` is not a real placement, but an
        // overflow here would wrap it onto the canvas — a unit teleporting to
        // the far edge instead of walking off this one.
        for sy in 0..sprite.height {
            let Ok(dy) = u32::try_from(i64::from(y) + i64::from(sy)) else {
                continue;
            };
            if dy >= self.height {
                continue;
            }
            for sx in 0..sprite.width {
                let Ok(dx) = u32::try_from(i64::from(x) + i64::from(sx)) else {
                    continue;
                };
                if dx >= self.width {
                    continue;
                }
                let s = (sy as usize * sprite.width as usize + sx as usize) * 4;
                let alpha = u32::from(sprite.rgba[s + 3]);
                if alpha == 0 {
                    continue;
                }
                let d = (dy as usize * self.width as usize + dx as usize) * 3;
                if alpha == 255 {
                    self.rgb[d] = sprite.rgba[s];
                    self.rgb[d + 1] = sprite.rgba[s + 1];
                    self.rgb[d + 2] = sprite.rgba[s + 2];
                    continue;
                }
                for c in 0..3 {
                    let src = u32::from(sprite.rgba[s + c]);
                    let dst = u32::from(self.rgb[d + c]);
                    // Cannot exceed 255: both terms are weighted by shares that
                    // sum to 255. `try_from` says so rather than a comment.
                    let out = (src * alpha + dst * (255 - alpha) + 127) / 255;
                    self.rgb[d + c] = u8::try_from(out).unwrap_or(u8::MAX);
                }
            }
        }
    }

    /// Encode the canvas as one sixel image, reusing the encoder's buffers.
    #[must_use]
    pub fn encode(&self, encoder: &mut Encoder) -> Vec<u8> {
        encoder.encode(self)
    }
}

/// Reusable scratch, kept alive across frames.
///
/// A battlefield redraws at a C&C-style 12–15 FPS, so the allocations that
/// matter are the ones that would happen every frame. Nothing here is per-frame
/// state: [`Encoder::encode`] leaves no meaning behind it, only capacity.
#[derive(Debug, Default)]
pub struct Encoder {
    /// One palette index per pixel.
    index: Vec<u8>,
    /// `256 * width` column masks for the band being written.
    masks: Vec<u8>,
    /// Which of the 256 entries the current band actually uses.
    used: Vec<u16>,
    /// Membership test backing `used`, so a band is O(pixels) and not O(256²).
    seen: Vec<bool>,
    out: Vec<u8>,
}

impl Encoder {
    #[must_use]
    pub fn new() -> Encoder {
        Encoder::default()
    }

    /// The whole image, from `ESC P` to `ESC \`.
    fn encode(&mut self, canvas: &Canvas) -> Vec<u8> {
        let width = canvas.width as usize;
        let height = canvas.height;

        self.index.clear();
        self.index.extend(canvas.rgb.chunks_exact(3).map(quantise));

        self.masks.clear();
        self.masks.resize(256 * width, 0);
        self.seen.clear();
        self.seen.resize(256, false);
        self.out.clear();

        self.out.push(ESC);
        // P1 = 0 (the terminal's default aspect), P2 = 0 (unset pixels take the
        // background — every pixel here is set, so this only states the intent),
        // P3 = 0 (no grid size opinion).
        self.out.extend_from_slice(b"P0;0;0q");
        // Raster attributes: 1:1 pixels, and the size, so a terminal can size the
        // region before it has parsed the data.
        self.out.extend_from_slice(b"\"1;1;");
        push_u32(&mut self.out, canvas.width);
        self.out.push(b';');
        push_u32(&mut self.out, height);

        self.define_palette();

        let bands = height.div_ceil(BAND);
        for band in 0..bands {
            self.write_band(canvas, band, width);
            // ⚠ No trailing newline after the final band: a terminal that honours
            // it scrolls the line under the image, which on a full-viewport
            // composite is a visible jump every frame.
            if band + 1 < bands {
                self.out.push(b'-');
            }
        }

        self.out.push(ESC);
        self.out.push(BACKSLASH);
        self.out.clone()
    }

    /// All 256 entries, up front.
    ///
    /// Defining the whole palette rather than only the colours in this frame
    /// costs about 4 KB once and buys something worth more at 15 FPS: **every
    /// frame's palette is identical**, so a terminal is never redefining a
    /// register mid-animation and the encoder needs no per-frame colour census.
    fn define_palette(&mut self) {
        for entry in 0u8..=u8::MAX {
            let [r, g, b] = expand(entry);
            self.out.push(b'#');
            push_u32(&mut self.out, u32::from(entry));
            // `2` selects RGB, and its arguments are percentages rather than
            // bytes — the one place this format surprises people.
            self.out.extend_from_slice(b";2;");
            push_u32(&mut self.out, percent(r));
            self.out.push(b';');
            push_u32(&mut self.out, percent(g));
            self.out.push(b';');
            push_u32(&mut self.out, percent(b));
        }
    }

    /// One band of six rows: build a column mask per colour, then emit each
    /// colour's row.
    fn write_band(&mut self, canvas: &Canvas, band: u32, width: usize) {
        self.used.clear();
        for entry in &mut self.seen {
            *entry = false;
        }

        let top = band * BAND;
        let bottom = (top + BAND).min(canvas.height);
        for y in top..bottom {
            let bit = 1u8 << (y - top);
            let row = y as usize * width;
            for x in 0..width {
                let colour = self.index[row + x];
                let slot = colour as usize * width + x;
                if self.masks[slot] == 0 && !self.seen[colour as usize] {
                    self.seen[colour as usize] = true;
                    self.used.push(u16::from(colour));
                }
                self.masks[slot] |= bit;
            }
        }
        // A band's colours come out in palette order rather than first-seen
        // order, so one canvas always encodes to one byte string.
        self.used.sort_unstable();

        let count = self.used.len();
        for i in 0..count {
            let colour = self.used[i];
            self.out.push(b'#');
            push_u32(&mut self.out, u32::from(colour));
            let base = colour as usize * width;
            emit_run_length(&mut self.out, &self.masks[base..base + width]);
            // Carriage return between colours: the next colour overprints the
            // same band. The last one does not need it.
            if i + 1 < count {
                self.out.push(b'$');
            }
        }

        // Clear only what was touched. Zeroing all 256 rows would make an empty
        // band cost as much as a full one.
        for &colour in &self.used {
            let base = colour as usize * width;
            self.masks[base..base + width].fill(0);
        }
    }
}

/// One colour's row of column masks, run-length encoded.
///
/// ⚠ **Trailing empty columns are dropped.** A colour that stops halfway across
/// the band has nothing to say about the rest of it, and on a battlefield — where
/// most colours occupy a small part of any given band — this is most of the
/// output. What it costs is that a decoder must treat a short row as
/// zero-filled, which the format already requires.
fn emit_run_length(out: &mut Vec<u8>, masks: &[u8]) {
    let end = masks.iter().rposition(|m| *m != 0).map_or(0, |i| i + 1);
    let mut x = 0;
    while x < end {
        let value = masks[x];
        let mut run = 1;
        while x + run < end && masks[x + run] == value {
            run += 1;
        }
        let ch = SIXEL_ZERO + value;
        let run32 = u32::try_from(run).unwrap_or(u32::MAX);
        if run32 >= RUN_WORTH_ENCODING {
            out.push(b'!');
            push_u32(out, run32);
            out.push(ch);
        } else {
            for _ in 0..run {
                out.push(ch);
            }
        }
        x += run;
    }
}

/// RGB332: three bits of red, three of green, two of blue.
fn quantise(px: &[u8]) -> u8 {
    (px[0] & 0xe0) | ((px[1] & 0xe0) >> 3) | (px[2] >> 6)
}

/// A palette entry back to the colour it stands for, spread across the full
/// range so that entry 255 is white rather than nearly-white.
fn expand(entry: u8) -> [u8; 3] {
    // Each channel is a small numerator over its own maximum, so every result is
    // in 0..=255 by construction. `u16` and a divide keeps it there without a
    // cast anyone has to check.
    let spread = |bits: u8, max: u16| -> u8 {
        let v = (u16::from(bits) * 255) / max;
        u8::try_from(v).unwrap_or(u8::MAX)
    };
    [
        spread((entry >> 5) & 7, 7),
        spread((entry >> 2) & 7, 7),
        spread(entry & 3, 3),
    ]
}

/// A byte as the percentage this format wants, rounded.
fn percent(v: u8) -> u32 {
    (u32::from(v) * 100 + 127) / 255
}

fn push_u32(out: &mut Vec<u8>, mut v: u32) {
    if v == 0 {
        out.push(b'0');
        return;
    }
    let mut digits = [0u8; 10];
    let mut n = 0;
    while v > 0 {
        digits[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    for i in (0..n).rev() {
        out.push(digits[i]);
    }
}

fn check(width: u32, height: u32, got: usize, channels: usize) -> Result<(), SixelError> {
    if width == 0 || height == 0 {
        return Err(SixelError::Empty);
    }
    let expected = width as usize * height as usize * channels;
    if expected != got {
        return Err(SixelError::Mismatched {
            width,
            height,
            expected,
            got,
        });
    }
    Ok(())
}
