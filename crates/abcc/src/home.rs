//! Where the log and the worktrees live, and the one rule about it that is not
//! taste.
//!
//! 🚨 **Neither may sit inside the repository being worked on**, and this module
//! exists to make that structural rather than remembered. Two independent
//! reasons, either one sufficient:
//!
//! * [`abcc_vcs::Repo::checkpoint`] stages the whole tree into a scratch index
//!   (ADR-0007). A log file inside that tree would therefore be *in* every
//!   snapshot, and since it changes on every event, no two checkpoints of an
//!   unchanged working tree would be equal — which is the one property the whole
//!   checkpoint design rests on.
//! * `Driver::new` already documents that the worktree directory must be outside
//!   the repository, because git refuses to nest one.
//!
//! So the default is a per-repository directory under the platform's data
//! directory, keyed by the repository's canonical path, and [`Home::resolve`]
//! refuses an explicit one that is inside the repository rather than letting the
//! operator find out at the second checkpoint.

use std::path::{Path, PathBuf};
use std::{env, fs, io};

/// Overrides the computed location. Absolute, and still checked against the
/// repository.
pub const HOME_ENV: &str = "ABCC_HOME";

/// The log, the worktrees, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    /// Work out where this repository's state lives, and make sure it exists.
    ///
    /// `explicit` is the `--home` flag; [`HOME_ENV`] is consulted next; the
    /// fallback is `<data dir>/abcc/<repo name>-<hash of its path>`.
    ///
    /// # Errors
    ///
    /// [`io::Error`] if the directories cannot be created, or
    /// [`io::ErrorKind::InvalidInput`] if the chosen location is inside `repo`.
    pub fn resolve(explicit: Option<PathBuf>, repo: &Path) -> io::Result<Home> {
        let root = match explicit.or_else(|| env::var_os(HOME_ENV).map(PathBuf::from)) {
            Some(given) => given,
            None => default_root(repo),
        };
        fs::create_dir_all(&root)?;
        // Canonicalised on both sides, because `D:\dev\abcc` and `d:\dev\abcc\.`
        // are the same directory and a string comparison would say otherwise.
        let root = plain(fs::canonicalize(&root)?);
        let repo = plain(fs::canonicalize(repo)?);
        if root.starts_with(&repo) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is inside the repository at {}. The log changes on every event and a \
                     checkpoint stages the whole tree, so a log in the tree would land in every \
                     snapshot and no two snapshots of unchanged work would be equal. Point \
                     {HOME_ENV} somewhere outside it.",
                    root.display(),
                    repo.display()
                ),
            ));
        }
        fs::create_dir_all(root.join("worktrees"))?;
        Ok(Home { root })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The event log. One file, and it is the whole source of truth.
    #[must_use]
    pub fn log(&self) -> PathBuf {
        self.root.join("log.sqlite")
    }

    /// Where attempt worktrees are cut.
    #[must_use]
    pub fn worktrees(&self) -> PathBuf {
        self.root.join("worktrees")
    }
}

/// Windows' `canonicalize` hands back an extended-length path, `\\?\C:\...`.
///
/// It is correct and it is unreadable, and this one does not only reach a
/// person's screen — it is handed to `git worktree add` and **written into the
/// log** as `WorktreeOpened { path }`, where it is a record somebody has to be
/// able to match against what their own shell prints.
///
/// ⚠ The prefix is dropped only for the simple drive form. A `\\?\UNC\...` path
/// keeps it, because there the prefix is carrying the share and removing it would
/// change where the path points.
fn plain(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    let Some(rest) = text.strip_prefix(r"\\?\") else {
        return path;
    };
    let bytes = rest.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return PathBuf::from(rest);
    }
    path
}

/// `<data dir>/abcc/<repo name>-<hash>`.
///
/// The name is for a human reading a directory listing; the hash is what makes it
/// unique, because two checkouts of the same project have the same last path
/// component and must not share a log.
///
/// Public because it creates nothing: it is the *decision* about where state
/// goes, and a test that had to call [`Home::resolve`] to check it would have to
/// write into the real user profile to do so.
#[must_use]
pub fn default_root(repo: &Path) -> PathBuf {
    let canonical = fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let name = canonical
        .file_name()
        .map_or_else(|| "repo".to_owned(), |n| slug(&n.to_string_lossy()));
    data_dir()
        .join("abcc")
        .join(format!("{name}-{:016x}", fingerprint(&canonical)))
}

/// The platform's per-user data directory, without a dependency to ask for it.
fn data_dir() -> PathBuf {
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local);
    }
    if let Some(xdg) = env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(xdg);
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".local/share");
    }
    env::temp_dir()
}

/// FNV-1a over the path, case-folded.
///
/// Case-folded because Windows paths are case-insensitive, so `D:\dev\abcc` and
/// `d:\dev\abcc` are one repository and must not get two logs. Not a
/// cryptographic hash and not used as one: it disambiguates directory names for a
/// human, and a collision costs a shared directory rather than anything silent.
fn fingerprint(path: &Path) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_string_lossy().to_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Everything that is not a letter, a digit, a dash or a dot becomes a dash.
fn slug(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "repo".to_owned()
    } else {
        cleaned
    }
}
