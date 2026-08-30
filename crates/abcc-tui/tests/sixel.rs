//! The sixel encoder, checked against the format rather than against another
//! implementation.
//!
//! 🚨 **No ported test vectors.** The spike's encoder passes `OpenAI` Codex CLI's
//! vectors verbatim, which is exactly why neither it nor they are here — see the
//! module docs and ADR-0001. Every expectation below is derived from the sixel
//! format's own rules: six rows to a band, a data byte is `0x3f` plus a six-bit
//! column mask, `$` returns to the start of the band, `-` starts the next one,
//! and `#n;2;r;g;b` defines a colour in **percentages**.
//!
//! The palette is emitted in full every frame, so a golden for the whole output
//! would be four kilobytes of colour definitions and one byte of picture. The
//! assertions are therefore on the head, the tail and the parts that carry the
//! image.

use abcc_tui::sixel::{Canvas, Encoder, SixelError, Sprite};

/// `ESC P 0;0;0 q "1;1;<w>;<h>`
fn head(width: u32, height: u32) -> Vec<u8> {
    let mut v = vec![0x1b];
    v.extend_from_slice(b"P0;0;0q\"1;1;");
    v.extend_from_slice(width.to_string().as_bytes());
    v.push(b';');
    v.extend_from_slice(height.to_string().as_bytes());
    v
}

/// `ESC \`
const TAIL: [u8; 2] = [0x1b, 0x5c];

/// A channel value from a generated gradient, saturating rather than wrapping —
/// a wrap here would put a bright pixel where the test expects a dark one and
/// the failure would look like an encoder bug.
fn byte(v: u32) -> u8 {
    u8::try_from(v).unwrap_or(u8::MAX)
}

fn red(width: u32, height: u32) -> Canvas {
    Canvas::filled(width, height, [255, 0, 0]).expect("canvas")
}

/// Everything after the 256 palette definitions — the picture itself.
fn picture(encoded: &[u8]) -> Vec<u8> {
    // The last definition is entry 255, and it is the only place `#255;2;` can
    // appear: a colour *selection* is `#255` with no semicolon after it.
    let marker = b"#255;2;";
    let at = encoded
        .windows(marker.len())
        .position(|w| w == marker)
        .expect("the palette was not written");
    let after = encoded[at + marker.len()..]
        .iter()
        .position(|b| *b == b'#')
        .expect("nothing followed the palette");
    encoded[at + marker.len() + after..].to_vec()
}

// ---------------------------------------------------------------------------
// the format
// ---------------------------------------------------------------------------

/// One red pixel: the smallest complete image the format can carry.
///
/// Red is palette entry `224` — `0b111_000_00` under RGB332 — and it expands to
/// 100% red, which is the percentage surprise the format is known for.
#[test]
fn one_red_pixel_is_a_header_a_palette_a_single_sixel_and_a_terminator() {
    let mut encoder = Encoder::new();
    let out = red(1, 1).encode(&mut encoder);

    assert!(
        out.starts_with(&head(1, 1)),
        "{:?}",
        &out[..24.min(out.len())]
    );
    assert!(out.ends_with(&TAIL));

    let defined = b"#224;2;100;0;0";
    assert!(
        out.windows(defined.len()).any(|w| w == defined),
        "red is not defined as 100% red"
    );

    // `@` is 0x3f + 0b000001 — the top row of the band, and only it.
    assert_eq!(picture(&out), b"#224@\x1b\\".to_vec());
}

/// A run of four is worth encoding; a run of three is not.
///
/// `!4@` is three bytes against `@@@@`'s four. At three the escape breaks even
/// on length and costs a decoder a parse, so it is not used.
#[test]
fn a_run_is_encoded_only_when_it_is_shorter_than_repeating_the_byte() {
    let mut encoder = Encoder::new();

    let four = red(4, 1).encode(&mut encoder);
    assert_eq!(picture(&four), b"#224!4@\x1b\\".to_vec());

    let three = red(3, 1).encode(&mut encoder);
    assert_eq!(picture(&three), b"#224@@@\x1b\\".to_vec());
}

/// Two colours in one band overprint it, separated by a carriage return, and
/// they come out in palette order so that one canvas has one encoding.
///
/// ⚠ Trailing empty columns are dropped and leading ones are not: blue starts
/// at column 4, so it has to say so, while red ends at column 3 and simply
/// stops.
#[test]
fn two_colours_share_a_band_and_the_second_returns_to_its_start() {
    let mut rgb = Vec::new();
    for x in 0..8 {
        rgb.extend_from_slice(if x < 4 { &[255, 0, 0] } else { &[0, 0, 255] });
    }
    let canvas = Canvas::from_rgb(8, 1, rgb).expect("canvas");
    let out = canvas.encode(&mut Encoder::new());

    // Blue is entry 3, red is entry 224, and 3 sorts first.
    assert_eq!(picture(&out), b"#3!4?!4@$#224!4@\x1b\\".to_vec());
}

