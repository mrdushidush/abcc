//! The checkpoint/worktree cycle against real git, on real trees.
//!
//! Every test here builds its own repository in a temp directory, because the
//! claims being checked are about what git actually does — F328's whole point is
//! that a repository can report one thing and hold another.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use abcc_vcs::{Change, ChangeKind, Repo, Sha, VcsError, checkpoint_ref};

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
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

/// A repository with one commit and three files.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("subject");
    fs::create_dir_all(root.join("src")).expect("mkdir");

    git(dir.path(), &["init", "-q", "-b", "main", "subject"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(&root, &["config", "user.name", "test"]);

    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 1 }\n").expect("write");
    fs::write(root.join("README.md"), "# subject\n").expect("write");
    fs::write(root.join(".gitignore"), "target/\n").expect("write");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "first"]);

    (dir, root)
}

fn changed_paths(changes: &[Change]) -> Vec<&str> {
    let mut v: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
    v.sort_unstable();
    v
}

// ---------------------------------------------------------------------------

/// 🚨 The snapshot captures files the agent *created*, which is exactly what
/// `git stash create` cannot do (F329) — and it is the reason a checkpoint is a
/// commit-tree rather than a stash.
#[test]
fn a_checkpoint_captures_untracked_files() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");

    fs::write(root.join("src/new.rs"), "pub fn two() -> u32 { 2 }\n").expect("write");

    let sha = repo
        .checkpoint(&checkpoint_ref("m1", 1), "checkpoint 1")
        .expect("checkpoint");
    let head = repo.head().expect("head");
    assert_ne!(sha, head, "the checkpoint is the same commit as HEAD");

    let changes = repo.changed_between(&head, &sha).expect("diff");
    assert_eq!(changed_paths(&changes), vec!["src/new.rs"]);
    assert_eq!(changes[0].kind, ChangeKind::Added);
}

/// 🚨 F844: a file named after a Windows device — a model's `2>nul` under Git
/// Bash makes one — must not fail the snapshot, and it must be reported as left
/// out rather than silently dropped.
#[cfg(windows)]
#[test]
fn a_checkpoint_survives_a_file_named_after_a_windows_device() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");

    // A verbatim `\\?\` path is the only way Windows itself will make these.
    let verbatim = |rel: &str| {
        let full = root.join(rel).display().to_string().replace('/', r"\");
        PathBuf::from(format!(r"\\?\{full}"))
    };
    fs::write(verbatim("nul"), "x\n").expect("write nul");
    fs::write(verbatim(r"src\Aux.rs"), "x\n").expect("write Aux.rs");
    fs::write(root.join("src/new.rs"), "pub fn two() -> u32 { 2 }\n").expect("write");

    let sha = repo
        .checkpoint(&checkpoint_ref("m1", 1), "checkpoint")
        .expect("a device-named file failed the checkpoint");
    let head = repo.head().expect("head");
    let changes = repo.changed_between(&head, &sha).expect("diff");
    assert_eq!(changed_paths(&changes), vec!["src/new.rs"]);

    let mut skipped = repo.unindexable().expect("unindexable");
    skipped.sort_unstable();
    assert_eq!(skipped, vec!["nul", "src/Aux.rs"]);
}

/// The snapshot touches neither the index nor the working tree. If it did, the
/// operator's own staged work would move underneath them every time the fleet
/// saved a game.
#[test]
fn a_checkpoint_does_not_disturb_the_index_or_the_working_tree() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");

    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 111 }\n").expect("write");
    fs::write(root.join("staged.rs"), "// staged by the operator\n").expect("write");
    git(&root, &["add", "staged.rs"]);

    let before_status = Command::new("git")
        .current_dir(&root)
        .args(["status", "--porcelain"])
        .output()
        .expect("status");
    let before = String::from_utf8_lossy(&before_status.stdout).into_owned();

    repo.checkpoint(&checkpoint_ref("m1", 1), "checkpoint")
        .expect("checkpoint");

    let after_status = Command::new("git")
        .current_dir(&root)
        .args(["status", "--porcelain"])
        .output()
        .expect("status");
    let after = String::from_utf8_lossy(&after_status.stdout).into_owned();

    assert_eq!(before, after, "the snapshot moved the index or the tree");
    assert_eq!(
        fs::read_to_string(root.join("src/lib.rs")).expect("read"),
        "pub fn one() -> u32 { 111 }\n"
    );
}

