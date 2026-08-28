//! The nine tools over a real directory.
//!
//! Nothing here is simulated. The claims are about a filesystem — what an ignore
//! rule hides, what a path check refuses, what a walk actually visited — and a
//! fake filesystem would be a fake answer to every one of them.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use abcc_core::event::Event;
use abcc_core::outcome::Why;
use abcc_core::seq::{AttemptId, Seq};
use abcc_engine::provider::ToolCall;
use abcc_engine::scripted::{Script, Scripted};
use abcc_engine::tools::{TOOLS, lookup};
use abcc_engine::turn::{ToolResult, Tools};
use abcc_engine::workspace::Workspace;
use abcc_engine::{Body, ControlPoint, Head, PhaseEnded, TurnLoop};

fn call(tool: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: "call-1".to_owned(),
        tool: tool.to_owned(),
        arguments: arguments.to_owned(),
    }
}

/// Run one tool against a workspace and hand back what the model would see.
fn run(workspace: &Workspace, tool: &str, arguments: &str) -> ToolResult {
    let spec = lookup(tool).expect("the tool is in the registry");
    workspace.run(spec, &call(tool, arguments))
}

fn write(root: &Path, rel: &str, content: &str) {
    let at = root.join(rel);
    if let Some(parent) = at.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(at, content).expect("write");
}

/// The refusal sentence, with the assertion that it *is* a refusal.
fn refusal(result: &ToolResult) -> String {
    match &result.unmeasured {
        Some(Why::FailedBeforeRunning { detail }) => {
            // 🚨 One string, two readers. What the model is told and what the log
            // records are the same sentence, so a transcript and a record cannot
            // disagree about why a tool did nothing.
            assert_eq!(
                &result.text, detail,
                "the model and the log were told different things"
            );
            detail.clone()
        }
        other => panic!(
            "expected a refusal, got {other:?} with text {:?}",
            result.text
        ),
    }
}

// ---------------------------------------------------------------------------
// The registry and the implementation
// ---------------------------------------------------------------------------

/// 🚨 A tool the head advertises and the workspace cannot run is a round of the
/// budget spent on an apology. The registry is one const and this is the test
/// that keeps the implementation level with it.
#[test]
fn every_tool_in_the_registry_has_an_implementation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::open(dir.path()).expect("open");
    for spec in TOOLS {
        // Empty arguments, so nothing runs: every tool refuses at its own
        // argument check, and none of them may refuse for want of an
        // implementation.
        let result = run(&workspace, spec.name, "{}");
        assert!(
            !result.text.contains("cannot run it"),
            "{} is advertised and unimplemented",
            spec.name
        );
    }
}

/// A file tool has no process, so it has no exit status. Inventing `Some(0)`
/// would be the record claiming a measurement nothing took.
#[test]
fn a_file_tool_has_no_exit_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "a.txt", "one\ntwo\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    let result = run(&workspace, "read_file", r#"{"path":"a.txt"}"#);
    assert_eq!(result.exit, None);
    assert_eq!(result.unmeasured, None);
    assert!(result.text.contains("one"), "{}", result.text);
}

// ---------------------------------------------------------------------------
// 🚨 The argument check
// ---------------------------------------------------------------------------

/// 🚨 **F400–F403, as the test that keeps the fix.** The measured failure is not
/// that the check refuses — it refuses correctly. It is that the subject then
/// routes around it with `bash`, and the artifact lands where the verifier does
/// not grade it in half of 80 attempts. So the refusal has to carry the answer.
#[test]
fn a_path_outside_the_workspace_is_refused_with_the_path_that_was_meant() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "tasks/answer.py", "print(1)\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    let result = run(
        &workspace,
        "write_file",
        r#"{"path":"/app/workspace/tasks/answer.py","content":"print(2)"}"#,
    );
    let sentence = refusal(&result);

    assert!(
        sentence.contains("outside the workspace"),
        "the refusal does not say what happened: {sentence}"
    );
    assert!(
        sentence.contains(&workspace.root().display().to_string()),
        "the refusal does not name the workspace: {sentence}"
    );
    assert!(
        sentence.contains("Did you mean tasks/answer.py?"),
        "the refusal does not carry the answer: {sentence}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("tasks/answer.py")).expect("read"),
        "print(1)\n",
        "a refused write wrote"
    );
}

