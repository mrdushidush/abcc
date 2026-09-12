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

/// 🚨 **F712 — the redactor was rewriting this repository's own source, and a
/// model asked to write Rust was handed Rust that does not parse.** With the
/// name half case-insensitive the English word `secrets` matched, and the value
/// half is greedy over non-space characters, so the erasure ate the type and the
/// closing paren with it. These are the exact lines, from the files the sweep
/// named.
#[test]
fn this_repositorys_own_rust_is_not_rewritten_before_a_model_sees_it() {
    let rust = [
        // abcc-drive/src/lib.rs — the builder, the field, the assignment.
        "    pub fn secrets(mut self, secrets: Secrets) -> Driver<'a> {",
        "        self.secrets = secrets;",
        "            secrets: Secrets::default(),",
        // abcc-core/src/redact.rs — the doc comment was cut exactly where it
        // would have named the one public constructor.
        "/// Text that has been through [`Secrets::scrub`] and nothing else.",
        // abcc-engine/src/turn.rs, abcc-fleet/src/lib.rs.
        "    secrets: &'a Secrets,",
        "        let secrets = self.secrets.clone();",
        // Not a secret in any case: a lowercase word in ordinary prose.
        "the password is checked by the caller, not here",
    ];
    let secrets = Secrets::default().with_literal("lm-studio-abc123xyz");
    for line in rust {
        let scrub = secrets.scrub(line);
        assert_eq!(
            scrub.text.as_str(),
            line,
            "the redactor rewrote the repository's own source: {:?}",
            scrub.removed
        );
    }
}

/// The other half of F712, and the half that makes the fix a fix rather than a
/// deletion: **every shape an env file or a shell export actually has is still
/// caught**, in the case those files actually use.
#[test]
fn the_shapes_an_env_file_actually_has_are_still_caught() {
    let caught = [
        "SECRET_KEY=hunter2hunter2",
        "export DATABASE_PASSWORD=hunter2hunter2",
        "API_KEY: \"sk-not-a-real-key-here\"",
        "MY_ACCESS_TOKEN = abcdefghijklmnop",
        "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIK7MDENGbPxRfiCY",
        "ABCC_MODEL_API_KEY=lm-studio-placeholder",
    ];
    let secrets = Secrets::shapes_only();
    for line in caught {
        let scrub = secrets.scrub(line);
        assert!(scrub.touched(), "a real env-file shape got through: {line}");
        assert!(scrub.text.as_str().contains(MARKER), "{}", scrub.text);
    }
}

/// 🚨 **The priced cost of F712, asserted so it cannot be paid twice by
/// accident.** Dropping `(?i)` on the name half loses a lowercase YAML
/// `password:`. The operator ruled it on 2026-09-12, against the alternative of
/// the model being shown source that does not compile. ▶ This test fails the day
/// somebody puts `(?i)` back — at which point read the note beside the pattern
/// first: the value half has to stop being greedy over `[^\s"'#]` before the
/// name half can afford to be case-blind again.
#[test]
fn a_lowercase_assignment_is_the_known_and_ruled_cost() {
    let missed = "password: hunter2hunter2";
    let scrub = Secrets::shapes_only().scrub(missed);
    assert_eq!(
        scrub.text.as_str(),
        missed,
        "the case-insensitive name half is back — and so is F712"
    );
}

/// 🚨 **The measurement F712 came from, kept as a test.** The shape half runs
/// over whatever a tool read, and this repository is the subject of the
/// SELF-HOST milestone, so *the denylist does not alter this workspace's own
/// `src`* is a property and not a coincidence. Before the fix this named 9
/// files and 51 assignment hits. ⚠ It asserts on [`Kind::Assignment`] only:
/// `redact.rs`'s own doc comments quote an `Authorization: Bearer` header, and
/// the rule is **right** about those.
#[test]
fn the_denylist_alters_no_source_file_in_this_workspace() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root is two levels above this crate")
        .join("crates");

    let mut sources = Vec::new();
    let mut stack = vec![crates];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "target") {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.components().any(|c| c.as_os_str() == "src")
            {
                // `src` only: a test file may hold a secret-shaped fixture on
                // purpose, and two of them do.
                sources.push(path);
            }
        }
    }
    assert!(sources.len() > 20, "found only {} files", sources.len());

    let secrets = Secrets::shapes_only();
    let mut rewritten: Vec<String> = Vec::new();
    for path in &sources {
        let text = std::fs::read_to_string(path).expect("readable");
        let scrub = secrets.scrub(text);
        if scrub.removed.iter().any(|r| r.kind == Kind::Assignment) {
            rewritten.push(path.display().to_string());
        }
    }
    assert!(
        rewritten.is_empty(),
        "the denylist rewrites this repository's own source (F712): {rewritten:#?}"
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
