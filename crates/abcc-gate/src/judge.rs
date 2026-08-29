//! **Judge** — the gate's other half, and the one thing in it that cannot refuse.
//!
//! ADR-0008's line is two lines, and this module is the second one:
//!
//! ```text
//! Accept  ⇔  structural ∧ acceptance ∧ ¬Veto        (and every one of them Measured)
//! Judge   →  a report attached to the attempt, never a term in the conjunction
//! ```
//!
//! # 🚨 There is nothing here to wire a vote into, and that is deliberate
//!
//! [`read`] returns an [`abcc_core::outcome::Claim`]. A `Claim` attaches through
//! [`Report::note`](abcc_core::outcome::Report::note), which touches no
//! [`Outcome`](abcc_core::outcome::Outcome) and therefore no
//! [`Headline`](abcc_core::outcome::Headline) — and there is no function in this
//! workspace that converts a `Claim` into an `Outcome`. So the Judge's verdict
//! reaches the report and stops, `AttemptPhase::may_refuse` stays
//! `!uses_model()`, and making the model's opinion count would be a new function
//! somebody has to write rather than a flag somebody can flip.
//!
//! ⚠ **The same rule runs the other way, and it is the one that is easy to
//! miss: the Judge's own failure is not the attempt's.** A review that times out,
//! says nothing, or comes back malformed leaves the ending exactly where the
//! measurements put it. A gate whose *reporter* can fail an attempt is a gate
//! with a fifth rung nobody declared.
//!
//! # This module makes no call
//!
//! It is the schema, the brief, the shape of the answer and the rendering — all
//! pure, all testable without a server. The call belongs to `abcc-drive`, which
//! is where the store, the repository and the provider already meet; putting a
//! `TurnLoop` in here would give the crate that measures a provider seam it has
//! no other use for.
//!
//! # The four rules it is built under, with the numbers (ADR-0008)
//!
//! 1. 🚨 **Show it the other artifact, never a threshold.** Pointwise scoring is
//!    **0 of 8** — the model has no absolute scale. Pairwise against the
//!    pre-image is **14 of 14** on the same defects and **order-stable on 7 of
//!    7 pairs**; position bias did not reproduce, and verbosity bias did only
//!    weakly and only on a genuine tie. So what it is shown is a *diff*
//!    ([`abcc_vcs::Repo::patch_between`]), whose `-` lines are the pre-image and
//!    whose `+` lines are the post-image.
//! 2. 🚨 **A finding must carry something runnable or it is not a finding.**
//!    Running the reviewer's own `call → expected → actual` catches **10 of 23**
//!    wrong trees against **1 of 34** false — three times the recall of the same
//!    model's bare verdict, out of the same call. That triple is why [`Finding`]
//!    has the fields it has and why they are all `required` in [`REVIEW`].
//! 3. 🚨 **What it reads decides its quality, not whose weights it is.** Same
//!    model, fresh call: **11/12** reading the diff, **5/12** reading the
//!    author's completion report (F280), **0/3** when the report is added
//!    *alongside* the diff (F281 — the report is *subtractive*), 4/12 with five
//!    empty payloads when it continues the author's own conversation (F282). So
//!    the body is fresh, and **nothing the author wrote in prose is in it.**
//! 4. **A second model is not the answer** (F275, F284; ADR-0003): co-residency
//!    is arithmetically impossible on this card, the swap is 26.3 s round trip
//!    plus 4.6× the decode, and the second model returned no verdict at all on
//!    the correct answer 3 of 3.
//!
//! # 🚨 What is left out of the brief, and why each absence is a decision
//!
//! * **Builders' claim** — rule 3. It is the measured worst input available.
//! * **Recon's brief.** `Head::Commandos`' charter used to promise it. The only
//!   measurement this project has about adding prose beside a diff is F281's
//!   **0 of 3**, and a localizer's brief is prose beside a diff; it is a
//!   different unit's prose than the one F281 tested, which makes it *untested*
//!   rather than *safe*. The charter was corrected to match the evidence rather
//!   than the evidence assumed to match the charter. ▶ It is a one-line change
//!   to put back, and the population that would justify it is the corpora.
//! * **The headline.** It sees every rung and not the conjunction they add up
//!   to. A reviewer shown the decision is a reviewer asked to agree with it, and
//!   ADR-0008's first rule is that it is shown the artifact rather than the
//!   verdict on it. The rungs carry strictly more information anyway.
//!
//! # ⚠ The schema is not free, and this stack has already lost calls to it
//!
//! ADR-0008 measured constrained output at **2.8× the decode, 2.7× the wall
//! clock, and 17 of 57 calls lost to the token cap** — every one legibly
//! `finish_reason: length`, which is [`Why::TruncatedAtCap`] here and a
//! measurement rather than a score. Two things follow, and both are in
//! [`REVIEW`]: the array is **bounded**, because an unbounded array is the shape
//! that ran into that cap; and there are **no `description` strings**, because a
//! schema on this stack becomes a decoding grammar and it is not established
//! that the model ever reads it. Every instruction the reviewer needs is in
//! [`brief`], where it is certainly read.