/// `..` is removed textually before anything touches the disk, and what is left
/// is refused by the same comparison as everything else.
#[test]
fn dot_dot_cannot_leave_the_workspace() {
    let outer = tempfile::tempdir().expect("tempdir");
    fs::create_dir(outer.path().join("inside")).expect("mkdir");
    fs::write(outer.path().join("secret.txt"), "not yours").expect("write");
    let workspace = Workspace::open(outer.path().join("inside")).expect("open");

    let result = run(&workspace, "read_file", r#"{"path":"../secret.txt"}"#);
    let sentence = refusal(&result);
    assert!(sentence.contains("outside the workspace"), "{sentence}");
}

/// 🚨 The root is canonical and so is every path compared against it — which is
/// what keeps `D:\x` and its verbatim form from being two directories to the
/// check. A model that answers with an absolute path inside the workspace is
/// right, and being refused for it would teach it to use the shell instead.
#[test]
fn an_absolute_path_inside_the_workspace_is_accepted() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "src/main.rs", "fn main() {}\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    // Deliberately the *uncanonicalised* path the operator passed in, not
    // `workspace.root()`: on this platform they differ by a verbatim prefix.
    let asked = dir.path().join("src/main.rs").display().to_string();
    let arguments = serde_json::json!({ "path": asked }).to_string();
    let result = run(&workspace, "read_file", &arguments);

    assert_eq!(result.unmeasured, None, "{}", result.text);
    assert!(result.text.contains("fn main"), "{}", result.text);
}

// ---------------------------------------------------------------------------
// read_file
// ---------------------------------------------------------------------------

#[test]
fn read_file_names_the_range_it_gave() {
    let dir = tempfile::tempdir().expect("tempdir");
    let body: String = (1..=10).fold(String::new(), |mut acc, n| {
        let _ = writeln!(acc, "line {n}");
        acc
    });
    write(dir.path(), "ten.txt", &body);
    let workspace = Workspace::open(dir.path()).expect("open");

    let result = run(
        &workspace,
        "read_file",
        r#"{"path":"ten.txt","from_line":3,"to_line":5}"#,
    );
    let mut lines = result.text.lines();
    assert_eq!(lines.next(), Some("ten.txt lines 3-5 of 10"));
    assert_eq!(lines.next(), Some("line 3"));
    assert_eq!(lines.next(), Some("line 4"));
    assert_eq!(lines.next(), Some("line 5"));
    assert_eq!(lines.next(), None);
}

/// A binary file decoded lossily is a screenful of replacement characters that
/// cost the window and answer nothing. Say what it is instead.
#[test]
fn read_file_refuses_a_binary_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.bin"), [0x7f, 0x45, 0x00, 0x01, 0x02]).expect("write");
    let workspace = Workspace::open(dir.path()).expect("open");

    let sentence = refusal(&run(&workspace, "read_file", r#"{"path":"a.bin"}"#));
    assert!(sentence.contains("is binary"), "{sentence}");
    assert!(sentence.contains("offset 2"), "{sentence}");
}

/// A range past the end of the file is a mistake worth naming, because the model
/// asked for it on purpose and the next thing it needs to know is the length.
#[test]
fn read_file_says_how_long_the_file_is_when_the_range_is_past_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "short.txt", "one\ntwo\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    let sentence = refusal(&run(
        &workspace,
        "read_file",
        r#"{"path":"short.txt","from_line":40}"#,
    ));
    assert!(sentence.contains("has 2 lines"), "{sentence}");
}

// ---------------------------------------------------------------------------
// list_files and search
// ---------------------------------------------------------------------------

