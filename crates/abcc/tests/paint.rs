//! `abcc paint --play` over a real repository, a real log and a real corpus.
//!
//! 🚨 **The player is the one part of `paint` that cannot be checked by reading
//! it.** A still is a function of its inputs; a player is a loop with a clock in
//! it, and the three things it can get wrong are invisible in a screenshot: it
//! can draw one frame over and over, it can walk down the terminal instead of
//! redrawing in place, and it can leave the caret inside the picture. All three
//! are in the bytes, so all three are asserted here.
//!
//! ⚠ **What this cannot answer is whether the animation is smooth.** Nothing
//! draws in a test — `is_terminal` is false, which the report itself says — so
//! the end-to-end rate still needs an operator in a sixel terminal.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as OsCommand;
use std::time::Duration;

use abcc::cli::{Command, Invocation, Paint};

// ---------------------------------------------------------------------------
// the fixture
// ---------------------------------------------------------------------------

fn git(cwd: &Path, args: &[&str]) {
    let out = OsCommand::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Subject {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    sprites: PathBuf,
}

/// A repository with one commit, a state directory beside it, and a corpus of
/// one animated pose.
fn subject() -> Subject {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("subject");
    fs::create_dir_all(&root).expect("mkdir");
    git(dir.path(), &["init", "-q", "-b", "main", "subject"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(&root, &["config", "user.name", "test"]);
    fs::write(root.join("src.rs"), "pub fn one() -> u32 { 1 }\n").expect("write");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "first"]);

    let sprites = dir.path().join("sprites");
    fs::create_dir_all(&sprites).expect("mkdir");
    // 🚨 **A figure, not a block**: a solid rectangle fills its own top row and
    // the corpus filter refuses art that touches its ceiling (F565). Three
    // frames of three colours, so a player that redraws frame 0 forever is
    // caught by the bytes differing.
    //
    // 🚨 **All four poses, because a corpus missing one draws nothing rather
    // than something else** — and the first version of this fixture held only
    // the east-facing coder, so the single unit on the board (which faces west,
    // `roster::face`) had no picture and every frame was bare ground. The
    // player was fine; the test was measuring an empty field.
    for name in [
        "coder-E-attacking.gif",
        "coder-W-attacking.gif",
        "building-E-attacking.gif",
        "building-W-attacking.gif",
    ] {
        a_gif(
            &sprites.join(name),
            &[[200, 40, 40, 255], [40, 200, 40, 255], [40, 40, 200, 255]],
        );
    }
    Subject {
        home: dir.path().join("state"),
        _dir: dir,
        root,
        sprites,
    }
}

fn a_gif(path: &Path, colours: &[[u8; 4]]) {
    let file = fs::File::create(path).expect("create");
    let mut encoder = image::codecs::gif::GifEncoder::new(std::io::BufWriter::new(file));
    encoder
        .set_repeat(image::codecs::gif::Repeat::Infinite)
        .expect("repeat");
    for colour in colours {
        let mut buffer = image::RgbaImage::new(16, 16);
        for (_, y, px) in buffer.enumerate_pixels_mut() {
            px.0 = if y == 0 { [0, 0, 0, 0] } else { *colour };
        }
        encoder
            .encode_frame(image::Frame::from_parts(
                buffer,
                0,
                0,
                image::Delay::from_numer_denom_ms(40, 1),
            ))
            .expect("frame");
    }
}

impl Subject {
    fn run(&self, command: Command) -> Vec<u8> {
        let invocation = Invocation {
            repo: Some(self.root.clone()),
            home: Some(self.home.clone()),
            command,
        };
        let mut out = Vec::new();
        abcc::dispatch(&invocation, &mut out).expect("paint");
        out
    }

    fn paint(&self, play: Option<Duration>) -> Vec<u8> {
        self.run(Command::Paint(Paint {
            sprites: Some(self.sprites.display().to_string()),
            px: 60,
            size: (320, 200),
            corpus: false,
            play,
            cell: 20,
        }))
    }

    /// A task on the board, so a unit stands on the field.
    fn queue(&self, prompt: &str) {
        self.run(Command::Task {
            prompt: prompt.to_owned(),
            title: None,
        });
    }
}

/// Every sixel payload written, in order: what landed between a cursor save and
/// the matching restore.
///
/// ⚠ **Not a split on `ESC`.** A sixel payload is `ESC P ... ESC \` and carries
/// its own escapes, so splitting the stream on `0x1b` and keeping the parts that
/// begin with `7` hands back the empty string between the save and the payload's
/// own introducer — every frame identical, which is exactly the failure this
/// test exists to catch. It found the test instead.
fn frames(out: &[u8]) -> Vec<&[u8]> {
    let mut found = Vec::new();
    let mut at = 0usize;
    while let Some(start) = out[at..].windows(2).position(|w| w == b"7") {
        let open = at + start + 2;
        let Some(end) = out[open..].windows(2).position(|w| w == b"8") else {
            break;
        };
        found.push(&out[open..open + end]);
        at = open + end + 2;
    }
    found
}

fn count(out: &[u8], needle: &[u8]) -> usize {
    out.windows(needle.len()).filter(|w| *w == needle).count()
}

// ---------------------------------------------------------------------------

/// 🚨 **The picture has to change, and it has to change in place.**
///
/// Three assertions, one per way a player fails silently: more than one distinct
/// payload (it is animating rather than redrawing frame 0), a save for every
/// restore (it returns the caret rather than walking down the terminal), and the
/// rows reserved before the run are the rows stepped over after it.
#[test]
fn the_player_redraws_in_place_and_the_frames_differ() {
    let subject = subject();
    subject.queue("a task, so that something stands on the field");
    let out = subject.paint(Some(Duration::from_millis(400)));

    let drawn = frames(&out);
    assert!(
        drawn.len() >= 4,
        "a 400 ms run at 40 ms a frame drew {} frame(s)",
        drawn.len()
    );
    let distinct: std::collections::BTreeSet<&[u8]> = drawn.iter().copied().collect();
    assert!(
        distinct.len() >= 2,
        "every frame is identical: the play head is not moving"
    );
    assert!(
        distinct.len() <= 3,
        "a three-frame film drew {} different pictures",
        distinct.len()
    );

    assert_eq!(
        count(&out, b"\x1b7"),
        count(&out, b"\x1b8"),
        "a cursor save without its restore leaves the caret in the picture"
    );
    // 200 px of field at 20 px a cell is ten rows, reserved before and stepped
    // over after. A player that got this wrong would scroll the report away.
    assert_eq!(count(&out, b"\x1b[10A"), 1, "the rows were not reserved");
    assert_eq!(
        count(&out, b"\x1b[10B"),
        1,
        "the report is inside the picture"
    );

    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("one frame every 40 ms"), "{text:.400}");
    assert!(text.contains("FPS achieved against 25.0 asked"));
    assert!(
        text.contains("stdout is NOT a terminal here"),
        "the report has to say nothing drew it"
    );
    // The legend is still under the picture: a player is not a different view.
    assert!(text.contains("a task, so that something stands on the field"));
    // 🚨 **And the fixture is complete**, which is the other half of the trap
    // above: with only the east-facing coder in the corpus, the one unit on the
    // board has no picture, the field is bare ground and every frame is
    // identical for a reason that has nothing to do with the player. Checked
    // by removing the other three, 2026-09-07 — it fails exactly that way.
    assert!(
        !text.contains("have no picture"),
        "the corpus is missing a pose, so this is measuring an empty field"
    );
}

/// The still is one frame and writes none of the player's cursor discipline —
/// the default did not quietly become an animation.
#[test]
fn without_play_it_is_still_one_frame() {
    let subject = subject();
    subject.queue("a task");
    let out = subject.paint(None);

    assert_eq!(count(&out, b"\x1b7"), 0, "the still moved the cursor");
    assert_eq!(frames(&out).len(), 0);
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("the field, from the log"));
    assert!(!text.contains("FPS"));
}
