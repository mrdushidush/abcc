//! A unified-diff applier, in this process.
//!
//! 🚨 **It is native rather than `git apply` for a structural reason, not a
//! preference.** `apply_patch` declares [`Reach::Edits`](crate::tools::Reach),
//! and this crate *derives* the exec class from reach — so an `apply_patch` that
//! shells out to git would be an exec-class tool admitted at
//! [`Tier::Write`](crate::tools::Tier). That is precisely the donor defect
//! ADR-0014 §2 exists to prevent: four tools identified as one class for one
//! control and gated separately for another. The derivation is only true if the
//! implementation makes it true.
//!
//! The same argument settles `list_files`, whose ignore rules are read in-process
//! rather than asked of `git ls-files`.
//!
//! # What it accepts
//!
//! Unified diffs, with or without git's extended headers, including creations
//! (`--- /dev/null`) and deletions (`+++ /dev/null`). Hunk headers are used as a
//! *hint*: a hunk is located by matching its own context, searching outward from
//! the line it claims, because a model that read the file through a range window
//! routinely gets the absolute line number wrong and is usually right about the
//! code.
//!
//! ⚠ **Matching is exact apart from one thing: a trailing carriage return.**
//! Content on this platform is frequently CRLF and a model writes LF, so
//! comparing CR-stripped lines is the difference between working and never
//! applying a patch here. The file's own dominant ending is what gets written
//! back. Nothing else is fuzzed — whitespace-insensitive matching is a guess
//! about intent, and what it hides is a patch applied in the wrong place.
//!
//! ⚠ **A patch is all-or-nothing across every file it touches.** A half-applied
//! diff leaves a tree that compiles for reasons nobody chose, and the model
//! cannot see that it happened.

use std::fmt::Write as _;

/// What a diff does to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Create,
    Modify,
    Delete,
}

/// One line of a hunk, and which side it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Context,
    Remove,
    Add,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Hunk {
    /// The line the hunk claims to start at, 1-based. A hint, never the answer.
    claims: usize,
    lines: Vec<(Op, String)>,
}

impl Hunk {
    /// The lines this hunk expects to find, in order.
    fn expects(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter(|(op, _)| *op != Op::Add)
            .map(|(_, text)| text.as_str())
            .collect()
    }
}

/// Everything one diff does to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    /// Workspace-relative, with git's `a/` and `b/` prefixes stripped.
    pub path: String,
    pub kind: Change,
    hunks: Vec<Hunk>,
}

impl FilePatch {
    /// How many hunks this file's patch has. It goes into what the model is told,
    /// so a diff that lost half of itself in transit is visible to its author.
    #[must_use]
    pub fn hunk_count(&self) -> usize {
        self.hunks.len()
    }

    /// Apply this file's hunks to `original`, or to nothing when it creates the
    /// file. `None` comes back when the patch deletes the file.
    ///
    /// # Errors
    ///
    /// [`PatchError::NoMatch`] when a hunk's context is nowhere in the file, and
    /// [`PatchError::Malformed`] when the diff says it creates a file that is
    /// already there.
    pub fn apply(&self, original: Option<&str>) -> Result<Option<String>, PatchError> {
        match self.kind {
            Change::Delete => Ok(None),
            Change::Create => {
                if original.is_some_and(|o| !o.is_empty()) {
                    return Err(PatchError::Malformed {
                        detail: format!("{} already exists and the diff creates it", self.path),
                    });
                }
                let added: Vec<&str> = self
                    .hunks
                    .iter()
                    .flat_map(|h| h.lines.iter())
                    .filter(|(op, _)| *op != Op::Remove)
                    .map(|(_, text)| text.as_str())
                    .collect();
                Ok(Some(join(&added, "\n", true)))
            }
            Change::Modify => {
                let source = original.unwrap_or_default();
                let (lines, ending, trailing) = split(source);
                let rebuilt = self.rebuild(&lines)?;
                Ok(Some(join(&rebuilt, ending, trailing)))
            }
        }
    }

