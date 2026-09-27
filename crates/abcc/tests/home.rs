//! Where the log and the worktrees go, and the one rule about it that is not
//! taste.
//!
//! 🚨 The claim under test is that state cannot end up inside the repository it
//! is about. Two independent reasons, either sufficient: a checkpoint stages the
//! whole tree, so a log that changes on every event would land in every snapshot
//! and no two snapshots of unchanged work would be equal; and git refuses to nest
//! a worktree inside the tree it came from.

use std::fs;
use std::path::Path;

use abcc::Home;
use abcc::home::default_root;

// ---------------------------------------------------------------------------
// the rule
// ---------------------------------------------------------------------------

#[test]
fn a_home_inside_the_repository_is_refused_and_says_why() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("subject");
    fs::create_dir_all(&repo).expect("mkdir");

    let inside = repo.join(".abcc");
    let refused = Home::resolve(Some(inside), &repo).expect_err("inside the repo");
    assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
    let said = refused.to_string();
    assert!(said.contains("inside the repository"), "{said}");
    // The sentence has to carry the reason, because the operator's next move is
    // to choose somewhere else and they need to know what they are avoiding.
    assert!(said.contains("every snapshot"), "{said}");
}

#[test]
fn the_repository_itself_is_refused_and_so_is_a_directory_under_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("subject");
    fs::create_dir_all(repo.join("deep/deeper")).expect("mkdir");

    assert!(Home::resolve(Some(repo.clone()), &repo).is_err());
    assert!(Home::resolve(Some(repo.join("deep/deeper")), &repo).is_err());
}

#[test]
fn a_home_beside_the_repository_is_allowed_and_holds_both_things() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("subject");
    fs::create_dir_all(&repo).expect("mkdir");

    let home = Home::resolve(Some(dir.path().join("state")), &repo).expect("beside");
    assert!(home.log().starts_with(home.root()));
    assert!(home.worktrees().starts_with(home.root()));
    assert_eq!(
        home.log().file_name().and_then(|n| n.to_str()),
        Some("log.sqlite")
    );
    // The worktree directory is made up front, because the driver cuts into it
    // and git will not create the parent.
    assert!(home.worktrees().is_dir());
    // ⚠ And it is not inside the repository, which is the thing the driver
    // documents as a requirement rather than checks.
    assert!(!home.worktrees().starts_with(&repo));
}

// ---------------------------------------------------------------------------
// the default
// ---------------------------------------------------------------------------

#[test]
fn the_default_is_keyed_by_the_repositorys_path_and_not_by_its_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let one = dir.path().join("a/abcc");
    let two = dir.path().join("b/abcc");
    fs::create_dir_all(&one).expect("mkdir");
    fs::create_dir_all(&two).expect("mkdir");

    // Two checkouts of the same project share a last path component and must not
    // share a log.
    assert_ne!(default_root(&one), default_root(&two));
    // The name is still in there, because a human reads that directory listing.
    assert!(
        name_of(&default_root(&one)).starts_with("abcc-"),
        "{:?}",
        default_root(&one)
    );
    // And the same repository twice is the same answer, which is what makes the
    // default usable at all.
    assert_eq!(default_root(&one), default_root(&one));
}

/// ⚠ Windows only, because the claim is: a case-sensitive filesystem really
/// does hold two repositories at two casings of one path.
#[cfg(windows)]
#[test]
fn a_repository_path_that_differs_only_in_case_is_one_repository() {
    // Windows paths are case-insensitive, so `D:\dev\abcc` and `d:\dev\abcc` are
    // one checkout and must not get two logs.
    //
    // ⚠ The directory must EXIST. The case is settled by `canonicalize`, which
    // asks the filesystem, so a path that is not there keeps the case it was
    // typed in. This test used to name `D:/dev/abcc` literally: green on the one
    // box that has that checkout, red on every CI runner since.
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("Subject");
    fs::create_dir_all(&repo).expect("mkdir");
    let typed = repo.to_string_lossy();
    assert_eq!(
        default_root(Path::new(&typed.to_uppercase())),
        default_root(Path::new(&typed.to_lowercase()))
    );
}

#[test]
fn the_default_never_lands_in_the_repository_it_is_about() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("subject");
    fs::create_dir_all(&repo).expect("mkdir");
    assert!(!default_root(&repo).starts_with(&repo));
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
