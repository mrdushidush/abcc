//! The diff applier, and the tool that drives it.
//!
//! The interesting cases are all the same shape: the model is right about the
//! code and wrong about where it is.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use abcc_core::outcome::Why;
use abcc_engine::patch::{Change, Patch};
use abcc_engine::provider::ToolCall;
use abcc_engine::tools::lookup;
use abcc_engine::turn::{ToolResult, Tools};
use abcc_engine::workspace::Workspace;

fn apply(workspace: &Workspace, diff: &str) -> ToolResult {
    let spec = lookup("apply_patch").expect("registry");
    workspace.run(
        spec,
        &ToolCall {
            id: "call-1".to_owned(),
            tool: "apply_patch".to_owned(),
            arguments: serde_json::json!({ "diff": diff }).to_string(),
        },
    )
}

fn write(root: &Path, rel: &str, content: &str) {
    let at = root.join(rel);
    if let Some(parent) = at.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(at, content).expect("write");
}

fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).expect("read")
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[test]
fn a_git_style_header_is_read_past() {
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n\
                index 1234567..89abcde 100644\n\
                --- a/src/lib.rs\n\
                +++ b/src/lib.rs\n\
                @@ -1,2 +1,2 @@\n\
                -old\n\
                +new\n\
                 tail\n";
    let patch = Patch::parse(diff).expect("parse");
    assert_eq!(patch.files.len(), 1);
    assert_eq!(patch.files[0].path, "src/lib.rs");
    assert_eq!(patch.files[0].kind, Change::Modify);
    assert_eq!(patch.files[0].hunk_count(), 1);
}

#[test]
fn dev_null_on_either_side_is_a_creation_or_a_deletion() {
    let created =
        Patch::parse("--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1 @@\n+hello\n").expect("parse");
    assert_eq!(created.files[0].kind, Change::Create);
    assert_eq!(created.files[0].path, "new.rs");

    let deleted =
        Patch::parse("--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-hello\n").expect("parse");
    assert_eq!(deleted.files[0].kind, Change::Delete);
    assert_eq!(deleted.files[0].path, "gone.rs");
}

#[test]
fn prose_with_no_diff_in_it_is_refused_by_name() {
    let err = Patch::parse("I would change the second line to say `new`.").expect_err("no diff");
    assert!(err.to_string().contains("no file header"), "{err}");
}

// ---------------------------------------------------------------------------
// Applying
// ---------------------------------------------------------------------------

#[test]
fn a_hunk_applies_where_its_context_is_rather_than_where_it_claims() {
    let dir = tempfile::tempdir().expect("tempdir");
    let body: String = (1..=20).fold(String::new(), |mut acc, n| {
        let _ = writeln!(acc, "line {n}");
        acc
    });
    write(dir.path(), "f.txt", &body);
    let workspace = Workspace::open(dir.path()).expect("open");

    // The hunk claims line 2 and its context is at line 12. A model that read the
    // file through a range window gets exactly this wrong.
    let diff =
        "--- a/f.txt\n+++ b/f.txt\n@@ -2,3 +2,3 @@\n line 11\n-line 12\n+line twelve\n line 13\n";
    let result = apply(&workspace, diff);

    assert_eq!(result.unmeasured, None, "{}", result.text);
    assert!(
        result.text.starts_with("applied 1 hunk to 1 file"),
        "{}",
        result.text
    );
    let after = read(dir.path(), "f.txt");
    assert!(after.contains("line twelve\n"), "{after}");
    assert!(after.contains("line 11\nline twelve\nline 13\n"), "{after}");
    assert_eq!(after.lines().count(), 20);
}

/// 🚨 The one thing that is fuzzed, and the reason. Content on this platform is
/// routinely CRLF and a model writes LF; comparing CR-stripped lines is the
/// difference between working here and never applying a patch at all. What gets
/// written back keeps the file's own ending.
#[test]
fn a_line_feed_diff_applies_to_a_carriage_return_file_and_leaves_it_that_way() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "crlf.txt", "alpha\r\nbeta\r\ngamma\r\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    let diff = "--- a/crlf.txt\n+++ b/crlf.txt\n@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n";
    let result = apply(&workspace, diff);

    assert_eq!(result.unmeasured, None, "{}", result.text);
    assert_eq!(read(dir.path(), "crlf.txt"), "alpha\r\nBETA\r\ngamma\r\n");
}