/// The tool's own description says it honours the repository's ignore rules, so
/// this is a test of the sentence in the frozen head as much as of the code.
#[test]
fn list_files_honours_the_repositorys_ignore_rules() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), ".gitignore", "ignored/\n*.log\n");
    write(dir.path(), "ignored/hidden.txt", "x");
    write(dir.path(), "noisy.log", "x");
    write(dir.path(), "kept/seen.txt", "x");
    let workspace = Workspace::open(dir.path()).expect("open");

    let listed = run(&workspace, "list_files", r#"{"path":"."}"#).text;

    // The positive control: the ignored file is on disk, so its absence from the
    // listing is the ignore rule and not a file that was never written.
    assert!(dir.path().join("ignored/hidden.txt").exists());
    assert!(listed.contains("kept/seen.txt"), "{listed}");
    assert!(!listed.contains("hidden.txt"), "{listed}");
    assert!(!listed.contains("noisy.log"), "{listed}");
}

/// `.git` is most of the entries in a repository and never what a model is
/// looking for.
#[test]
fn list_files_does_not_walk_into_dot_git() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), ".git/config", "[core]\n");
    write(dir.path(), "real.txt", "x");
    let workspace = Workspace::open(dir.path()).expect("open");

    let listed = run(&workspace, "list_files", r#"{"path":".","depth":4}"#).text;
    assert!(listed.contains("real.txt"), "{listed}");
    assert!(!listed.contains(".git"), "{listed}");
}

/// 🚨 **A zero from an instrument nobody validated is not a measurement.** "No
/// matches" reads the same whether the pattern is absent or the walk visited
/// nothing, so the count of files searched is part of the answer.
#[test]
fn search_says_how_many_files_it_looked_at() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "a.rs", "fn alpha() {}\n");
    write(dir.path(), "b.rs", "fn beta() {}\n");
    write(dir.path(), "c.txt", "gamma\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    let found = run(&workspace, "search", r#"{"pattern":"fn \\w+"}"#).text;
    assert!(found.starts_with("2 matches in 2 of 3 files"), "{found}");

    let absent = run(&workspace, "search", r#"{"pattern":"nowhere-at-all"}"#).text;
    assert!(absent.starts_with("0 matches in 0 of 3 files"), "{absent}");
}

#[test]
fn search_honours_a_glob() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "a.rs", "needle\n");
    write(dir.path(), "b.txt", "needle\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    let found = run(
        &workspace,
        "search",
        r#"{"pattern":"needle","glob":"*.rs"}"#,
    )
    .text;
    assert!(found.contains("a.rs:1"), "{found}");
    assert!(!found.contains("b.txt"), "{found}");
}

/// A pattern that will not compile is the model's mistake and it can fix it —
/// but only if it is shown what the engine objected to.
#[test]
fn a_pattern_that_will_not_compile_says_why() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "a.rs", "x\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    let sentence = refusal(&run(&workspace, "search", r#"{"pattern":"fn ("}"#));
    assert!(sentence.contains("is not a pattern"), "{sentence}");
    assert!(sentence.contains("unclosed"), "{sentence}");
}

// ---------------------------------------------------------------------------
// write_file
// ---------------------------------------------------------------------------

#[test]
fn write_file_creates_the_directories_it_needs_and_says_which_it_did() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::open(dir.path()).expect("open");

    let made = run(
        &workspace,
        "write_file",
        r#"{"path":"src/deep/new.rs","content":"fn main() {}\n"}"#,
    );
    assert_eq!(made.unmeasured, None, "{}", made.text);
    assert!(
        made.text.starts_with("created src/deep/new.rs"),
        "{}",
        made.text
    );

    let again = run(
        &workspace,
        "write_file",
        r#"{"path":"src/deep/new.rs","content":"fn main() {} // again\n"}"#,
    );
    assert!(
        again.text.starts_with("replaced src/deep/new.rs"),
        "{}",
        again.text
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("src/deep/new.rs")).expect("read"),
        "fn main() {} // again\n"
    );
}

// ---------------------------------------------------------------------------
// The arguments
// ---------------------------------------------------------------------------