/// Seven rows is two bands, and only the join between them carries a newline.
///
/// ⚠ There is no trailing `-` after the last band: a terminal that honours one
/// scrolls a line in under the image, which at 15 FPS is a jump every frame.
#[test]
fn a_seventh_row_starts_a_second_band_and_the_last_band_has_no_newline() {
    let out = red(1, 7).encode(&mut Encoder::new());
    // Band 0: all six rows set — 0b111111 = 63, so 0x3f + 63 = 0x7e, `~`.
    // Band 1: the top row only.
    assert_eq!(picture(&out), b"#224~-#224@\x1b\\".to_vec());
}

// ---------------------------------------------------------------------------
// 🚨 the scratch buffers, which are the only thing here that can go stale
// ---------------------------------------------------------------------------

/// 🚨 **A reused encoder must produce what a fresh one would.**
///
/// This is the test the design actually needs. [`Encoder`] keeps its per-band
/// masks alive across frames and clears **only the colours a band touched** —
/// zeroing all 256 rows would make an empty band cost what a full one does. That
/// optimisation is exactly the kind that works until the second frame, so the
/// second frame is what is asserted: encode a wide multi-colour image, then a
/// different one, and require the result to be byte-identical to a first
/// encoding of the same thing.
#[test]
fn an_encoder_carries_no_pixels_from_the_frame_before() {
    let busy = {
        let mut rgb = Vec::new();
        for y in 0..13u32 {
            for x in 0..40u32 {
                rgb.extend_from_slice(&[byte(x * 6), byte(y * 19), byte(x * y)]);
            }
        }
        Canvas::from_rgb(40, 13, rgb).expect("canvas")
    };
    let sparse = red(40, 13);

    let mut reused = Encoder::new();
    let _ = busy.encode(&mut reused);
    let second = sparse.encode(&mut reused);

    let fresh = sparse.encode(&mut Encoder::new());
    assert_eq!(
        second, fresh,
        "the encoder rendered a different image on its second use"
    );

    // And the other order, because a sparse frame leaves different residue than
    // a busy one.
    let mut reused = Encoder::new();
    let _ = sparse.encode(&mut reused);
    assert_eq!(busy.encode(&mut reused), busy.encode(&mut Encoder::new()));
}

/// The same canvas twice through the same encoder is the same bytes.
#[test]
fn encoding_is_deterministic() {
    let canvas = red(9, 9);
    let mut encoder = Encoder::new();
    assert_eq!(canvas.encode(&mut encoder), canvas.encode(&mut encoder));
}

// ---------------------------------------------------------------------------
// 🚨 ADR-0012 §2 — the composite, which is the ruling F145 paid for
// ---------------------------------------------------------------------------

/// A sprite blended at full alpha replaces; at zero it does nothing.
#[test]
fn alpha_nothing_and_alpha_everything_are_the_two_ends_of_the_same_blend() {
    let mut canvas = Canvas::filled(2, 1, [0, 0, 0]).expect("canvas");
    let sprite =
        Sprite::from_rgba(2, 1, vec![255, 255, 255, 255, 255, 255, 255, 0]).expect("sprite");
    canvas.blend(&sprite, 0, 0);
    assert_eq!(canvas.pixels(), &[255, 255, 255, 0, 0, 0]);
}

/// 🚨 **The feathered pixels F145 measured, blended rather than thresholded.**
///
/// A threshold-128 rule would make this pixel either fully white or fully
/// absent. Composited it is the midpoint, which is what makes a soft edge look
/// like a soft edge instead of a hole.
///
/// ⚠ The arithmetic rounds rather than truncates. Truncation biases every
/// feathered pixel one step dark, and F145 counted **5,062** of them on a single
/// sprite.
#[test]
fn a_half_transparent_pixel_lands_halfway_and_is_rounded() {
    let mut canvas = Canvas::filled(1, 1, [0, 0, 0]).expect("canvas");
    let sprite = Sprite::from_rgba(1, 1, vec![255, 255, 255, 128]).expect("sprite");
    canvas.blend(&sprite, 0, 0);
    // (255*128 + 0*127 + 127) / 255 = 128, where truncation would give 128 too;
    // the case that separates them is an odd destination.
    assert_eq!(canvas.pixels(), &[128, 128, 128]);

    let mut canvas = Canvas::filled(1, 1, [10, 10, 10]).expect("canvas");
    let sprite = Sprite::from_rgba(1, 1, vec![0, 0, 0, 1]).expect("sprite");
    canvas.blend(&sprite, 0, 0);
    // 10*254/255 = 9.96 — rounds back to 10, where truncation would darken a
    // pixel that is 99.6% background.
    assert_eq!(canvas.pixels(), &[10, 10, 10]);
}