use std::fmt::Write as _;

use abcc_core::outcome::{Claim, Outcome, Report};
use abcc_engine::Schema;
use serde::{Deserialize, Serialize};

use crate::Measured;

/// The call sign that signs the claim. It is [`abcc_engine::Head::Commandos`]'s,
/// and it is spelled once so the log's `ClaimRecorded.by` and the report's
/// `Claim.by` cannot drift.
pub const BY: &str = "Commandos";

/// 🚨 **The largest diff this phase will review, in characters.**
///
/// **Derived, not tuned.** The champion is loaded at a 32,768-token window
/// (David's ruling of 2026-08-30: stay at 32k until after the corpora) and
/// [`abcc_engine::Head::budget`] reserves 16,384 of it for the answer, so
/// everything sent has to fit in the other 16,384. Diff text is token-dense —
/// roughly three characters a token rather than four — which puts the whole
/// prompt at about 49,000 characters, and the head, the task and the
/// measurements are in there too. Half of that budget is the diff's share.
///
/// ⚠ **Over it, the Judge is not asked and the reason is written down — the
/// diff is never cut to fit.** A review of part of a change is a review of a
/// different change, and it would arrive indistinguishable from a review of the
/// whole one. Being told *no review, the diff is 61,000 characters* costs an
/// operator nothing they did not already know; a confident review of the first
/// third costs them the thing the gate exists to give them.
///
/// ⚠ It is an assumption about a server this code cannot ask, and it is
/// falsifiable in flight: when it is wrong the call comes back
/// [`Why::ContextOverflow`](abcc_core::outcome::Why::ContextOverflow) with the
/// window *observed* (F498), and that is a number to correct this one with.
pub const MAX_PATCH_CHARS: usize = 24_000;

/// The constrained-output contract for one review.
///
/// `strict: true` (ADR-0011 §3, and `openai.rs` sends nothing else) requires
/// every object to close `additionalProperties` and to list every property in
/// `required` — so a field here is a field the model must produce, which is the
/// point: [`Finding`]'s `call`/`expected`/`actual` are rule 2, and a schema that
/// let them be omitted would be rule 2 written as a suggestion.
///
/// ⚠ **Never `json_object`**: the server answers HTTP 400 (ADR-0011 §3), which
/// is not a style preference.
pub const REVIEW: Schema = Schema {
    name: "commandos_review",
    json: REVIEW_JSON,
};

