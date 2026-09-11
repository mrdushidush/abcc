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

/// What a deterministic rung refused the tree underneath this attempt for.
///
/// 🚨 **The same two strings the operator is shown** — `brief::refused` puts them
/// in the question, this puts them in the brief — and they come from the rung's
/// own `Event::RungRecorded`, so a verdict is not rendered twice from two
/// sources that agree until one is edited.
///
/// ⚠ **Not from `AttemptOutcome::Refused`**, which is what shipped first and
/// fired zero times in nine live attempts: that says how the *conversation*
/// ended, and a tree can be refused by a rung while the attempt over it ends
/// `Uncertain` because the model ran out of rounds. See
/// `Driver::refusal_under`.
pub struct Refusal {
    pub rung: String,
    pub detail: String,
}

/// 🚨 **F700: what already refused this exact tree.**
///
/// The ending that recommends a retry writes the rung and the check's own output
/// into `Event::OperatorPrompted`, and nothing that builds a model body has ever
/// read that event. So the recommended retry opened with a brief byte-identical
/// to the fresh attempt's and re-derived, or failed to re-derive, a verdict the
/// system was already holding. Four attempts have reached the standard rung on
/// the live subject and none has passed it; half of those refusals were rustfmt
/// and every summary of them had said clippy (F673).
///
/// ⚠ **It says *cannot land* and not *is wrong*, which is `brief::refused`'s
/// distinction and is real rather than diplomatic**: on F512's runs the
/// champion's work compiles, prints and passes every test, and is refused for a
/// function one line over this repository's own limit.
///
/// ⚠ **It is only ever built for a model standing on the tree that was refused.**
/// `Driver::refusal_under` reads `Opened::continues`, not the task's last
/// attempt, so this paragraph cannot describe a tree the model is not looking at
/// — which is the failure F702 was.
///
/// An empty string when there is nothing, so the brief has one shape.
fn refused_before(refusal: Option<&Refusal>) -> String {
    refusal.map_or_else(String::new, |Refusal { rung, detail }| {
        format!(
            "\n## A check has already refused this tree\n\n\
             The tree you are looking at was left unfinished by an earlier attempt, \
             and the {rung} rung has already been run over it and refused it. It is \
             one of the checks this repository asks of every change, it runs again \
             on whatever you leave behind, and until it passes the change cannot \
             land however good it is. This is what it said:\n\n{detail}\n\n\
             Fix what it names before anything else. It is a statement about the \
             tree, not about whether the task was understood.\n"
        )
    })
}

/// What Recon is asked, opening the Localize phase.
#[must_use]
pub fn localize(row: &TaskRow, redirect: Option<&str>, refused: Option<&Refusal>) -> String {
    format!(
        "## The task\n\n{}\n\n{}\n{}{}\n## What is wanted from you now\n\n\
         Find the place. Name the files and the specific lines the change belongs in, \
         and say what is already there. You are read-only: nothing you do can change \
         the tree, so look as widely as you need to. When you know where the work \
         goes, say so and stop asking for tools.\n",
        row.title,
        row.prompt,
        redirected(redirect),
        refused_before(refused)
    )
}

/// 🚨 **F646: what the operator said to do instead, if they said anything.**
///
/// An **addendum**, deliberately: the task's own prompt is still above it. A
/// redirect is the operator turning work that already had a definition, and a
/// brief that replaced the task with the redirect would leave the model holding
/// one sentence with nothing behind it.
///
/// ⚠ It also says where the tree is, because that is the half a redirected
/// attempt cannot work out for itself: it opens on the checkpoint the stopped
/// attempt reached rather than on a clean tree, so work already done is present
/// and re-doing it is the failure this paragraph exists to prevent.
///
/// 🚨 **That last paragraph was false for the whole of Skeleton, Gate and Fleet
/// (F702), and the witness that would have caught it is this comment.**
/// `Driver::open_workspace` snapshotted the operator's checkout and never read
/// `Cause`, so every redirected attempt opened on a clean tree while this
/// sentence sat in its context telling it otherwise. **Nothing caught it because
/// the comment and the prose agreed with each other** — two copies of one claim,
/// which is one witness and not two. It is true since F701; what makes it true
/// is `Driver::fork_point`, and if that function stops consulting the cause,
/// this paragraph is a lie again with nothing here to notice. The test that
/// holds the two together is
/// `a_redirected_attempt_opens_on_the_tree_its_brief_promises`, which asserts
/// the tree and this string in one place, on purpose.
///
/// An empty string when there is no redirect, so the brief has one shape.
fn redirected(prompt: Option<&str>) -> String {
    prompt.map_or_else(String::new, |prompt| {
        format!(
            "\n## The operator has redirected this task\n\n{prompt}\n\n\
             This was typed while an earlier attempt was running, and it stopped that \
             attempt. Where it and the task above differ, this is the more recent \
             instruction and it wins. The tree you are looking at is the one that \
             attempt left behind, at its checkpoint - so work it already did is \
             there, and doing it again is not what was asked for.\n"
        )
    })
}

/// What Builders is asked, opening the Change phase.
///
/// ⚠ Recon's answer arrives as *a claim about the tree*, and it is labelled as
/// one. ADR-0009's whole finding is that a truthful sentence cannot carry scope
/// or time: on 98 unimpeded attempts a test really ran 81 of 81 times and 14 of
/// those green, measured, true claims sat on trees the gate fails.
#[must_use]
pub fn change(
    row: &TaskRow,
    found: &str,
    redirect: Option<&str>,
    refused: Option<&Refusal>,
) -> String {
    format!(
        "## The task\n\n{}\n\n{}\n{}{}\n## What Recon reported\n\n{found}\n\n\
         ## What is wanted from you now\n\n\
         Make the change. Recon's report is a claim about the tree rather than a \
         measurement of it, so check the part you are about to rely on before you \
         rely on it. When the change is made, say what you changed and where, and \
         stop asking for tools.\n",
        row.title,
        row.prompt,
        redirected(redirect),
        refused_before(refused)
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