/// A unit walking off the edge is a normal thing on a battlefield.
#[test]
fn a_sprite_placed_off_the_canvas_is_clipped_and_never_wraps() {
    let mut canvas = Canvas::filled(3, 1, [0, 0, 0]).expect("canvas");
    let sprite = Sprite::from_rgba(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 255]).expect("sprite");

    // Half off the left: only the second sprite column lands, at column 0.
    canvas.blend(&sprite, -1, 0);
    assert_eq!(canvas.pixels(), &[0, 255, 0, 0, 0, 0, 0, 0, 0]);

    // Entirely past the right, and entirely below: nothing changes, no panic.
    let before = canvas.pixels().to_vec();
    canvas.blend(&sprite, 99, 0);
    canvas.blend(&sprite, 0, 99);
    canvas.blend(&sprite, 0, -99);
    assert_eq!(canvas.pixels(), &before[..]);
}

/// F145's instrument, kept so a new sprite can be asked rather than assumed.
#[test]
fn the_feather_count_is_the_measurement_that_produced_the_composite_rule() {
    let sprite = Sprite::from_rgba(
        4,
        1,
        vec![
            0, 0, 0, 0, // fully transparent — not feathered
            0, 0, 0, 255, // fully opaque — not feathered
            0, 0, 0, 1, // feathered
            0, 0, 0, 254, // feathered
        ],
    )
    .expect("sprite");
    assert_eq!(sprite.feathered(), 2);
}

// ---------------------------------------------------------------------------
// what a caller can get wrong
// ---------------------------------------------------------------------------

#[test]
fn a_buffer_that_does_not_match_its_dimensions_is_refused_with_both_numbers() {
    let err = Sprite::from_rgba(2, 2, vec![0; 8]).expect_err("should refuse");
    assert_eq!(
        err,
        SixelError::Mismatched {
            width: 2,
            height: 2,
            expected: 16,
            got: 8,
        }
    );
    // The message carries the numbers, because "invalid buffer" is not something
    // an operator can act on.
    assert!(err.to_string().contains("16"), "{err}");

    assert_eq!(Canvas::filled(0, 4, [0, 0, 0]), Err(SixelError::Empty));
    assert_eq!(Sprite::from_rgba(4, 0, vec![]), Err(SixelError::Empty));
}

// ---------------------------------------------------------------------------
// 🚨 the round trip, and how its bit order was settled
// ---------------------------------------------------------------------------