/// 🚨 `maxItems` is a **stop, not an opinion about how much is wrong.** The
/// Judge does not vote, so a sixth finding that does not fit costs one line of a
/// report; an array that runs into the token cap costs the whole call, which is
/// what happened to 17 of 57 of them (ADR-0008). Bound the artifact, never the
/// verdict — there is no verdict here to bound.
const REVIEW_JSON: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["assessment", "findings"],
  "properties": {
    "assessment": { "type": "string" },
    "findings": {
      "type": "array",
      "maxItems": 5,
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["at", "defect", "call", "expected", "actual"],
        "properties": {
          "at": { "type": "string" },
          "defect": { "type": "string" },
          "call": { "type": "string" },
          "expected": { "type": "string" },
          "actual": { "type": "string" }
        }
      }
    }
  }
}"#;

/// Everything the Judge is shown. **The struct is the list** — if it is not a
/// field here it is not in the prompt, which is how rule 3 stays true after
/// somebody edits [`brief`].
#[derive(Debug, Clone, Copy)]
pub struct Dossier<'a> {
    /// The task as the operator wrote it: the title, then the prompt.
    pub title: &'a str,
    pub prompt: &'a str,
    /// The change, pre-image and post-image together
    /// ([`abcc_vcs::Repo::patch_between`]).
    pub patch: &'a str,
    /// What the host already watched. The rungs, not the headline.
    pub measured: &'a Measured,
}

/// 🚨 **How much of each rung the dossier shows — F531's open question, as a
/// type rather than an argument.**
///
/// The brief withholds the *headline* on purpose — a reviewer shown the decision
/// is a reviewer asked to agree with it — and then shows every rung's `counts`
/// and every character it captured. On the corpora's three **shams**, K's own
/// tempting local fix that repairs the symptom the ticket named and leaves the
/// defect, that output did the headline's job anyway: all three came back
/// *"Defects: None found"* **citing the rungs as the proof**, and on one of them
/// the reviewer turned the acceptance rung's `18 run / 18 passed` into *"the
/// host's acceptance suite confirms all eighteen discrepancies are resolved"*
/// about a tree where **7 of 18 invoices are still wrong**. Eighteen tests and
/// eighteen invoices are not the same quantity — that is F517's defect class in
/// the model's own voice, and it is the one place a report costs trust rather
/// than minutes.
///
/// ⚠ **[`Full`](RungView::Full) is the default and it is what ships.**
/// [`Named`](RungView::Named) exists to be measured against it on that same
/// population before anything changes: [`brief`] still calls `Full`, no
/// production caller passes a view at all, and the switch is reached only by
/// `tests/corpus_review.rs` under `ABCC_JUDGE_RUNGS=named`. Three trees is an
/// observation and not yet a number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RungView {
    /// Every rung with the exit status it ended on, its `counts` where it has
    /// them, and the output it captured. **The shipped view**, and the one the
    /// 121-tree run was measured under: recall 43 of 46 on the wrong trees, 0
    /// confirmed false positives on 59 correct ones — and 0 of 3 on the shams.
    #[default]
    Full,
    /// Which rungs ran, and whether each was measured. **No exit status, no
    /// counts, no captured output** — so `measured` says a rung reached this
    /// tree and says nothing whatever about what it concluded.
    ///
    /// ⚠ **An `Unmeasured` rung keeps its `why` under both views, and the
    /// asymmetry is deliberate.** A reason that no measurement exists cannot be
    /// read as a pass, so it cannot produce F531's failure at all; and
    /// ADR-0009's whole subject is that *I did not measure that* must not arrive
    /// in the same words as *I measured it and it was fine*. Under `Named` the
    /// word `measured` no longer claims *fine*, so the two are still different
    /// sentences — which is the property the ruling has to preserve.
    Named,
}

/// One defect, with the thing that shows it.
///
/// 🚨 **The last three fields are the finding.** A defect sentence on its own is
/// an opinion and the operator has their own; `call → expected → actual` is
/// something a person can run, and running it is what turns 3 of 23 into
/// **10 of 23** out of the same model call (ADR-0008).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Where in the diff — a path, and a line or a symbol.
    pub at: String,
    /// What is wrong with it.
    pub defect: String,
    /// The command or the input that shows it.
    pub call: String,
    pub expected: String,
    pub actual: String,
}