    /// Walk the hunks in order, copying the untouched runs between them.
    fn rebuild<'a>(&'a self, lines: &[&'a str]) -> Result<Vec<&'a str>, PatchError> {
        let mut out: Vec<&str> = Vec::with_capacity(lines.len());
        let mut cursor = 0usize;
        let mut drift = 0isize;

        for (n, hunk) in self.hunks.iter().enumerate() {
            let expects = hunk.expects();
            let claims = isize::try_from(hunk.claims.saturating_sub(1)).unwrap_or(0);
            let at = locate(lines, &expects, claims + drift, cursor).ok_or_else(|| {
                PatchError::NoMatch {
                    path: self.path.clone(),
                    hunk: n + 1,
                    claims: hunk.claims,
                    first: expects
                        .first()
                        .map_or_else(String::new, |l| (*l).to_owned()),
                }
            })?;

            out.extend_from_slice(&lines[cursor..at]);
            for (op, text) in &hunk.lines {
                if *op != Op::Remove {
                    out.push(text);
                }
            }
            cursor = at + expects.len();
            drift = isize::try_from(at).unwrap_or(0) - claims;
        }
        out.extend_from_slice(&lines[cursor.min(lines.len())..]);
        Ok(out)
    }
}

/// A parsed diff.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Patch {
    pub files: Vec<FilePatch>,
}

impl Patch {
    /// Parse a unified diff.
    ///
    /// # Errors
    ///
    /// [`PatchError::NoHeader`] when there is no `---`/`+++` pair anywhere, and
    /// [`PatchError::Malformed`] for a hunk header that is not one.
    pub fn parse(diff: &str) -> Result<Patch, PatchError> {
        let lines: Vec<&str> = diff.lines().collect();
        let mut files: Vec<FilePatch> = Vec::new();
        let mut i = 0usize;

        while i < lines.len() {
            let line = lines[i];
            let Some(old) = line.strip_prefix("--- ") else {
                i += 1;
                continue;
            };
            let Some(new) = lines.get(i + 1).and_then(|l| l.strip_prefix("+++ ")) else {
                return Err(PatchError::Malformed {
                    detail: format!("a `---` line with no `+++` after it: {line}"),
                });
            };
            let (path, kind) = heads(old, new)?;
            i += 2;
            let mut hunks = Vec::new();
            while i < lines.len() && lines[i].starts_with("@@") {
                let (read, next) = hunk(&lines, i)?;
                hunks.push(read);
                i = next;
            }
            files.push(FilePatch { path, kind, hunks });
        }

        if files.is_empty() {
            return Err(PatchError::NoHeader);
        }
        Ok(Patch { files })
    }
}

/// Read one `--- a/x` / `+++ b/x` pair into a path and a kind.
fn heads(old: &str, new: &str) -> Result<(String, Change), PatchError> {
    let old = strip(old);
    let new = strip(new);
    let kind = match (old.as_str(), new.as_str()) {
        ("/dev/null", "/dev/null") => {
            return Err(PatchError::Malformed {
                detail: "a diff from /dev/null to /dev/null".to_owned(),
            });
        }
        ("/dev/null", _) => Change::Create,
        (_, "/dev/null") => Change::Delete,
        _ => Change::Modify,
    };
    let path = if kind == Change::Delete { old } else { new };
    Ok((path, kind))
}

/// One file header line, without its `a/`/`b/` prefix, its timestamp column or
/// its quoting.
fn strip(raw: &str) -> String {
    // A timestamp is separated by a tab; git omits it and diff(1) does not.
    let head = raw.split('\t').next().unwrap_or(raw).trim();
    let head = head.trim_matches('"');
    if head == "/dev/null" {
        return head.to_owned();
    }
    let head = head
        .strip_prefix("a/")
        .or_else(|| head.strip_prefix("b/"))
        .unwrap_or(head);
    head.replace('\\', "/")
}