#[test]
fn two_hunks_in_one_file_both_land() {
    let dir = tempfile::tempdir().expect("tempdir");
    let body: String = (1..=30).fold(String::new(), |mut acc, n| {
        let _ = writeln!(acc, "line {n}");
        acc
    });
    write(dir.path(), "f.txt", &body);
    let workspace = Workspace::open(dir.path()).expect("open");

    let diff = "--- a/f.txt\n+++ b/f.txt\n\
                @@ -4,3 +4,3 @@\n line 4\n-line 5\n+five\n line 6\n\
                @@ -24,3 +24,3 @@\n line 24\n-line 25\n+twenty-five\n line 26\n";
    let result = apply(&workspace, diff);

    assert_eq!(result.unmeasured, None, "{}", result.text);
    let after = read(dir.path(), "f.txt");
    assert!(after.contains("line 4\nfive\nline 6\n"), "{after}");
    assert!(after.contains("line 24\ntwenty-five\nline 26\n"), "{after}");
    assert_eq!(after.lines().count(), 30);
}

#[test]
fn a_creation_and_a_deletion_do_what_they_say() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "gone.rs", "delete me\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    let created = apply(
        &workspace,
        "--- /dev/null\n+++ b/src/new.rs\n@@ -0,0 +1,2 @@\n+fn new() {}\n+\n",
    );
    assert_eq!(created.unmeasured, None, "{}", created.text);
    assert_eq!(read(dir.path(), "src/new.rs"), "fn new() {}\n\n");

    let deleted = apply(
        &workspace,
        "--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-delete me\n",
    );
    assert_eq!(deleted.unmeasured, None, "{}", deleted.text);
    assert!(!dir.path().join("gone.rs").exists());
}

/// ⚠ **All or nothing.** A half-applied diff leaves a tree nobody chose, and the
/// model cannot see that it happened.
#[test]
fn one_hunk_that_does_not_match_leaves_every_file_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "first.txt", "alpha\nbeta\n");
    write(dir.path(), "second.txt", "gamma\ndelta\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    let diff = "--- a/first.txt\n+++ b/first.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+BETA\n\
                --- a/second.txt\n+++ b/second.txt\n@@ -1,2 +1,2 @@\n nothing like this\n-at all\n+neither\n";
    let result = apply(&workspace, diff);

    match &result.unmeasured {
        Some(Why::FailedBeforeRunning { detail }) => {
            assert!(detail.contains("second.txt"), "{detail}");
            assert!(detail.contains("hunk 1"), "{detail}");
            // F638. The message has to carry the EVIDENCE rather than the
            // claimed line. It used to say "nowhere in the file", and the
            // model's observed response was to change the line number - which
            // is the one part of a hunk that `locate` never reads.
            assert!(detail.contains("you wrote:"), "{detail}");
            assert!(detail.contains("the file has:"), "{detail}");
            assert!(
                detail.contains("nothing like this"),
                "the model's own line has to be quoted back: {detail}"
            );
            assert!(
                detail.contains("gamma"),
                "and the file's real text at the closest place: {detail}"
            );
        }
        other => panic!("expected a refusal, got {other:?}: {}", result.text),
    }
    assert_eq!(
        read(dir.path(), "first.txt"),
        "alpha\nbeta\n",
        "the first file was written"
    );
    assert_eq!(read(dir.path(), "second.txt"), "gamma\ndelta\n");
}

