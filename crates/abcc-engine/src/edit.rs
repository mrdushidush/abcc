//! `edit_file`'s string half: replace one unique snippet, and when the snippet
//! is not there, say how close the file came.
//!
//! PLAN-TOOL D1. Reimplemented from claudette's `edit_file` and `near_miss.rs`
//! (decision 2a, 2026-09-23), with claudette's tests carried over as the spec in
//! `tests/edit.rs`. Pure functions over strings; [`crate::workspace`] does the
//! reading and the atomic write.

use std::fmt;

/// Files larger than this skip the near-miss scan on the failure path.
const NEAR_MISS_MAX_BYTES: usize = 256 * 1024;
/// Blocks longer than this are not scanned for a near miss.
const NEAR_MISS_MAX_LINES: usize = 64;
/// Quoted fragments in a hint are cut to this many characters.
const SNIPPET_CHARS: usize = 120;
/// The most match sites an ambiguity refusal lists.
const LISTED_SITES: usize = 8;

/// A replacement that was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edited {
    /// The whole new file.
    pub content: String,
    /// How many occurrences were replaced.
    pub replacements: usize,
    /// The 1-based line the first replacement starts on.
    pub line: usize,
}

/// Why nothing was replaced. Each arm's text is the sentence the model reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    EmptyOldText,
    NotFound { hint: Option<String> },
    Ambiguous { count: usize, lines: Vec<usize> },
    NoChange,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::EmptyOldText => f.write_str(
                "old_text is empty. Copy the lines you want to change from the file into old_text.",
            ),
            EditError::NotFound { hint } => {
                f.write_str("old_text was not found in the file. ")?;
                f.write_str(hint.as_deref().unwrap_or(
                    "It must match the file exactly, whitespace included: \
                     read the lines you mean and copy them.",
                ))
            }
            EditError::Ambiguous { count, lines } => {
                let shown: Vec<String> = lines.iter().map(ToString::to_string).collect();
                let more = if *count > lines.len() { ", …" } else { "" };
                write!(
                    f,
                    "old_text appears {count} times (at lines {}{more}). Add surrounding \
                     lines to old_text until it is unique, or pass replace_all to change \
                     every one.",
                    shown.join(", ")
                )
            }
            EditError::NoChange => f.write_str(
                "no change: old_text and new_text are identical, so nothing was written. \
                 What you meant to change may already be there. Do not send this edit again \
                 unchanged.",
            ),
        }
    }
}

/// Replace `old` with `new` in `content`: exactly one occurrence, or every
/// occurrence when `replace_all` is set.
///
/// # Errors
///
/// An empty `old`, no occurrence (with a near-miss hint when there is one), more
/// than one without `replace_all`, or a result identical to the input.
pub fn replace(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<Edited, EditError> {
    if old.is_empty() {
        return Err(EditError::EmptyOldText);
    }
    let sites: Vec<usize> = content.match_indices(old).map(|(at, _)| at).collect();
    let Some(&first) = sites.first() else {
        return Err(EditError::NotFound {
            hint: near_miss_hint(content, old),
        });
    };
    if sites.len() > 1 && !replace_all {
        return Err(EditError::Ambiguous {
            count: sites.len(),
            lines: sites
                .iter()
                .take(LISTED_SITES)
                .map(|at| line_of(content, *at))
                .collect(),
        });
    }
    let replaced = if replace_all {
        content.replace(old, new)
    } else {
        content.replacen(old, new, 1)
    };
    if replaced == content {
        return Err(EditError::NoChange);
    }
    Ok(Edited {
        content: replaced,
        replacements: sites.len(),
        line: line_of(content, first),
    })
}

/// The 1-based line a byte offset sits on.
fn line_of(content: &str, at: usize) -> usize {
    content[..at].matches('\n').count() + 1
}

/// Why `block` is not in `content`, if the file came close enough to say.
///
/// Two diagnoses, in order: doubled backslashes (the JSON-escaping mistake
/// local models make with raw-string regexes), then the window of lines that
/// matches the block best after trimming. Fewer than half the lines matching is
/// not a near miss, and `None` sends the generic advice instead.
#[must_use]
pub fn near_miss_hint(content: &str, block: &str) -> Option<String> {
    if block.is_empty() || content.len() > NEAR_MISS_MAX_BYTES {
        return None;
    }

    if block.contains("\\\\") {
        let single = block.replace("\\\\", "\\");
        if found_trimmed(content, &single) {
            let sample = block
                .lines()
                .find(|l| l.contains("\\\\"))
                .map(|l| cut(l.trim()))
                .unwrap_or_default();
            return Some(format!(
                "Your old_text over-escapes backslashes: the file has single \
                 backslashes where yours are doubled (your `{sample}`). Send the same \
                 edit with single backslashes."
            ));
        }
    }

    let lines: Vec<&str> = content.lines().collect();
    let want: Vec<&str> = block.lines().map(str::trim).collect();
    let m = want.len();
    if m == 0 || m > NEAR_MISS_MAX_LINES || lines.len() < m {
        return None;
    }
    let score = |i: usize| (0..m).filter(|&j| lines[i + j].trim() == want[j]).count();
    let (start, best) = (0..=lines.len() - m)
        .map(|i| (i, score(i)))
        // The first window wins a tie, so the hint points at the earliest one.
        .fold((0, 0), |acc, cur| if cur.1 > acc.1 { cur } else { acc });
    if best == 0 || best * 2 < m {
        return None;
    }

    let (from, to) = (start + 1, start + m);
    Some(match (0..m).find(|&j| lines[start + j].trim() != want[j]) {
        Some(j) => format!(
            "Closest match: lines {from}-{to} ({best}/{m} lines match). First difference \
                 at line {}: the file has `{}` but your old_text has `{}`.",
            start + j + 1,
            cut(lines[start + j].trim()),
            cut(want[j]),
        ),
        None => format!(
            "Lines {from}-{to} match your old_text except for whitespace or \
                 indentation. Copy those lines exactly as the file has them."
        ),
    })
}

/// Whether `block` is in `content`, exactly or as a run of trimmed-equal lines.
fn found_trimmed(content: &str, block: &str) -> bool {
    if content.contains(block) {
        return true;
    }
    let lines: Vec<&str> = content.lines().collect();
    let want: Vec<&str> = block.lines().map(str::trim).collect();
    let m = want.len();
    m > 0
        && lines.len() >= m
        && (0..=lines.len() - m).any(|i| (0..m).all(|j| lines[i + j].trim() == want[j]))
}

/// A fragment cut to [`SNIPPET_CHARS`] characters, with an ellipsis when cut.
fn cut(s: &str) -> String {
    if s.chars().count() <= SNIPPET_CHARS {
        return s.to_owned();
    }
    let head: String = s.chars().take(SNIPPET_CHARS).collect();
    format!("{head}…")
}