/// Read one `@@ -l,c +l,c @@` hunk, starting at `lines[at]`.
fn hunk(lines: &[&str], at: usize) -> Result<(Hunk, usize), PatchError> {
    let claims = claimed(lines[at])?;
    let mut body = Vec::new();
    let mut i = at + 1;

    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("@@") || line.starts_with("--- ") || line.starts_with("diff ") {
            break;
        }
        // `\ No newline at end of file` is a note about the line before it, and
        // whether a file ends with a newline is decided by the file.
        if line.starts_with('\\') {
            i += 1;
            continue;
        }
        let (op, text) = match line.split_at_checked(1) {
            Some((" ", rest)) => (Op::Context, rest),
            Some(("-", rest)) => (Op::Remove, rest),
            Some(("+", rest)) => (Op::Add, rest),
            // An empty line inside a hunk is a context line whose single leading
            // space was eaten — by an editor, by a chat client, or by the model.
            // Reading it as the end of the hunk is how a patch silently loses its
            // tail.
            None => (Op::Context, ""),
            Some(_) => break,
        };
        body.push((op, text.trim_end_matches('\r').to_owned()));
        i += 1;
    }

    Ok((
        Hunk {
            claims,
            lines: body,
        },
        i,
    ))
}

/// The old-side start line a hunk header claims.
fn claimed(header: &str) -> Result<usize, PatchError> {
    let malformed = || PatchError::Malformed {
        detail: format!("not a hunk header: {header}"),
    };
    let old = header
        .split_whitespace()
        .find(|t| t.starts_with('-'))
        .ok_or_else(malformed)?;
    let start = old
        .trim_start_matches('-')
        .split(',')
        .next()
        .ok_or_else(malformed)?;
    start.parse::<usize>().map_err(|_| malformed())
}

/// Find where a hunk's expected lines actually are.
///
/// 🚨 The hunk header is a hint and the context is the evidence. The search runs
/// outward from the claimed position so the *nearest* match wins, which is what
/// keeps two similar hunks in one file from swapping places.
fn locate(lines: &[&str], expects: &[&str], guess: isize, floor: usize) -> Option<usize> {
    let floor = isize::try_from(floor).unwrap_or(0);
    let last = isize::try_from(lines.len().saturating_sub(expects.len())).unwrap_or(0);
    if expects.is_empty() {
        return usize::try_from(guess.clamp(floor, last.max(floor))).ok();
    }
    for step in 0..=isize::try_from(lines.len()).unwrap_or(0) {
        for at in [guess - step, guess + step] {
            if at < floor || at > last {
                continue;
            }
            let at = usize::try_from(at).ok()?;
            if same(&lines[at..at + expects.len()], expects) {
                return Some(at);
            }
            if step == 0 {
                break;
            }
        }
    }
    None
}

/// Exact, apart from a trailing carriage return. See the module note.
fn same(found: &[&str], expects: &[&str]) -> bool {
    found
        .iter()
        .zip(expects)
        .all(|(a, b)| a.trim_end_matches('\r') == b.trim_end_matches('\r'))
}

/// Split content into lines, its dominant ending, and whether it ends with one.
fn split(source: &str) -> (Vec<&str>, &'static str, bool) {
    let crlf = source.matches("\r\n").count();
    let lf = source.matches('\n').count() - crlf;
    let ending = if crlf > lf { "\r\n" } else { "\n" };
    let trailing = source.ends_with('\n');
    let mut lines: Vec<&str> = source
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    if trailing {
        lines.pop();
    }
    (lines, ending, trailing)
}

fn join(lines: &[&str], ending: &str, trailing: bool) -> String {
    let mut out = lines.join(ending);
    if trailing && !out.is_empty() {
        out.push_str(ending);
    }
    out
}

/// Why a diff was not applied.
///
/// Every arm is a sentence the model can act on, for the same reason a denial
/// goes into its transcript: a refusal it cannot read is a refusal it asks for
/// again, and the round budget is what pays for that.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    #[error("the diff has no file header; a unified diff needs `--- <path>` and `+++ <path>`")]
    NoHeader,
    #[error("the diff is malformed: {detail}")]
    Malformed { detail: String },
    #[error(
        "{path}: hunk {hunk} claims line {claims} and its context is nowhere in the file; \
         it starts {first:?}"
    )]
    NoMatch {
        path: String,
        hunk: usize,
        claims: usize,
        first: String,
    },
}

/// One line naming what a patch did, for the model's own transcript.
#[must_use]
pub fn summarise(applied: &[(String, usize)]) -> String {
    let hunks: usize = applied.iter().map(|(_, n)| n).sum();
    let mut out = format!(
        "applied {hunks} hunk{} to {} file{}:",
        plural(hunks),
        applied.len(),
        plural(applied.len())
    );
    for (path, n) in applied {
        let _ = write!(out, " {path} ({n})");
    }
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
