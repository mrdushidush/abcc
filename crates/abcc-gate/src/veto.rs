//! The veto: deterministic, non-scoring, boolean — and honest about how few
//! rules it has.
//!
//! ADR-0008 lists four things a veto refuses on: **security HIGH, empty diff,
//! build break, a required measurement that came back `Uncertain`.** Three of
//! those are not here, and each absence is a decision rather than an omission:
//!
//! * **empty diff** is the structural rung's, which already owns it. A second
//!   copy of a rule is a second thing that can drift from the first, and this
//!   project has spent a milestone finding out what that costs.
//! * **a required measurement that came back `Uncertain`** is already
//!   [`Headline::Unverified`], by `Green`'s coverage promise — and `Unverified`
//!   is the *better* sentence, because it lists exactly what was missing. Vetoing
//!   on it would collapse *we could not tell* into *it is broken*, which is the
//!   one distinction ADR-0009 exists to protect. ⚠ Note that it is already not a
//!   pass: `Headline::is_pass` is `Green` and nothing else.
//! * **security HIGH** has no scanner. It arrives with the Posture milestone.
//!   Declaring the rule now would be a name standing in for a specification, and
//!   a veto that cannot fire is a veto an operator will trust for the wrong
//!   reason.
//!
//! So the veto has **one rule**, and it is the one the evidence asked for.

use abcc_core::outcome::{Measurement, Outcome, Report, Why};

use crate::rung::Rung;

/// 🚨 **The one rule: a tree whose checker could not get as far as running is
/// broken, not unmeasured.**
///
/// The acceptance rung answers `Unmeasured { FailedBeforeRunning }` when the
/// runner never reached a test — for cargo that is a `test result:` line that
/// never appeared, which on a Rust tree means the test target did not compile.
/// That is a determinate fact about the work, and leaving it as an absence would
/// let *the tree does not build* arrive on the console reading *nothing measured
/// it*.
///
/// **The evidence is F516, from this repository:** raising the completion budget
/// to 16,384 bought three tree-changing runs out of five, and **two of those
/// three (runs 22 and 24) left trees that do not compile**, because both ended
/// mid-edit. At the smaller budget every tree-changing run left a coherent tree,
/// so this failure mode arrived with the fix for a different one. It is the
/// reason *tree changed* must never be read as *work done* — and this rung is
/// where that sentence is enforced rather than remembered.
///
/// ⚠ **The acceptance rung's own outcome is not rewritten.** It stays
/// `Unmeasured { FailedBeforeRunning }` on the log, exactly as it was measured;
/// the veto is a *second* rung with a *second* outcome. Nothing is collapsed —
/// something is added, and a reader can see both.
pub(crate) fn measure(sha: &str, so_far: &Report) -> Outcome {
    let rung = Rung::Veto.name().to_owned();
    let broken = so_far.outcomes().iter().find_map(|outcome| match outcome {
        Outcome::Unmeasured {
            rung,
            why: Why::FailedBeforeRunning { detail },
        } if rung == Rung::Acceptance.name() => Some(detail.clone()),
        _ => None,
    });
    match broken {
        Some(detail) => Outcome::Measured(Measurement {
            rung,
            sha: sha.to_owned(),
            exit: 1,
            counts: None,
            detail: format!("the tree does not build — {detail}"),
        }),
        None => Outcome::Measured(Measurement {
            rung,
            sha: sha.to_owned(),
            exit: 0,
            counts: None,
            detail: "nothing vetoed".to_owned(),
        }),
    }
}
