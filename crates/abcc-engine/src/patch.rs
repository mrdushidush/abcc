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
            // F638. When the exact search fails, a second and purely
            // *diagnostic* pass finds where the context came closest and reports
            // the first line that differs. It decides nothing — the patch is
            // already refused by the time it runs.
            let Some(at) = locate(lines, &expects, claims + drift, cursor) else {
                return Err(nearest_miss(&self.path, n + 1, lines, &expects, cursor));
            };

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
        // F637, before anything else: a payload carrying tool-call markup is a
        // transcript, and every message the rest of this function can produce
        // would describe it as a broken diff instead.
        if let Some(bad) = markup(&lines) {
            return Err(bad);
        }
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

/// The markers a tool-call transcript carries and a unified diff never does.
///
/// Deliberately a short, literal list rather than a general "does this look like
/// XML" rule: a diff legitimately contains almost anything, including angle
/// brackets and the word `function`. These four strings are the tool-call
/// syntax this family of models emits, and a diff that contains one of them at
/// the start of a line is not a diff that happens to mention it.
const TOOL_MARKUP: [&str; 4] = ["<tool_call>", "</tool_call>", "<function=", "<parameter="];

/// F637: is this payload a transcript rather than a diff?
///
/// 🚨 **Only *bare* lines count — ones carrying no diff prefix at all.** A diff
/// may legitimately add a line containing `<tool_call>`: this repository's own
/// research notes do, and refusing that would be a new way to reject correct
/// work. Inside a hunk every line starts with `' '`, `'+'` or `'-'`, so the
/// markup that matters is the markup that is *outside* one — which is exactly
/// where a concatenated transcript puts it.
fn markup(lines: &[&str]) -> Option<PatchError> {
    lines.iter().enumerate().find_map(|(n, line)| {
        if line.starts_with([' ', '+', '-']) {
            return None;
        }
        let text = line.trim_start();
        TOOL_MARKUP
            .iter()
            .find(|m| text.starts_with(**m))
            .map(|m| PatchError::NotADiff {
                marker: (*m).to_owned(),
                line: n + 1,
            })
    })
}

