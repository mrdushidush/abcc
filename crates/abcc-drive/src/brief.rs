//! What each phase is told, on top of a head that cannot vary.
//!
//! 🚨 **None of this is in the head.** [`abcc_engine::Head::prefix`] takes no
//! arguments and returns `&'static str`, because one token changed at the front
//! of the prompt costs the whole 79.7% prefix-cache saving (F81) — so the task,
//! the repository and everything else that varies arrives as the *body*, which
//! has no operation but `append`.
//!
//! The briefs are short on purpose. The charter, the tool list, the untrusted-
//! content rule and the honesty rule are all already in the head that is about to
//! be sent; repeating any of them here would be paying for the same tokens twice
//! and, worse, would put two versions of one instruction in front of the model.

use abcc_core::outcome::Why;
use abcc_store::TaskRow;

/// What Recon is asked, opening the Localize phase.
#[must_use]
pub fn localize(row: &TaskRow) -> String {
    format!(
        "## The task\n\n{}\n\n{}\n\n## What is wanted from you now\n\n\
         Find the place. Name the files and the specific lines the change belongs in, \
         and say what is already there. You are read-only: nothing you do can change \
         the tree, so look as widely as you need to. When you know where the work \
         goes, say so and stop asking for tools.\n",
        row.title, row.prompt
    )
}

/// What Builders is asked, opening the Change phase.
///
/// ⚠ Recon's answer arrives as *a claim about the tree*, and it is labelled as
/// one. ADR-0009's whole finding is that a truthful sentence cannot carry scope
/// or time: on 98 unimpeded attempts a test really ran 81 of 81 times and 14 of
/// those green, measured, true claims sat on trees the gate fails.
#[must_use]
pub fn change(row: &TaskRow, found: &str) -> String {
    format!(
        "## The task\n\n{}\n\n{}\n\n## What Recon reported\n\n{found}\n\n\
         ## What is wanted from you now\n\n\
         Make the change. Recon's report is a claim about the tree rather than a \
         measurement of it, so check the part you are about to rely on before you \
         rely on it. When the change is made, say what you changed and where, and \
         stop asking for tools.\n",
        row.title, row.prompt
    )
}

/// The question the operator is left with when the ladder could not see enough
/// to say either way.
///
/// 🚨 **It names what was missing, one rung at a time.** `Green` is a claim about
/// coverage, so a rung that could not run does not fail the attempt and does not
/// quietly disappear — and the difference between *the suite is red* and *there
/// was no suite* is the whole of ADR-0009. v1 cannot tell them apart at all: its
/// dispatcher is gated on the word `passed`, which an all-red run does not
/// contain, so eight real run-endings become three records.
///
/// ⚠ An empty `missing` is the case where no rung was asked at all. It is a
/// different sentence, and it is chosen here rather than at the call site because
/// the operator reads one paragraph either way.
#[must_use]
pub fn unverified(artifact: &str, missing: &[(String, Why)]) -> String {
    if missing.is_empty() {
        return format!(
            "Builders finished and no rung was asked about it. The change is \
             snapshotted at {artifact}, so the work is unverified rather than \
             accepted. Read it and say what should happen to the task."
        );
    }
    let listed = missing
        .iter()
        .map(|(rung, why)| format!("  {rung}: {why}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Builders finished and the gate could not measure all of it. The change is \
         snapshotted at {artifact}. What was missing:\n\n{listed}\n\nNothing here \
         says the work is wrong and nothing says it is right. Read it and say what \
         should happen to the task."
    )
}

/// The question the operator is left with when a deterministic rung refused.
///
/// 🚨 **A refusal an operator cannot read is a refusal nobody can fix** (F505).
/// So it names the rung, quotes the evidence the host actually watched, and names
/// the snapshot that evidence is about — because a measurement is stamped with
/// the sha it was taken at, and an operator reading a different tree is the
/// failure mode ADR-0009 §6 exists for.
///
/// ⚠ It says *cannot land* rather than *is wrong*, and the distinction is real
/// rather than diplomatic: on F512's runs 10 and 23 the champion's work compiles,
/// prints `abcc 0.1.0` and passes every test, and it is refused for a function
/// **one line** over this repository's own limit. The rung is right and the work
/// is good, and only a person gets to decide what that combination means.
#[must_use]
pub fn refused(rung: &str, detail: &str, artifact: &str) -> String {
    format!(
        "The {rung} rung refused this change, so it cannot land unattended. The \
         change is snapshotted at {artifact}, and this is what the check said:\n\n\
         {detail}\n\nAnother attempt could fix it, you could fix it by hand, or you \
         could take responsibility for it as it is. Say which."
    )
}

/// The question the operator is left with when the conversation filled the
/// server's context window.
///
/// 🚨 It names the window **and the number the operator has to beat**, because
/// the one action that fixes this is loading a larger one and the operator
/// should not have to work out how much larger. The window is measured rather
/// than guessed (F498) — see [`abcc_core::Why::ContextOverflow`].
#[must_use]
pub fn overflowed(window: u32, prompt_tokens: u32) -> String {
    format!(
        "The conversation filled the server's {window}-token window — {prompt_tokens} of it \
         was prompt, and the reply was cut off part-way. Nothing here failed and nothing \
         measured the work: the same attempt runs unchanged against a server holding a \
         larger window. Load one and retry, or say what should happen to the task."
    )
}

/// The question the operator is left with when the fleet has spent its budget on
/// a task and the last attempt still produced an absence.
///
/// 🚨 **The two attempts are named, and so is the fact that they are the whole
/// budget** (ADR-0010, F373: the second attempt buys 6.4–21.1 points and the
/// third buys 0.7–6.7, so the third is a per-population decision rather than a
/// default). An operator who is not told a budget ran out reads this as the
/// system giving up arbitrarily, which is the reading F393 says v1 earns.
///
/// ⚠ It does not repeat the failure — `why` is a sentence about something that
/// did not happen, and the attempts it names carry theirs on the log.
#[must_use]
pub fn exhausted(spent: u32, why: &Why) -> String {
    format!(
        "This task has had {spent} attempts, which is the whole budget, and the \
         last one ended without producing anything to measure: {why}.\n\n\
         A third attempt is worth buying only where the outcome history says this \
         population is not bimodal, and that is your call rather than the fleet's. \
         You could say go again, take the work over, or stop here. Say which."
    )
}