/// Decode a sixel stream back to palette indices, written from the format so
/// that it can disagree with the encoder.
///
/// 🚨 **The one thing an author cannot check against themselves is BIT ORDER** —
/// whether bit 0 of a data byte is the top row of the band or the bottom. An
/// encoder and a decoder written together share that mistake and round-trip
/// perfectly while a terminal draws scrambled bands.
///
/// ▶ **It was settled against a different implementation, on 2026-08-30.** The
/// W5 spike's encoder (`research/spikes/w5-sixel`) is proven to render on
/// David's own Windows Terminal — that is what the spike passing all five
/// questions means. Its `hello` output was captured to a file and decoded under
/// this convention: it came back **240x96, with rows 0 and 1 uniform yellow and
/// a 34-colour gradient below**, which is the yellow-bordered box the spike
/// documents. Under the opposite convention the border would have landed at rows
/// 4-5. That is the donor used the way ADR-0001 allows — **as a test corpus, by
/// its output, never by its source.**
fn unsixel(stream: &[u8]) -> (u32, u32, Vec<Vec<Option<u16>>>) {
    let mut i = 0;
    // Skip to `ESC P`, past any text or other escapes ahead of it.
    while i + 1 < stream.len() && !(stream[i] == 0x1b && stream[i + 1] == b'P') {
        i += 1;
    }
    i += 2;
    while i < stream.len() && stream[i] != b'q' {
        i += 1;
    }
    i += 1;

    let number = |at: &mut usize| -> u32 {
        let mut v = 0u32;
        while *at < stream.len() && stream[*at].is_ascii_digit() {
            v = v * 10 + u32::from(stream[*at] - b'0');
            *at += 1;
        }
        v
    };

    let (mut width, mut height) = (0u32, 0u32);
    let mut rows: Vec<Vec<Option<u16>>> = Vec::new();
    let mut colour = 0u16;
    let mut x = 0usize;
    let mut band_top = 0usize;

    while i < stream.len() {
        match stream[i] {
            0x1b => break,
            b'"' => {
                i += 1;
                let mut nums = Vec::new();
                loop {
                    nums.push(number(&mut i));
                    if i < stream.len() && stream[i] == b';' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                if nums.len() >= 4 {
                    width = nums[2];
                    height = nums[3];
                }
            }
            b'#' => {
                i += 1;
                colour = u16::try_from(number(&mut i)).unwrap_or(u16::MAX);
                // A definition carries `;2;r;g;b`; a selection does not.
                if i < stream.len() && stream[i] == b';' {
                    while i < stream.len() && (stream[i] == b';' || stream[i].is_ascii_digit()) {
                        i += 1;
                    }
                }
            }
            b'$' => {
                x = 0;
                i += 1;
            }
            b'-' => {
                band_top += 6;
                x = 0;
                i += 1;
            }
            b'!' => {
                i += 1;
                let run = number(&mut i);
                let bits = stream[i] - 0x3f;
                i += 1;
                put(&mut rows, band_top, x, bits, run, colour);
                x += run as usize;
            }
            c if (0x3f..=0x7e).contains(&c) => {
                put(&mut rows, band_top, x, c - 0x3f, 1, colour);
                x += 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    (width, height, rows)
}

/// Paint one data byte's six vertical pixels. A free function rather than a
/// closure so that `band_top` stays assignable while it is in use.
fn put(
    rows: &mut Vec<Vec<Option<u16>>>,
    band_top: usize,
    at: usize,
    bits: u8,
    run: u32,
    colour: u16,
) {
    for k in 0..run as usize {
        for bit in 0..6usize {
            if bits & (1 << bit) == 0 {
                continue;
            }
            let y = band_top + bit;
            while rows.len() <= y {
                rows.push(Vec::new());
            }
            let row = &mut rows[y];
            while row.len() <= at + k {
                row.push(None);
            }
            row[at + k] = Some(colour);
        }
    }
}

/// RGB332, the encoder's quantisation, restated here rather than shared — a test
/// that calls the code under test to compute its own expectation checks nothing.
fn quantised(px: &[u8]) -> u16 {
    u16::from((px[0] & 0xe0) | ((px[1] & 0xe0) >> 3) | (px[2] >> 6))
}

/// 🚨 **Every pixel of a composited battlefield survives the encode.**
///
/// The picture has vertical structure on purpose: a uniform rule on row 0 with a
/// gradient beneath it, so a band written upside down fails rather than merely
/// looking different. A half-transparent sprite is composited into it, so the
/// ADR-0012 §2 path is in the byte stream and not only in a unit test.
///
/// ⚠ **The comparison is palette index, not colour.** Sixel defines a colour
/// register in *percentages* — about 101 levels a channel — so only **32 of the
/// 256** RGB332 entries are exactly representable and the rest land within
/// 1/255. Measured 2026-08-30, over the whole palette. Nothing anywhere should
/// claim a sixel reproduces its source byte for byte.
#[test]
fn every_pixel_of_a_composited_canvas_survives_the_round_trip() {
    let (w, h) = (64u32, 20u32);
    let mut rgb = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if y == 0 {
                rgb.extend_from_slice(&[255, 255, 0]);
            } else {
                rgb.extend_from_slice(&[byte(x * 4), byte(y * 12), 64]);
            }
        }
    }
    let mut canvas = Canvas::from_rgb(w, h, rgb).expect("canvas");
    let sprite = Sprite::from_rgba(8, 8, [0, 255, 0, 128].repeat(64)).expect("sprite");
    canvas.blend(&sprite, 4, 8);

    let stream = canvas.encode(&mut Encoder::new());
    let (dw, dh, rows) = unsixel(&stream);
    assert_eq!((dw, dh), (w, h), "the raster attributes lost the size");

    let mut checked = 0;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let at = (y * w as usize + x) * 3;
            let want = quantised(&canvas.pixels()[at..at + 3]);
            let got = rows
                .get(y)
                .and_then(|r| r.get(x).copied().flatten())
                .unwrap_or_else(|| panic!("no pixel decoded at ({x},{y})"));
            assert_eq!(got, want, "at ({x},{y})");
            checked += 1;
        }
    }
    assert_eq!(checked, (w * h) as usize);

    // Row 0 is the rule and row 1 is not: the assertion that fails if a band is
    // written bottom-up.
    let rule = rows[0][0];
    assert!(rows[0].iter().all(|c| *c == rule), "row 0 is not uniform");
    assert!(rows[1].iter().any(|c| *c != rule), "row 1 matches the rule");
}
