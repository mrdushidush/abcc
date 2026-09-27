//! The commit this binary was built from, as semver build metadata:
//! `0.1.0+52f7bd0`. `abcc --version` prints it and every `RunStarted` on the log
//! carries it, so a log says which build wrote it. Before this, every build
//! since the first said `0.1.0`, and the bench pins binaries by commit.
//!
//! ⚠ No dirty marker. Cargo reruns this script when `HEAD` moves, not when the
//! working tree changes, so a marker would be stale as often as it was right.
//! Without git (a source tarball), it is the plain package version.

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn main() {
    let package = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let version = match git(&["rev-parse", "--short=7", "HEAD"]) {
        Some(sha) => format!("{package}+{sha}"),
        None => package,
    };
    println!("cargo:rustc-env=ABCC_VERSION={version}");

    // Rerun when HEAD moves: a checkout changes HEAD, a commit changes the ref
    // HEAD names, and a packed ref lives in packed-refs. `--git-path` because
    // in a worktree the refs are not under its own git dir; and only files that
    // exist, because a missing one reads as changed on every build.
    let mut watched = vec!["HEAD".to_owned(), "packed-refs".to_owned()];
    watched.extend(git(&["symbolic-ref", "-q", "HEAD"]));
    for name in watched {
        if let Some(path) = git(&["rev-parse", "--git-path", &name])
            && Path::new(&path).exists()
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