/// 🚨 F330: an unreferenced snapshot does not survive `git gc --prune=now`. This
/// is the line that makes `Holding { checkpoint }` and every fork-from-checkpoint
/// promise real rather than aspirational, so it gets the harshest gc git offers.
#[test]
fn a_checkpoint_survives_an_aggressive_gc_because_it_has_a_ref() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    fs::write(root.join("src/new.rs"), "pub fn two() -> u32 { 2 }\n").expect("write");

    let name = checkpoint_ref("m1", 7);
    let sha = repo.checkpoint(&name, "checkpoint").expect("checkpoint");

    git(&root, &["gc", "--prune=now", "-q"]);

    // The object is still there and the ref still points at it.
    let out = Command::new("git")
        .current_dir(&root)
        .args(["cat-file", "-t", sha.as_str()])
        .output()
        .expect("cat-file");
    assert!(
        out.status.success(),
        "the snapshot was collected: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "commit");

    let resolved = Command::new("git")
        .current_dir(&root)
        .args(["rev-parse", &name])
        .output()
        .expect("rev-parse");
    assert_eq!(
        String::from_utf8_lossy(&resolved.stdout).trim(),
        sha.as_str()
    );
}

/// The change list is the diff between two snapshots and nothing else, and
/// `.gitignore` keeps build output out of it by construction — which is what
/// makes this the gate's free structural rung.
#[test]
fn the_change_list_is_two_snapshots_and_excludes_ignored_output() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");

    let before = repo
        .checkpoint(&checkpoint_ref("m1", 1), "before")
        .expect("checkpoint");

    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    fs::write(root.join("src/added.rs"), "pub fn three() {}\n").expect("write");
    fs::remove_file(root.join("README.md")).expect("remove");
    // 3 MB of build output that must not appear.
    fs::create_dir_all(root.join("target/debug")).expect("mkdir");
    fs::write(root.join("target/debug/subject.exe"), vec![0u8; 3_000_000]).expect("write");

    let after = repo
        .checkpoint(&checkpoint_ref("m1", 2), "after")
        .expect("checkpoint");

    let changes = repo.changed_between(&before, &after).expect("diff");
    assert_eq!(
        changed_paths(&changes),
        vec!["README.md", "src/added.rs", "src/lib.rs"]
    );
    for c in &changes {
        assert!(!c.path.starts_with("target/"), "build output leaked: {c:?}");
    }

    let by = |p: &str| changes.iter().find(|c| c.path == p).expect(p).kind.clone();
    assert_eq!(by("src/added.rs"), ChangeKind::Added);
    assert_eq!(by("src/lib.rs"), ChangeKind::Modified);
    assert_eq!(by("README.md"), ChangeKind::Deleted);
}

/// 🚨 **The Judge's pre-image and post-image, in one artifact.** ADR-0008's
/// pairwise result is 14 of 14 against 0 of 8 pointwise, and what makes a diff
/// pairwise is that both versions of every changed line are in it — so this
/// asserts both directions rather than only that a diff came back.
///
/// ⚠ It also asserts the same ignore behaviour the change list has, because a
/// review is where 3 MB of build output would actually cost something: it would
/// arrive as prompt tokens.
#[test]
fn the_patch_carries_both_sides_of_every_change_and_no_ignored_output() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");

    let before = repo
        .checkpoint(&checkpoint_ref("m1", 1), "before")
        .expect("checkpoint");

    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 2 }\n").expect("write");
    fs::create_dir_all(root.join("target/debug")).expect("mkdir");
    fs::write(root.join("target/debug/subject.exe"), vec![0u8; 3_000_000]).expect("write");

    let after = repo
        .checkpoint(&checkpoint_ref("m1", 2), "after")
        .expect("checkpoint");

    let patch = repo.patch_between(&before, &after).expect("patch");
    assert!(
        patch.contains("-pub fn one() -> u32 { 1 }"),
        "the pre-image is missing:\n{patch}"
    );
    assert!(
        patch.contains("+pub fn one() -> u32 { 2 }"),
        "the post-image is missing:\n{patch}"
    );
    assert!(patch.contains("src/lib.rs"), "{patch}");
    assert!(!patch.contains("target/"), "build output leaked:\n{patch}");
}

