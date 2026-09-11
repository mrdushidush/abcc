//! The denylist: what it removes, what it deliberately leaves, and the two
//! properties that make it a boundary rather than a function somebody calls.
//!
//! ADR-0014 §5. 🚨 **These tests do not claim the redactor is a control.** W7
//! measured text-level checks at 39 of 50 and ruled them out as the control
//! (F404–F406); this asserts what a *backstop* is supposed to do, which is the
//! same relationship `tests/policy.rs` has to the destructive-git table.

use abcc_core::redact::{Kind, MARKER, Secrets};

/// The exact half. A value this process holds is removed wherever it appears —
/// in prose, inside a JSON blob, in the middle of a word — because a key that
/// survives by being adjacent to a quote is a key on the log.
#[test]
fn a_value_this_process_holds_is_removed_everywhere_it_appears() {
    let secrets = Secrets::default().with_literal("lm-studio-abc123xyz");

    let scrub = secrets.scrub(
        "connecting with lm-studio-abc123xyz\n\
         {\"api_key\":\"lm-studio-abc123xyz\"}\n\
         curl -H 'x-key: lm-studio-abc123xyz' http://localhost:1234",
    );

    assert!(
        !scrub.text.as_str().contains("lm-studio-abc123xyz"),
        "the key survived: {}",
        scrub.text
    );
    assert_eq!(scrub.text.as_str().matches(MARKER).count(), 3);
    assert_eq!(
        scrub.removed,
        vec![abcc_core::redact::Removed {
            kind: Kind::Known,
            count: 3
        }]
    );
}

/// 🚨 A literal too short to be a credential is **dropped rather than
/// searched for**. `Secrets::with_literal("x")` scrubbing every `x` in the
/// transcript is a denial of service against the model's own context, and it
/// would fire on the ordinary case of no key being configured at all.
#[test]
fn a_literal_too_short_to_be_a_credential_is_not_searched_for() {
    let secrets = Secrets::default()
        .with_literal("lm-studio")
        .with_literal("");
    assert_eq!(secrets.literals(), 1, "only the 9-character one is kept");

    let short = Secrets::default().with_literal("abc");
    assert_eq!(short.literals(), 0);
    let scrub = short.scrub("abc is a perfectly ordinary abcdef word");
    assert_eq!(
        scrub.text.as_str(),
        "abc is a perfectly ordinary abcdef word"
    );
    assert!(!scrub.touched());
}

/// The shape half, one case per arm, all in one text so the counting is
/// asserted too. Every one of these is a heuristic; the test is that the
/// well-known formats are caught, never that the class is closed.
#[test]
fn the_four_shapes_are_each_caught_and_counted_by_class() {
    let text = "\
-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmU
-----END OPENSSH PRIVATE KEY-----
Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig
export GITHUB_TOKEN=ghp_0123456789abcdefghijklmnopqrstuvwxyz
DATABASE_PASSWORD=hunter2hunter2
";
    let scrub = Secrets::shapes_only().scrub(text);

    for leaked in [
        "b3BlbnNzaC1rZXktdjEAAAAABG5vbmU",
        "eyJhbGciOiJIUzI1NiJ9",
        "ghp_0123456789abcdefghijklmnopqrstuvwxyz",
        "hunter2hunter2",
    ] {
        assert!(
            !scrub.text.as_str().contains(leaked),
            "{leaked} survived:\n{}",
            scrub.text
        );
    }

    let kinds: Vec<Kind> = scrub.removed.iter().map(|r| r.kind).collect();
    assert!(kinds.contains(&Kind::PrivateKey), "{kinds:?}");
    assert!(kinds.contains(&Kind::AuthHeader), "{kinds:?}");
    assert!(kinds.contains(&Kind::VendorToken), "{kinds:?}");
    assert!(kinds.contains(&Kind::Assignment), "{kinds:?}");
}

/// 🚨 **The half that would make the redactor useless, and the reason it is
/// tested at all.** A scrubber that eats ordinary source is a scrubber that
/// gets turned off. Everything here is text this project's own tools produce
/// every day, and none of it may be touched.
#[test]
fn ordinary_source_and_ordinary_diagnostics_are_left_alone() {
    let untouched = [
        "fn main() { println!(\"hello\"); }",
        "error[E0004]: non-exhaustive patterns: `Err(_)` not covered",
        "@@ -123,3 +123,4 @@ impl Display for Task {",
        "let key = map.get(&id).expect(\"present\");",
        // Named like a secret, valued like a flag. The value half of the
        // assignment rule is what keeps this out.
        "SECRET=1",
        "password: \"\"",
        // A word that merely contains a vendor prefix is not a token.
        "the sk-learn docs",
        "git commit -m \"add the AKIA prefix to the table\"",
        "https://github.com/mrdushidush/abcc",
    ];
    let secrets = Secrets::default().with_literal("lm-studio-abc123xyz");
    for line in untouched {
        let scrub = secrets.scrub(line);
        assert_eq!(
            scrub.text.as_str(),
            line,
            "a false positive on ordinary text: {:?}",
            scrub.removed
        );
        assert!(!scrub.touched());
    }
}

