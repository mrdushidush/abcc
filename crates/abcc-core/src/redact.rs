//! Redaction at the tool-result boundary, as a property of the result type.
//!
//! ADR-0014 §5. The threat-model rows this closes are **credentials on disk**
//! and **console and session on disk**: any secret in any tool result reaches
//! three sinks — the durable log, the console that projects it, and the model's
//! own context — and the donor redacted the first two and neither of the others
//! (F418).
//!
//! 🚨 **This is not the control and it may not be described as one.** The
//! control for *credentials on disk* is denying the class (ADR-0014 §1): a role
//! without the exec tier cannot run `cat ~/.ssh/id_rsa` in the first place.
//! Redaction is what limits the damage of the results a role **is** allowed to
//! ask for, and a denylist over text is exactly the shape W7 measured at 39 of
//! 50 and rejected as a control (F404–F406, F416). It ships as what it is.
//!
//! ## Why it is a type and not a function somebody calls
//!
//! [`Scrubbed`] has a private field and one constructor. The two sinks that
//! take free text out of a tool call — [`Event::ToolCallEnded`]'s `arguments`
//! and the `Role::Tool` message appended to the body — take a `Scrubbed`, so a
//! second path from a tool's output to the log or the context **does not
//! compile** until somebody has passed it through here. The donor's denylist
//! was a good one hung off `validate_read_path`, which `bash` never calls; a
//! check that is not on the path is the defect this shape exists to prevent
//! (F422's sibling).
//!
//! ⚠ `Scrubbed` derives `Deserialize`, so reading the log back reconstitutes
//! one. That is the log handing back text it already scrubbed, not a second
//! door: nothing else in the workspace deserializes one from raw input.
//!
//! [`Event::ToolCallEnded`]: crate::event::Event::ToolCallEnded

use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// What replaces a match. Fixed rather than per-shape, because the operator's
/// question is *was something removed here*, and a family of markers invites
/// reading the marker as a classification of a thing nobody looked at.
pub const MARKER: &str = "[redacted]";

/// Below this, a literal is not searched for. A one- or two-character API key is
/// not a credential, and scrubbing every occurrence of `a` would empty the
/// transcript — a denylist whose entry is short enough to match prose is a
/// denial of service against the model's own context.
const MIN_LITERAL: usize = 8;

/// Text that has been through [`Secrets::scrub`].
///
/// 🚨 **There is no `From<String>` and no public field**, which is the whole
/// point: the type is the proof that the boundary was crossed, once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Scrubbed(String);

impl Scrubbed {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Scrubbed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A shape the scrubber knows, named so the record can say what class was
/// removed without keeping what was removed.
///
/// ⚠ **Every arm is a heuristic except [`Kind::Known`]**, which is an exact
/// match against a value this process actually holds and is therefore the only
/// one with no false-positive story at all. That asymmetry is why the two halves
/// are counted separately (F686).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A literal this process holds — the model API key, today.
    Known,
    /// `-----BEGIN … PRIVATE KEY-----` through its `-----END …-----`.
    PrivateKey,
    /// An `Authorization:` header value.
    AuthHeader,
    /// A vendor-prefixed token: `sk-`, `ghp_`, `xoxb-`, `AKIA…`.
    VendorToken,
    /// `SOMETHING_SECRET=value`, in the shape an env file or a shell export
    /// has — and **the name half is read in that shape's case**, so a
    /// lowercase `secrets:` in source is not one of these (F712).
    Assignment,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::Known => "a value this process holds",
            Kind::PrivateKey => "a private key block",
            Kind::AuthHeader => "an authorization header",
            Kind::VendorToken => "a vendor-prefixed token",
            Kind::Assignment => "a secret-shaped assignment",
        })
    }
}

/// What one scrub removed. Kept beside the text because *something was removed
/// here* is a fact the operator needs and the marker alone does not carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removed {
    pub kind: Kind,
    pub count: usize,
}

/// The result of scrubbing: the text, and what came out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scrub {
    pub text: Scrubbed,
    /// Empty when nothing matched, which is the overwhelmingly common case.
    pub removed: Vec<Removed>,
}

impl Scrub {
    #[must_use]
    pub fn touched(&self) -> bool {
        !self.removed.is_empty()
    }

    /// One line for the log, or `None` when nothing was removed.
    ///
    /// ⚠ It names the class and the count and never the value — a note that
    /// quoted what it removed would put the secret back on the log one field to
    /// the left.
    #[must_use]
    pub fn note(&self) -> Option<String> {
        if self.removed.is_empty() {
            return None;
        }
        let parts: Vec<String> = self
            .removed
            .iter()
            .map(|r| format!("{}x {}", r.count, r.kind))
            .collect();
        Some(format!("redacted: {}", parts.join(", ")))
    }
}

/// The denylist: the literals this process holds, plus the shapes.
///
/// 🚨 **Two halves, deliberately unequal.** The literal half is exact and is the
/// only half that can be said to *work*; the shape half is a heuristic that
/// catches the well-known formats and will miss a credential that looks like
/// prose. A `Secrets` with no literals in it is still useful and is still not a
/// control.
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    literals: Vec<String>,
}

impl Secrets {
    /// A scrubber with the shapes and no literals.
    #[must_use]
    pub fn shapes_only() -> Secrets {
        Secrets {
            literals: Vec::new(),
        }
    }

    /// Add a value this process holds.
    ///
    /// ⚠ Values shorter than [`MIN_LITERAL`] are **dropped rather than
    /// rejected**: a caller passing an empty or trivial key is the ordinary case
    /// of *there is no key configured*, and failing there would put a `Result`
    /// on the construction of the thing that protects the log.
    #[must_use]
    pub fn with_literal(mut self, value: impl AsRef<str>) -> Secrets {
        let value = value.as_ref().trim();
        if value.len() >= MIN_LITERAL {
            self.literals.push(value.to_owned());
        }
        self
    }