/// Two snapshots of the same tree produce nothing, and *nothing* is empty rather
/// than a header with no hunks under it. `abcc-drive` reads this to decide the
/// Judge is not worth asking, so the emptiness has to be checkable.
#[test]
fn an_unchanged_tree_produces_an_empty_patch() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let before = repo
        .checkpoint(&checkpoint_ref("m1", 1), "before")
        .expect("checkpoint");
    let after = repo
        .checkpoint(&checkpoint_ref("m1", 2), "after")
        .expect("checkpoint");
    // 🚨 The two shas *differ* — a checkpoint is a commit, and two commits of one
    // tree have different messages and different parents. So the emptiness a
    // caller can act on is the patch's, never the shas'.
    assert_ne!(before, after, "two checkpoints collapsed into one commit");
    assert!(
        repo.patch_between(&before, &after)
            .expect("patch")
            .is_empty(),
        "an unchanged tree produced a patch"
    );
}

/// A rename arrives as one change naming where it came from, not as an add and a
/// delete. A reviewer reading "one file renamed" and a reviewer reading "one file
/// added, one deleted" are being told different things.
#[test]
fn a_rename_is_one_change_that_names_its_source() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    // Big enough that git's similarity detection is unambiguous.
    let body = (0..200).fold(String::new(), |mut s, i| {
        use std::fmt::Write as _;
        writeln!(s, "// line {i}").expect("write to a String");
        s
    });
    fs::write(root.join("src/lib.rs"), &body).expect("write");

    let before = repo
        .checkpoint(&checkpoint_ref("m1", 1), "before")
        .expect("checkpoint");

    fs::rename(root.join("src/lib.rs"), root.join("src/renamed.rs")).expect("rename");

    let after = repo
        .checkpoint(&checkpoint_ref("m1", 2), "after")
        .expect("checkpoint");

    let changes = repo.changed_between(&before, &after).expect("diff");
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].path, "src/renamed.rs");
    assert_eq!(
        changes[0].kind,
        ChangeKind::Renamed {
            from: "src/lib.rs".to_owned()
        }
    );
}

/// Restore is the operator's undo, and it puts back what the snapshot held —
/// exactly, up to the repository's own text attributes.
#[test]
fn restore_puts_the_tree_back_to_a_snapshot() {
    let (_dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");

    let original = fs::read_to_string(root.join("src/lib.rs")).expect("read");
    let saved = repo
        .checkpoint(&checkpoint_ref("m1", 1), "save game")
        .expect("checkpoint");

    fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 999 }\n").expect("write");
    fs::write(root.join("src/wrong.rs"), "// a bad idea\n").expect("write");

    repo.restore(&saved).expect("restore");

    assert_eq!(
        fs::read_to_string(root.join("src/lib.rs")).expect("read"),
        original
    );
    // `read-tree -u --reset` restores tracked content; the file the agent added
    // after the snapshot is untracked in that tree and is deliberately left
    // alone rather than deleted, because deleting files git never knew about is
    // not something an undo button should do quietly.
    assert!(root.join("src/wrong.rs").exists());
}