/// An authorization header keeps its name and loses its value. The operator
/// reading the log needs to know a header was there; nobody needs the token.
#[test]
fn a_header_keeps_its_name_and_loses_its_value() {
    let scrub = Secrets::shapes_only().scrub("Authorization: Bearer sk-live-0123456789abcdef");
    assert_eq!(
        scrub.text.as_str(),
        format!("Authorization: Bearer {MARKER}")
    );
}

/// The literal half runs first, so a key that *also* matches a shape is counted
/// as the thing it is. Otherwise the operator's record says *a vendor token was
/// removed* about the one value in the process whose identity is known exactly.
#[test]
fn a_known_value_that_also_matches_a_shape_is_counted_as_known() {
    let key = "sk-0123456789abcdefghij";
    let scrub = Secrets::default()
        .with_literal(key)
        .scrub(format!("k={key}"));
    assert_eq!(
        scrub.removed,
        vec![abcc_core::redact::Removed {
            kind: Kind::Known,
            count: 1
        }],
        "ran the shapes first and lost the exact identification"
    );
}

/// The note names the class and the count and never the value. A record that
/// quoted what it removed would put the secret back on the log one field to the
/// left — which is the failure this whole module exists to prevent.
#[test]
fn the_note_names_the_class_and_never_the_value() {
    let secrets = Secrets::default().with_literal("lm-studio-abc123xyz");
    let scrub = secrets.scrub("key=lm-studio-abc123xyz and key=lm-studio-abc123xyz");
    let note = scrub.note().expect("something was removed");

    assert!(!note.contains("lm-studio-abc123xyz"), "{note}");
    assert!(note.contains('2'), "the count is missing: {note}");
    assert!(note.contains("a value this process holds"), "{note}");

    assert_eq!(Secrets::default().scrub("nothing here").note(), None);
}

/// 🚨 **Scrubbing twice changes nothing.** The boundary is crossed once by
/// design, but the marker must not itself be scrubbable — a redactor that ate
/// its own output would corrupt a transcript every time a result passed a seam
/// somebody added later.
#[test]
fn scrubbing_an_already_scrubbed_string_changes_nothing() {
    let secrets = Secrets::default().with_literal("lm-studio-abc123xyz");
    let once = secrets.scrub("Authorization: Bearer lm-studio-abc123xyz");
    let twice = secrets.scrub(once.text.as_str());

    assert_eq!(once.text, twice.text);
    assert!(
        !twice.touched(),
        "the second pass found {:?}",
        twice.removed
    );
}

/// Two private keys in one file are two matches and not everything between
/// them. The `(?s)` that lets a key span lines is what makes a greedy pattern
/// swallow an entire file, so the laziness is load-bearing.
#[test]
fn two_key_blocks_are_two_matches_and_not_everything_between_them() {
    let text = "\
-----BEGIN RSA PRIVATE KEY-----
aaa
-----END RSA PRIVATE KEY-----
this line is ordinary and must survive
-----BEGIN RSA PRIVATE KEY-----
bbb
-----END RSA PRIVATE KEY-----";
    let scrub = Secrets::shapes_only().scrub(text);

    assert!(
        scrub
            .text
            .as_str()
            .contains("this line is ordinary and must survive"),
        "the greedy match ate the file: {}",
        scrub.text
    );
    assert_eq!(
        scrub.removed,
        vec![abcc_core::redact::Removed {
            kind: Kind::PrivateKey,
            count: 2
        }]
    );
}

/// 🚨 **The default is ON.** A `Secrets` nobody configured still carries every
/// shape — a redactor whose default is *nothing* protects only the code paths
/// somebody remembered to wire, which is the donor's defect (F416) one layer up.
#[test]
fn the_default_scrubber_still_carries_the_shapes() {
    let scrub = Secrets::default().scrub("Authorization: Bearer sk-live-0123456789abcdef");
    assert!(scrub.touched(), "an unconfigured Secrets scrubbed nothing");
    assert_eq!(Secrets::default().literals(), 0);
}