/// 🚨 **F638: where did the context come closest, and what differs there?**
///
/// Runs only after [`locate`] has already failed, so it cannot change whether a
/// patch applies — it changes only what the model is told about why it did
/// not. That separation is deliberate: the module refuses to *match* loosely,
/// and this is not matching, it is reporting.
///
/// Scores every window at or after `floor` by how many of its lines agree, and
/// keeps the first best one. Ties go to the earliest position, which is the same
/// nearest-wins rule [`locate`] uses.
fn nearest_miss(
    path: &str,
    hunk: usize,
    lines: &[&str],
    expects: &[&str],
    floor: usize,
) -> PatchError {
    let span = expects.len();
    let mut best_at = floor.min(lines.len());
    let mut best_hits = 0usize;
    let last = lines.len().saturating_sub(span);
    for at in floor..=last.max(floor) {
        let hits = expects
            .iter()
            .enumerate()
            .filter(|(i, want)| {
                lines
                    .get(at + i)
                    .is_some_and(|got| got.trim_end_matches('\r') == want.trim_end_matches('\r'))
            })
            .count();
        if hits > best_hits {
            best_hits = hits;
            best_at = at;
        }
    }

    // The first line that differs at that position, which is the one thing a
    // model can act on. `expects` is never empty here: `locate` answers `Some`
    // for an empty context, so this function is unreachable for one.
    let (line, wrote, found) = expects
        .iter()
        .enumerate()
        .find_map(|(i, want)| {
            let got = lines.get(best_at + i);
            let differs =
                got.is_none_or(|g| g.trim_end_matches('\r') != want.trim_end_matches('\r'));
            differs.then(|| {
                (
                    i + 1,
                    (*want).to_owned(),
                    got.map_or_else(
                        || "<past the end of the file>".to_owned(),
                        |g| (*g).to_owned(),
                    ),
                )
            })
        })
        .unwrap_or((1, String::new(), String::new()));

    PatchError::NoMatch {
        path: path.to_owned(),
        hunk,
        at: best_at + 1,
        matched: best_hits,
        of: span,
        line,
        wrote,
        found,
    }
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
    /// F637: the argument carries tool-call markup, so it is a transcript
    /// rather than a diff.
    ///
    /// Measured on this project's log: **32 of 46 recoverable `apply_patch`
    /// refusals (70%) contain `<tool_call>`, `<function=` or `<parameter=`
    /// inside the `diff` string** — several attempted calls concatenated with
    /// the model's prose between them, and present on the *first* call of an
    /// attempt in 14 of 14 cases, so it is not a reaction to being refused.
    ///
    /// Without this, `parse` accepts such a payload (it finds a real header
    /// near the top), applies nothing, and fails several hunks later with
    /// [`PatchError::NoMatch`] — a sentence about line numbers, for a payload
    /// whose problem is that it is not one diff. The round is spent either way;
    /// this at least spends it on a sentence the model can act on.
    ///
    /// ⚠ **2026-09-23 (PLAN-TOOL B1): the offer now names `edit_file`**, which
    /// needs a snippet rather than the whole file. The history below is why
    /// there is an offer at all.
    ///
    /// 🚨🚨 **F649: the sentence now names `write_file`, because a refusal its
    /// reader cannot act on is a loop with a receipt.** F641 put the fold in the
    /// **server's** tool-call parser, before abcc sees anything, so this message
    /// asks the model to stop doing something it may not be doing — and F648
    /// measured what that costs: `apply_patch` refused **5 of 5** across two
    /// attempts on one subject, both ending `BudgetExhausted { 24 rounds }` with
    /// **no artifact at all**, for 48 rounds and 1,280,734 input tokens.
    ///
    /// So the sentence offers the other door, and which door is evidence rather
    /// than a guess: **the one `Accomplished` in this project's entire log
    /// reached it by falling back to `write_file` after `apply_patch` failed
    /// twice**, and `write_file` is refused **1 of 17** against `apply_patch`'s
    /// 51 of 66. Both are [`Reach::Edits`](crate::tools::Reach::Edits), so any
    /// ceiling that admitted the refused call admits the one being offered —
    /// `tests/policy.rs` holds that, because a refusal naming a tool the role
    /// cannot have would be worse than the one it replaces.
    ///
    /// ⚠ **It is offered on the second refusal, not the first**, which is the
    /// shape the one success actually had. A first refusal can be an honest
    /// mistake — a transcript pasted into a real diff — and a message that
    /// abandoned the tool on sight would spend rounds steering the model off the
    /// tool that works whenever it arrives intact.
    ///
    /// ⚠ **This is a prompt, so it binds only as far as the model complies**
    /// (`tools` module docs: 39 of 50 for the shipped wrapper, 0 of 50 for the
    /// best re-aimed one). It is a string flown as an arm, not a fix — the fold
    /// it routes around is still there and still the server's.
    #[error(
        "the diff argument contains tool-call markup ({marker:?} at line {line}), so it is a \
         transcript and not a diff. Send one call, whose `diff` is only the unified diff text: \
         no `<tool_call>` wrappers, no commentary, and nothing after the last hunk. \
         If the next `apply_patch` is refused this way too, stop patching and use \
         `edit_file` instead: `path`, the exact `old_text` to replace, and its `new_text`."
    )]
    NotADiff { marker: String, line: usize },
    #[error("the diff is malformed: {detail}")]
    Malformed { detail: String },
    /// 🚨 **F638: it says what the file actually has, because the old
    /// message sent the model to fix the wrong thing.**
    ///
    /// It used to read *"hunk 1 claims line 187 and its context is nowhere in
    /// the file"*, and the observed response was the model changing the line
    /// number — 195, then 196, then 187. But the hunk header is a
    /// **hint**: [`locate`] searches the whole file outward from it, so the
    /// claimed line is the one part of a hunk that cannot cause this failure.
    /// The message named it anyway, and named it first.
    ///
    /// So the sentence now carries the evidence the model needs to write an
    /// exact patch next round: where the context came closest, how much of it
    /// matched there, and **the file's own text at the first line that
    /// differs**. Measured motivation: `apply_patch` is refused 77% of the time
    /// on this project's log. Of the refusals that genuinely ARE diffs, more
    /// than half are one context line out — `everywhere:` for
    /// `Everywhere:`, one character.
    ///
    /// ⚠ **This is a change to the diagnosis and not to the matching.**
    /// Application stays exact (module docs: whitespace-insensitive matching is
    /// a guess about intent, and what it hides is a patch applied in the wrong
    /// place). Nothing here makes a patch apply that would not have applied.
    #[error(
        "{path}: hunk {hunk} does not match. The closest place is line {at}, where {matched} of \
         its {of} context lines match. The first difference is line {line} of the hunk:\n  \
         you wrote:    {wrote:?}\n  the file has: {found:?}"
    )]
    NoMatch {
        path: String,
        hunk: usize,
        /// 1-based, in the file.
        at: usize,
        matched: usize,
        of: usize,
        /// 1-based, within the hunk's expected lines.
        line: usize,
        wrote: String,
        found: String,
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