/// One review, parsed. **It is what the model said**, and nothing in this crate
/// turns it into a measurement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub assessment: String,
    pub findings: Vec<Finding>,
}

/// Parse a review out of the phase's artifact.
///
/// # Errors
///
/// The payload is not the shape [`REVIEW`] promised. ⚠ Under constrained
/// decoding this should be unreachable and it is still handled, because *the
/// server honoured the grammar* is a claim about somebody else's process:
/// [`read`] keeps the text either way, and the claim is what the model said
/// rather than what we could make of it.
pub fn parse(text: &str) -> Result<Review, serde_json::Error> {
    serde_json::from_str(text)
}

/// The artifact, as the claim an operator reads.
///
/// 🚨 **This is a rendering and never a verdict** — the same rule
/// `abcc_core::outcome::evidence` is written under. It counts findings because
/// an operator wants to know how many there are; it does not weigh them, score
/// them, or decide anything from the count, and the raw payload is already on
/// the log as `ClaimRecorded` for anybody who wants to check this against it.
#[must_use]
pub fn read(text: &str) -> Claim {
    let body = match parse(text) {
        Ok(review) => render(&review),
        // ⚠ Not thrown away and not called an error about the work. The model
        // said something; we could not read it; both halves go to the operator.
        Err(e) => format!(
            "The review did not parse as the shape it was asked for ({e}). What came back, \
             verbatim:\n\n{text}"
        ),
    };
    Claim {
        by: BY.to_owned(),
        text: body,
    }
}

fn render(review: &Review) -> String {
    let mut s = String::with_capacity(512);
    match review.findings.len() {
        0 => s.push_str("Reviewed the change and reported no findings.\n\n"),
        1 => s.push_str("Reviewed the change and reported 1 finding.\n\n"),
        n => {
            let _ = writeln!(s, "Reviewed the change and reported {n} findings.\n");
        }
    }
    s.push_str(review.assessment.trim());
    for (i, f) in review.findings.iter().enumerate() {
        let _ = write!(
            s,
            "\n\n{}. {} — {}\n     run:      {}\n     expected: {}\n     actual:   {}",
            i + 1,
            f.at.trim(),
            f.defect.trim(),
            f.call.trim(),
            f.expected.trim(),
            f.actual.trim()
        );
    }
    s.push('\n');
    s
}

/// What the one call is asked, on top of a head that cannot vary.
///
/// 🚨 **None of the charter is repeated here.** `Head::Commandos`' prefix
/// already carries the role, the no-tools sentence, the untrusted-content rule
/// and the honesty rule, and it is `&'static str` because one token changed at
/// the front of the prompt annihilates the 79.7% prefix-cache saving (F81).
/// Repeating any of it would pay for the same tokens twice and put two versions
/// of one instruction in front of the model.
///
/// ⚠ The diff is fenced. The head's untrusted-content paragraph is what makes
/// that safe to rely on rather than the fence itself — measured at 39 of 50 for
/// the best-written wrapper in the family (W7), which is why the real control is
/// that this role has no tools at all.
///
/// 🚨 **The measurements carry a sentence saying not to report them back, and
/// that sentence is F521**: on the first live review of a refused tree the
/// champion spent finding 1 of 2 restating the standard rung it had just been
/// shown — `run: cargo clippy`, `expected: exit 0`, `actual: exit 101` — which
/// is report volume with no information in it, and report volume on wrong trees
/// is ADR-0008's own falsifier. ⚠ Finding **2** of the same call was the reason
/// the phase exists: `Seq::new(-1).back(5)`, an `as u64` cast that turns a
/// negative into a large positive and walks straight past the saturation the
/// task asked for. No rung saw it, and it arrived with something runnable
/// attached.
#[must_use]
pub fn brief(dossier: &Dossier<'_>) -> String {
    brief_with(dossier, RungView::Full)
}