/// Every path in a diff goes through the same argument check as every other
/// path. A diff is not a second way in.
#[test]
fn a_diff_cannot_write_outside_the_workspace() {
    let outer = tempfile::tempdir().expect("tempdir");
    fs::create_dir(outer.path().join("inside")).expect("mkdir");
    fs::write(outer.path().join("target.txt"), "untouched\n").expect("write");
    let workspace = Workspace::open(outer.path().join("inside")).expect("open");

    let result = apply(
        &workspace,
        "--- a/../target.txt\n+++ b/../target.txt\n@@ -1 +1 @@\n-untouched\n+owned\n",
    );
    assert!(
        result.text.contains("outside the workspace"),
        "{}",
        result.text
    );
    assert_eq!(
        fs::read_to_string(outer.path().join("target.txt")).expect("read"),
        "untouched\n"
    );
}

// ---------------------------------------------------------------------------
// 🚨 F637 / F638 — what the model actually sends, and what it is told back
// ---------------------------------------------------------------------------

/// 🚨 **F637: a payload carrying tool-call markup is refused as what it is.**
///
/// This is not a hypothetical shape. **32 of the 46 recoverable `apply_patch`
/// refusals on this project's log (70%) look exactly like this** — a real diff,
/// then the model's prose, then another `<tool_call>` block, concatenated into
/// one `diff` argument. It is present on the *first* `apply_patch` of an attempt
/// in 14 of 14 attempts, so it is not a reaction to being refused.
///
/// Before this, `Patch::parse` accepted it: there is a genuine `--- / +++` pair
/// at the top, so the payload parses, applies nothing, and fails several hunks
/// later with a sentence about a line number. The round is spent either way —
/// the difference is whether the model is told something it can act on.
#[test]
fn a_diff_argument_carrying_tool_call_markup_is_refused_as_a_transcript() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "first.txt", "alpha\nbeta\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    // Abridged from a real payload on the log.
    let diff = "--- a/first.txt\n+++ b/first.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+BETA\n\
                \"\n\nWait, that last patch block is broken. Let me redo this properly.\n\n\
                <tool_call>\n<function=apply_patch>\n<parameter=diff>\n\
                --- a/first.txt\n+++ b/first.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+BETA\n";
    let result = apply(&workspace, diff);

    match &result.unmeasured {
        Some(Why::FailedBeforeRunning { detail }) => {
            assert!(
                detail.contains("tool-call markup"),
                "it has to name the real problem: {detail}"
            );
            assert!(detail.contains("<tool_call>"), "{detail}");
            assert!(
                !detail.contains("claims line") && !detail.contains("closest place"),
                "and must NOT talk about line numbers or near misses: {detail}"
            );
        }
        other => panic!("expected a refusal, got {other:?}: {}", result.text),
    }
    // ⚠ All or nothing still holds: the first hunk is perfectly valid and is
    // still not applied, because a transcript is not a request.
    assert_eq!(read(dir.path(), "first.txt"), "alpha\nbeta\n");
}