/// Two attempts get two worktrees at two snapshots, and neither can see the
/// other's work or the operator's. This is the thing no donor does: v1's
/// parallel endpoint has no caller and its three locking surfaces all exclude
/// the agent that writes code (F326).
#[test]
fn two_worktrees_are_isolated_from_each_other_and_from_the_checkout() {
    let (dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let head = repo.head().expect("head");

    let a = repo
        .open_worktree(&dir.path().join("wt-a"), &head)
        .expect("worktree a");
    let b = repo
        .open_worktree(&dir.path().join("wt-b"), &head)
        .expect("worktree b");

    fs::write(a.path().join("src/lib.rs"), "// attempt A\n").expect("write");
    fs::write(b.path().join("src/lib.rs"), "// attempt B\n").expect("write");

    assert_eq!(
        fs::read_to_string(a.path().join("src/lib.rs")).expect("read"),
        "// attempt A\n"
    );
    assert_eq!(
        fs::read_to_string(b.path().join("src/lib.rs")).expect("read"),
        "// attempt B\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("src/lib.rs")).expect("read"),
        "pub fn one() -> u32 { 1 }\n",
        "an attempt reached the operator's checkout"
    );

    a.close().expect("close a");
    b.close().expect("close b");
}

/// 🚨 `close` calls **both** `worktree remove` and `worktree prune`. BCF calls
/// neither, so its `.git/worktrees/` accumulates one stale entry per run.
#[test]
fn closing_a_worktree_removes_it_and_prunes_the_metadata() {
    let (dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let head = repo.head().expect("head");
    let path = dir.path().join("wt");

    let wt = repo.open_worktree(&path, &head).expect("worktree");
    assert!(path.exists());
    let meta = repo.common_dir().expect("common dir").join("worktrees");
    assert!(meta.exists(), "git did not record the worktree");

    wt.close().expect("close");

    assert!(!path.exists(), "the worktree directory survived");
    let listed = Command::new("git")
        .current_dir(&root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .expect("list");
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert_eq!(
        listed.matches("worktree ").count(),
        1,
        "stale worktree metadata remains:\n{listed}"
    );
}

/// 🚨 A worktree can be closed by a process that did not open it, because the
/// durable record of one is the event log and not a value held in memory.
///
/// `abcc take` cuts the operator a tree in one process and `abcc release` takes
/// it down in another, an hour later; without this the only thing that could
/// close a worktree was the `Worktree` handed back by `open_worktree`, which
/// dies with the process that made it. The two paths have to end in the same
/// place — same removal, same prune — so this asserts the metadata as well as
/// the directory.
#[test]
fn a_worktree_can_be_adopted_by_its_path_and_closed_by_whoever_finds_it() {
    let (dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let head = repo.head().expect("head");
    let path = dir.path().join("wt");

    // The process that opened it drops its handle without closing.
    drop(repo.open_worktree(&path, &head).expect("worktree"));
    assert!(path.exists());

    // A second `Repo`, as a second process would have.
    let found = Repo::open(&root).expect("open again");
    found.adopt_worktree(&path, &head).close().expect("close");

    assert!(!path.exists(), "the adopted worktree survived");
    let listed = Command::new("git")
        .current_dir(&root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .expect("list");
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert_eq!(
        listed.matches("worktree ").count(),
        1,
        "stale worktree metadata remains:\n{listed}"
    );
}

/// A worktree is where the isolation is enforced, so it must be at the sha it
/// was asked for rather than at whatever the operator's checkout happens to be.
#[test]
fn a_worktree_opens_at_the_snapshot_it_was_given() {
    let (dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");

    fs::write(root.join("src/only-in-the-snapshot.rs"), "// here\n").expect("write");
    let snapshot = repo
        .checkpoint(&checkpoint_ref("m1", 1), "snap")
        .expect("checkpoint");
    fs::remove_file(root.join("src/only-in-the-snapshot.rs")).expect("remove");

    let wt = repo
        .open_worktree(&dir.path().join("wt"), &snapshot)
        .expect("worktree");
    assert!(
        wt.path().join("src/only-in-the-snapshot.rs").exists(),
        "the worktree is not at the snapshot"
    );
    assert_eq!(wt.sha(), &snapshot);
    wt.close().expect("close");
}

/// 🚨 In a worktree `.git` is a *file*, not a directory, and this is the normal
/// case under ADR-0007. Opening one must work and must resolve to the same
/// shared git directory.
#[test]
fn a_worktree_is_itself_a_repo_and_shares_the_common_dir() {
    let (dir, root) = fixture();
    let repo = Repo::open(&root).expect("open");
    let head = repo.head().expect("head");
    let wt = repo
        .open_worktree(&dir.path().join("wt"), &head)
        .expect("worktree");

    assert!(
        wt.path().join(".git").is_file(),
        "expected .git to be a file in a worktree"
    );

    let inner = Repo::open(wt.path()).expect("open the worktree as a repo");
    assert_eq!(
        fs::canonicalize(inner.common_dir().expect("inner common")).expect("canon"),
        fs::canonicalize(repo.common_dir().expect("outer common")).expect("canon"),
        "the worktree resolved to a different git directory"
    );

    wt.close().expect("close");
}

#[test]
fn a_path_outside_a_repository_is_named_as_such() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = Repo::open(dir.path()).expect_err("a non-repository opened");
    assert!(matches!(err, VcsError::NotARepo { .. }), "{err}");
}

#[test]
fn a_sha_is_forty_hex_characters_and_nothing_else() {
    assert!(Sha::parse("0123456789abcdef0123456789abcdef01234567").is_ok());
    assert!(Sha::parse("  0123456789abcdef0123456789abcdef01234567\n").is_ok());
    assert!(Sha::parse("HEAD").is_err());
    assert!(Sha::parse("0123456").is_err());
    assert!(Sha::parse("refs/abcc/checkpoints/m1/1").is_err());
    assert_eq!(
        Sha::parse("0123456789abcdef0123456789abcdef01234567")
            .unwrap()
            .short(),
        "0123456"
    );
}

#[test]
fn checkpoint_refs_live_in_one_namespace() {
    assert_eq!(
        checkpoint_ref("m17", 4242),
        "refs/abcc/checkpoints/m17/4242"
    );
}