/// [`brief`], with F531's switch on how much of each rung it is shown.
///
/// 🚨 **Only the rung block moves.** The task, the diff and every sentence of
/// prose are byte-identical across the two views, because a probe that changes
/// two things at once measures neither. ⚠ One sentence is left standing that
/// [`RungView::Named`] makes unanswerable — *where one of them refused and you
/// know why, that belongs in the assessment* — and it is left there on purpose:
/// correcting it is part of the ruling, not part of the measurement.
#[must_use]
pub fn brief_with(dossier: &Dossier<'_>, view: RungView) -> String {
    let Dossier {
        title,
        prompt,
        patch,
        measured,
    } = dossier;
    format!(
        "## The task\n\n{title}\n\n{prompt}\n\n\
         ## The change\n\n\
         This is the whole change, as a diff against the tree it started from. The `-` lines \
         are what was there before it and the `+` lines are what is there now — so both \
         versions are in front of you, and the question is what this change does to that \
         tree rather than how good the result looks on its own.\n\n\
         ```diff\n{patch}\n```\n\n\
         ## What the host already measured\n\n{rungs}\n\n\
         These already ran and the operator has already been shown them, so a finding that \
         repeats one is a line they read twice. Report what these could not see. Where one of \
         them refused and you know why, that belongs in the assessment.\n\n\
         ## What is wanted from you now\n\n\
         Say what this change does to the tree it started from, in at most three sentences. \
         Then report the defects you actually found, most serious first, and at most five.\n\n\
         Every defect carries three things beyond where it is and what is wrong: `call`, the \
         command or the input that shows it; `expected`, what that should produce; and \
         `actual`, what it produces now. A defect you cannot fill those in for is one nobody \
         can reproduce, and the operator has their own opinions already — leave it out and \
         say so in the assessment instead.\n\n\
         If you found nothing, the findings list is empty. That is a usable answer and it \
         costs you nothing: you are not blocking this change and nothing you say here \
         accepts or refuses it. The host has already run the checks above and recorded what \
         it watched; a person reads what you write next to them.\n",
        rungs = rungs(&measured.report, view)
    )
}

/// The measurements, as the reviewer sees them.
///
/// 🚨 **Every rung, including the ones that produced nothing** — that is
/// ADR-0009's whole subject, and it is the half a reviewer is most likely to be
/// missing: *the suite is red* and *there was no suite* are different facts
/// about a change, and only one of them is about the change.
fn rungs(report: &Report, view: RungView) -> String {
    if report.outcomes().is_empty() {
        return "Nothing. No rung ran against this tree.".to_owned();
    }
    let mut s = String::with_capacity(256);
    for outcome in report.outcomes() {
        match outcome {
            Outcome::Measured(m) => match view {
                RungView::Full => {
                    let _ = write!(s, "- {} — exit {}", m.rung, m.exit);
                    if let Some(c) = m.counts {
                        let _ = write!(
                            s,
                            ", {} run / {} passed / {} failed",
                            c.run, c.passed, c.failed
                        );
                    }
                    let _ = writeln!(s, "\n{}", indent(&m.detail));
                }
                // 🚨 F531: a rung that ran, and nothing about what it concluded.
                RungView::Named => {
                    let _ = writeln!(s, "- {} — measured", m.rung);
                }
            },
            // ⚠ Named as an absence rather than folded in beside the failures.
            // The family's vocabulary for "I did not measure that" is the same
            // word as "I measured it and it was fine", and that is the design
            // failure `abcc_core::outcome` removes; handing it back to the model
            // in prose would put it straight back.
            Outcome::Unmeasured { rung, why } => {
                let _ = writeln!(s, "- {rung} — no measurement: {why}");
            }
        }
    }
    s.trim_end().to_owned()
}

fn indent(detail: &str) -> String {
    detail
        .lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}