/// 🚨🚨 **F649: the refusal names `write_file`, and it names it in the
/// string the model actually reads.**
///
/// F648 flew the refusal as shipped and it was correct, well-aimed and useless:
/// five of five `apply_patch` calls refused with this sentence, two attempts
/// ended on the round budget, no artifact. F641 is why — the fold happens in the
/// **server's** parser, so a message asking the model to send one clean call
/// asks it to stop doing something it may not be doing. The sentence now offers
/// the door the log says works: the single `Accomplished` on this project's log
/// got there by falling back to `write_file`, refused 1 of 17 against
/// `apply_patch`'s 51 of 66.
///
/// 🚨 **This asserts `result.text`, and that is the point.** The older test
/// beside it reads `unmeasured.detail`, which is what the *log* keeps;
/// `Message::tool_result` is built from `text`. A message that reached the
/// record and not the transcript would pass that test and change nothing about
/// the run — F647's rule, one layer down: assert the wire, not the fold.
#[test]
fn the_refusal_the_model_reads_offers_write_file_as_the_way_out() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "first.txt", "alpha\nbeta\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    let diff = "--- a/first.txt\n+++ b/first.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+BETA\n\
                <tool_call>\n<function=apply_patch>\n";
    // ⚠ Not a tautology: the payload names neither tool, so every assertion
    // below is about the sentence and not about the fixture (F632).
    assert!(
        !diff.contains("write_file"),
        "the fixture must not supply it"
    );
    assert!(
        !diff.contains("content"),
        "nor the argument being recommended"
    );

    let result = apply(&workspace, diff);

    assert!(
        result.text.contains("write_file"),
        "the model is told to keep patching and nothing else: {}",
        result.text
    );
    assert!(
        result.text.contains("content"),
        "naming the tool without naming its argument spends a round: {}",
        result.text
    );
    // The offer is conditional on a second refusal, which is the shape the one
    // success had. A message that abandoned the tool on sight would be a
    // different arm, and this is the one that flew.
    assert!(
        result.text.contains("the next `apply_patch`"),
        "the offer must be the second refusal's, not the first's: {}",
        result.text
    );
    // 🚨 One string, two destinations. `refused` clones it deliberately so a
    // transcript and a record cannot disagree about why a tool did nothing.
    match &result.unmeasured {
        Some(Why::FailedBeforeRunning { detail }) => {
            assert_eq!(detail, &result.text, "the log and the transcript diverged");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(read(dir.path(), "first.txt"), "alpha\nbeta\n");
}

/// ⚠ **A diff that *adds* a line containing tool-call markup is not a
/// transcript.** The check reads bare lines only — inside a hunk every line
/// carries a `' '`, `'+'` or `'-'` prefix — because this repository's own
/// research notes quote `<tool_call>` and refusing to patch them would be a new
/// way to reject correct work.
#[test]
fn a_diff_that_adds_a_line_containing_markup_still_applies() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "notes.md", "one\ntwo\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    let diff = "--- a/notes.md\n+++ b/notes.md\n@@ -1,2 +1,3 @@\n one\n two\n+<tool_call> is what it emits\n";
    let result = apply(&workspace, diff);

    assert!(
        result.unmeasured.is_none(),
        "a legitimate addition was refused: {:?} {}",
        result.unmeasured,
        result.text
    );
    assert_eq!(
        read(dir.path(), "notes.md"),
        "one\ntwo\n<tool_call> is what it emits\n"
    );
}

/// 🚨 **F638: when a real diff does not match, say what the file actually has.**
///
/// The old message named the claimed line, and the model's observed response was
/// to change it — three times in one attempt, 195 then 196 then 187. But the
/// hunk header is a *hint*: `locate` searches the whole file, so the claimed line
/// is the one part of a hunk that cannot cause the failure.
///
/// ⚠ The diagnosis is not the matching. Application stays exact; this only
/// changes the sentence, and the sentence now carries the evidence.
#[test]
fn a_hunk_that_does_not_match_is_told_where_it_came_closest_and_what_differs() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(
        dir.path(),
        "cli.rs",
        "Everywhere:\n  --repo <path>\n  --home <path>\n",
    );
    let workspace = Workspace::open(dir.path()).expect("open");

    // One character wrong, exactly the real case: lowercase `everywhere:`.
    let diff = "--- a/cli.rs\n+++ b/cli.rs\n@@ -1,3 +1,4 @@\n everywhere:\n   --repo <path>\n   --home <path>\n+  --version\n";
    let result = apply(&workspace, diff);

    match &result.unmeasured {
        Some(Why::FailedBeforeRunning { detail }) => {
            assert!(detail.contains("2 of its 3"), "how much matched: {detail}");
            assert!(detail.contains("line 1"), "where it came closest: {detail}");
            assert!(
                detail.contains(r#"you wrote:    "everywhere:""#),
                "the model's own line: {detail}"
            );
            assert!(
                detail.contains(r#"the file has: "Everywhere:""#),
                "and the file's: {detail}"
            );
        }
        other => panic!("expected a refusal, got {other:?}: {}", result.text),
    }
}

/// 🚨 **And the case the bare-line rule actually exists for: markup in a hunk's
/// CONTEXT.**
///
/// A `+` prefix is not stripped by `trim_start`, so an *added* line was never at
/// risk — which is why the first version of the test above passed with the guard
/// removed and therefore taught nothing. A **context** line carries a leading
/// space, and stripping that leaves `<tool_call>` at the head of the line. This
/// is the real shape: editing a file that already quotes the markup, such as
/// this project's own research notes.
#[test]
fn markup_in_a_hunks_context_is_content_and_not_a_transcript() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "notes.md", "<tool_call>\nis what it emits\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    let diff = "--- a/notes.md\n+++ b/notes.md\n@@ -1,2 +1,3 @@\n <tool_call>\n is what it emits\n+and we refuse it\n";
    let result = apply(&workspace, diff);

    assert!(
        result.unmeasured.is_none(),
        "markup quoted as context is content: {:?} {}",
        result.unmeasured,
        result.text
    );
    assert_eq!(
        read(dir.path(), "notes.md"),
        "<tool_call>\nis what it emits\nand we refuse it\n"
    );
}

