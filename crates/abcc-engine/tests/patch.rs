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
            assert!(detail.contains("nowhere in the file"), "{detail}");
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