/// ⚠ The advertised schema says `additionalProperties: false`; the parser is
/// lenient anyway, because refusing a field nobody read costs a round of the
/// budget and buys nothing. A *missing* required field is a different thing:
/// there is no work to do without it.
#[test]
fn the_arguments_are_read_leniently_and_a_missing_one_is_still_a_refusal() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "a.txt", "content\n");
    let workspace = Workspace::open(dir.path()).expect("open");

    let extra = run(
        &workspace,
        "read_file",
        r#"{"path":"a.txt","encoding":"utf-8"}"#,
    );
    assert_eq!(extra.unmeasured, None, "{}", extra.text);

    let sentence = refusal(&run(&workspace, "read_file", r#"{"from_line":1}"#));
    assert!(
        sentence.contains("not what its schema describes"),
        "{sentence}"
    );
    assert!(sentence.contains("path"), "{sentence}");
}

// ---------------------------------------------------------------------------
// The whole path
// ---------------------------------------------------------------------------

/// The tool layer plugged into the loop it exists for: a scripted turn asks for
/// a tool, the policy admits it, this workspace runs it, and the file is on disk
/// afterwards. Everything in between is the real code.
#[test]
fn the_loop_drives_this_workspace_and_the_write_lands_on_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::open(dir.path()).expect("open");
    let provider = Scripted::new(vec![
        Script::calls(
            "t1",
            "write_file",
            r#"{"path":"src/new.rs","content":"fn main() {}\n"}"#,
        ),
        Script::says("written"),
    ]);
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("create src/new.rs");
    let mut log: Vec<Event> = Vec::new();

    let ended = TurnLoop::new(&provider, &workspace, "qwen3.6-35b-a3b-mtp@iq3_s").run(
        Head::Builders,
        AttemptId::at(Seq::new(7)),
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    match &ended {
        PhaseEnded::Answered { text, report } => {
            assert_eq!(text, "written");
            assert_eq!(report.tool_calls, 1);
            assert_eq!(report.denials, 0);
        }
        other => panic!("expected an answer, got {other:?}"),
    }
    assert_eq!(
        fs::read_to_string(dir.path().join("src/new.rs")).expect("read"),
        "fn main() {}\n"
    );
    let ended_call = log
        .iter()
        .find_map(|e| match e {
            Event::ToolCallEnded {
                tool, unmeasured, ..
            } => Some((tool.clone(), unmeasured.clone())),
            _ => None,
        })
        .expect("the tool call is on the log");
    assert_eq!(ended_call, ("write_file".to_owned(), None));
}

/// 🚨 The ceiling refuses before this workspace is asked, so the file tool that
/// would have written never runs and nothing on disk moves. **The control is
/// denying the class**, and the workspace is not a second place the decision is
/// made.
#[test]
fn a_role_below_the_ceiling_never_reaches_the_workspace_at_all() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::open(dir.path()).expect("open");
    let provider = Scripted::new(vec![
        Script::calls(
            "t1",
            "write_file",
            r#"{"path":"src/new.rs","content":"fn main() {}\n"}"#,
        ),
        Script::says("refused, then"),
    ]);
    let (mut control, _handle) = ControlPoint::new();
    let mut body = Body::opening("create src/new.rs");
    let mut log: Vec<Event> = Vec::new();

    let ended = TurnLoop::new(&provider, &workspace, "qwen3.6-35b-a3b-mtp@iq3_s").run(
        // Recon reads. It does not write, and it may not be talked into it.
        Head::Recon,
        AttemptId::at(Seq::new(7)),
        None,
        &mut body,
        &mut control,
        &mut |e: Event| log.push(e),
    );

    assert_eq!(ended.report().denials, 1);
    assert_eq!(ended.report().tool_calls, 0);
    assert!(
        !dir.path().join("src").exists(),
        "a denied write created a directory"
    );
    let denial = log
        .iter()
        .find_map(|e| match e {
            Event::ToolCallEnded { unmeasured, .. } => unmeasured.clone(),
            _ => None,
        })
        .expect("the denial is on the log");
    assert!(matches!(denial, Why::Denied { .. }), "{denial:?}");
}