/// 🚨 **A diff that names one path twice applied both entries, and the
/// second write undid the first.**
///
/// Not hypothetical: it is how `t14977` was spent on 2026-09-20. The model
/// repeated the `---`/`+++` header before its second hunk — ordinary output
/// that `git apply` takes in its stride — and `apply_patch` read every entry's
/// original with `fs::read` while the write loop was still to come. Entry two
/// therefore re-derived the *unmodified* file and wrote it back over entry one.
/// The tool reported `applied 2 hunks to 2 files` and left the tree byte for
/// byte as it found it, which is a success sentence for no change at all.
///
/// ⚠ The diff is built with `concat!` rather than a continued string
/// literal, because `\` at the end of a line eats the next line's leading
/// whitespace — including the single space that makes a context line a
/// context line.
#[test]
fn one_path_named_twice_applies_both_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::open(dir.path()).expect("open");
    write(dir.path(), "src/lib.rs", "one\ntwo\nthree\n");

    let diff = concat!(
        "--- a/src/lib.rs\n",
        "+++ b/src/lib.rs\n",
        "@@ -1,1 +1,2 @@\n",
        " one\n",
        "+ONE AND A HALF\n",
        "--- a/src/lib.rs\n",
        "+++ b/src/lib.rs\n",
        "@@ -3,1 +4,2 @@\n",
        " three\n",
        "+FOUR\n",
    );

    let result = apply(&workspace, diff);
    assert!(
        result.unmeasured.is_none(),
        "the diff is well formed: {:?}",
        result.unmeasured
    );
    assert_eq!(
        read(dir.path(), "src/lib.rs"),
        "one\nONE AND A HALF\ntwo\nthree\nFOUR\n"
    );
    //   One path is one file however many times the diff spells it, and the
    // summary counts files rather than headers.
    assert!(
        result.text.contains("2 hunks to 1 file"),
        "the summary should name one file: {}",
        result.text
    );
}

/// The same shape, and the half that matters most: whatever the summary says,
/// the tree must not come out identical to how it went in.
#[test]
fn a_success_sentence_is_never_reported_over_an_unchanged_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::open(dir.path()).expect("open");
    let before = "alpha\nbeta\n";
    write(dir.path(), "src/lib.rs", before);

    let diff = concat!(
        "--- a/src/lib.rs\n",
        "+++ b/src/lib.rs\n",
        "@@ -1,1 +1,2 @@\n",
        " alpha\n",
        "+GAMMA\n",
        "--- a/src/lib.rs\n",
        "+++ b/src/lib.rs\n",
        "@@ -2,1 +3,2 @@\n",
        " beta\n",
        "+DELTA\n",
    );

    let result = apply(&workspace, diff);
    assert!(result.unmeasured.is_none(), "{:?}", result.unmeasured);
    assert_ne!(
        read(dir.path(), "src/lib.rs"),
        before,
        "apply_patch reported `{}` and changed nothing",
        result.text
    );
    assert_eq!(
        read(dir.path(), "src/lib.rs"),
        "alpha\nGAMMA\nbeta\nDELTA\n"
    );
}