    /// How many literals are actually being searched for. For the operator, and
    /// for the test that asserts a short key was dropped rather than kept.
    #[must_use]
    pub fn literals(&self) -> usize {
        self.literals.len()
    }

    /// Scrub the text. **The one constructor of [`Scrubbed`].**
    ///
    /// Literals first: an exact hit is the high-precision half, and running it
    /// first means a key that also matches a shape is counted as what it is.
    #[must_use]
    pub fn scrub(&self, text: impl Into<String>) -> Scrub {
        let mut text = text.into();
        let mut removed: Vec<Removed> = Vec::new();

        for literal in &self.literals {
            let hits = text.matches(literal.as_str()).count();
            if hits > 0 {
                text = text.replace(literal.as_str(), MARKER);
                bump(&mut removed, Kind::Known, hits);
            }
        }

        for (kind, pattern) in shapes() {
            let mut hits = 0usize;
            let replaced = pattern.replace_all(&text, |caps: &regex::Captures<'_>| {
                let (replacement, changed) = rewrite(caps);
                if changed {
                    hits += 1;
                }
                replacement
            });
            if hits > 0 {
                text = replaced.into_owned();
                bump(&mut removed, *kind, hits);
            }
        }

        removed.sort_by_key(|r| r.kind);
        Scrub {
            text: Scrubbed(text),
            removed,
        }
    }
}

/// What a shape leaves behind, and whether it removed anything.
///
/// A pattern with a capture group named `keep` keeps that group and replaces the
/// rest, so `Authorization: Bearer xyz` stays recognisable as an authorization
/// header rather than becoming an unattributable marker. Without one, the whole
/// match goes.
///
/// 🚨 **The second half of the tuple is why scrubbing twice is a no-op, and it
/// is structural rather than a special case for [`MARKER`].** A rewrite that
/// produces exactly what it matched removed nothing, so it is not counted —
/// which is the whole of the idempotence property. Matching the marker
/// *textually* instead would be a fifth shape to keep in step with the other
/// four, and `Authorization: Bearer [redacted]` genuinely does match the
/// authorization rule: the pattern is right, the rewrite is empty.
fn rewrite(caps: &regex::Captures<'_>) -> (String, bool) {
    let replacement = match caps.name("keep") {
        Some(kept) => format!("{}{MARKER}", kept.as_str()),
        None => MARKER.to_owned(),
    };
    let whole = caps.get(0).map_or("", |m| m.as_str());
    let changed = replacement != whole;
    (replacement, changed)
}

fn bump(removed: &mut Vec<Removed>, kind: Kind, count: usize) {
    match removed.iter_mut().find(|r| r.kind == kind) {
        Some(existing) => existing.count += count,
        None => removed.push(Removed { kind, count }),
    }
}

/// The shape table.
///
/// ⚠ **Order matters and it is the order of decreasing certainty.** A private
/// key block is unmistakable; an assignment is the loosest rule here and runs
/// last, so a `Bearer` token inside one is already gone by the time it sees it.
///
/// 🚨 **Compiled once.** These are constant inputs to a `Regex`, so a failure to
/// compile is a bug in this file and nowhere else, and `expect` fires on the
/// first scrub in the process rather than in the field.
fn shapes() -> &'static [(Kind, Regex)] {
    static SHAPES: LazyLock<Vec<(Kind, Regex)>> = LazyLock::new(|| {
        vec![
            (
                Kind::PrivateKey,
                // `(?s)` so the body may span lines, and lazy so two keys in one
                // file are two matches rather than everything between them.
                Regex::new(
                    r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
                )
                .expect("private key shape"),
            ),
            (
                Kind::AuthHeader,
                Regex::new(r"(?i)(?P<keep>authorization\s*[:=]\s*(?:bearer|basic|token)\s+)\S+")
                    .expect("auth header shape"),
            ),
            (
                Kind::VendorToken,
                Regex::new(
                    r"\b(?:sk-[A-Za-z0-9_\-]{16,}|gh[pousr]_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{20,}|xox[baprs]-[A-Za-z0-9\-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_\-]{20,})",
                )
                .expect("vendor token shape"),
            ),
            (
                Kind::Assignment,
                // The name half is the discriminating half; the value must also
                // be long enough to be a secret, so a bare `token=1` is not
                // matched and a `PASSWORD=` with nothing after it is not either.
                //
                // 🚨 **The name half is case-SENSITIVE, and that is the whole of
                // F712's fix.** With `(?i)` it matched the English word
                // `secrets`, so `pub fn secrets(mut self, secrets: Secrets) ->`
                // reached the model as `secrets: [redacted] ->` — the type gone
                // and the closing paren with it, because the value half is
                // greedy over non-space characters and Rust is not an env file.
                // 14 of this workspace's 132 files were rewritten before a model
                // saw them. An env var is `SECRET_KEY`; the cost of the fix is a
                // lowercase YAML `password:`, and the operator ruled it
                // (2026-09-12). ⚠ Do not put `(?i)` back without moving the
                // value half off `[^\s"'#]` first.
                Regex::new(
                    r#"(?P<keep>\b[A-Z0-9_]*(?:API[_-]?KEY|SECRET|PASSWORD|PASSWD|CREDENTIALS?|ACCESS[_-]?TOKEN|AUTH[_-]?TOKEN|PRIVATE[_-]?KEY)[A-Z0-9_]*\s*[:=]\s*)["']?[^\s"'#]{8,}["']?"#,
                )
                .expect("assignment shape"),
            ),
        ]
    });
    &SHAPES
}
