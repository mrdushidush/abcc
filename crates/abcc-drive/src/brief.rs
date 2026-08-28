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

/// The question the operator is left with when the attempt produced an artifact
/// and nothing measured it.
///
/// 🚨 This sentence is the Skeleton milestone's own gap, written where an
/// operator reads it. There is no gate yet — `Measure`, the structural rung, the
/// acceptance test and Veto all arrive at Gate — so the honest ending of a
/// working attempt is *unverified*, never *accomplished*. The day a rung exists
/// this question stops being asked; nothing else about the driver changes.
#[must_use]
pub fn unverified(artifact: &str) -> String {
    format!(
        "Builders finished and nothing measured it. The change is snapshotted at \
         {artifact}; this milestone has no gate, so the work is unverified rather \
         than accepted. Read it and say what should happen to the task."
    )
}
