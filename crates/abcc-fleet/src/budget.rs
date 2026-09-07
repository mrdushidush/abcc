//! The retry budget: one number, one place, and a count read from the log.
//!
//! ADR-0010 §2 says the budget must be **one counter**, because the donor defect
//! it was written against (F392) is only possible where two mechanisms share one
//! integer: v1's retry budget and its escalation ladder read the same field, so
//! raising a per-phase budget by one silently deleted the top tier — and then
//! labelled the outcome with the phase that never ran. Here the number is
//! [`ATTEMPTS`], it is defined once, and nothing else in the workspace holds a
//! copy. What the driver is told is a *derived* fact — whether an attempt is in
//! hand — and never the count.
//!
//! # What "budget 2" means, settled 2026-08-30
//!
//! 🚨 **Two attempts in total, of which at most one is a retry.** The name reads
//! two ways against the shipped types — [`Cause::spends_retry_budget`] counts only
//! [`Cause::Retry`], so a literal reading gives two retries and three attempts —
//! and the evidence settles it. F373's table is `pass@k` where *k is total
//! attempts*:
//!
//! | | pass@1 | @2 | @3 |
//! |---|---|---|---|
//! | Q56 | 88.2% | **94.6%** (+6.4) | 96.8% (+2.1) |
//! | U100 | 88.8% | **99.3%** (+10.5) | 100.0% (+0.7) |
//! | K champion | 66.7% | **86.7%** (+20.0) | 93.3% (+6.7) |
//!
//! The second attempt buys 6.4–21.1 points and the third buys 0.7–6.7, which is
//! why ADR-0010 §3's heading is *"Retry budget 2, and **the third attempt is a
//! per-population decision**"* — a sentence that only parses if budget 2 is
//! attempts 1 and 2. ADR-0010 §2's *"`Attempt` twice, then `HandToOperator`"* is
//! then the scheduler dispatching `Attempt` for both.
//!
//! # A line of enquiry, not a task's whole history
//!
//! The budget is spent on **one question asked repeatedly**, so the chain resets
//! when the operator changes the question. That is [`Cause`]'s own distinction and
//! not a new one: *"a rescope or an edit is the operator changing the question, so
//! it does not [count] — v1 folded all four into one counter and then could not
//! explain the number."*

use abcc_core::attempt::Cause;

/// How many attempts one line of enquiry may have. **The only copy of this
/// number.** ADR-0010, F373, ratified by David 2026-08-30.
pub const ATTEMPTS: u32 = 2;

/// How many attempts the current line of enquiry has already had, given a task's
/// attempt causes **in the order the log wrote them**.
///
/// Zero for a task that has never run. The walk is forward rather than backward
/// so that it is one pass and reads like the log does.
#[must_use]
pub fn spent(causes: &[Cause]) -> u32 {
    let mut chain: u32 = 0;
    for cause in causes {
        match cause {
            // A new question. Everything before it was about something else.
            Cause::Fresh | Cause::Edit { .. } | Cause::Rescope { .. } => chain = 1,
            Cause::Retry { .. } => chain = chain.saturating_add(1),
            // ⚠ "Re-run for the console's after-action view. **Produces no new
            // work.**" A replay that spent budget would let looking at a task
            // stop it from being worked on.
            Cause::Replay { .. } => {}
        }
    }
    chain
}

/// How long the line of enquiry would be **with this attempt on it** — the causes
/// the log already holds, plus the one about to be dispatched.
///
/// 🚨 **F646: this exists because the dispatched cause is no longer always a
/// `Retry`.** A redirect dispatches [`Cause::Edit`], which *resets* the chain
/// rather than extending it — the operator changed the question — so a budget
/// asked before the cause is known would refuse the very attempt the redirect
/// just bought. Asking [`spent`] about the causes *before* the dispatch and then
/// adding one is only correct while every dispatch is a retry, which stopped
/// being true when `Holding` grew a way back onto a slot.
#[must_use]
pub fn spent_with(causes: &[Cause], next: &Cause) -> u32 {
    let mut chain = Vec::with_capacity(causes.len() + 1);
    chain.extend_from_slice(causes);
    chain.push(next.clone());
    spent(&chain)
}

/// Whether the fleet may dispatch **this** attempt on this line of enquiry.
///
/// 🚨 **There is deliberately no cause-blind version of this.** There was one —
/// `available(&causes)`, asked before the dispatch was known — and F646 is the
/// session where it silently became wrong: two of the three causes a sortie can
/// now dispatch reset the chain rather than extending it, so the cause-blind
/// answer refuses the very attempt a redirect just bought. Two ways to ask one
/// budget, one of them wrong for two thirds of its callers, is F392's donor
/// defect in miniature — so the old pair was removed rather than kept beside
/// these.
#[must_use]
pub fn admits(causes: &[Cause], next: &Cause) -> bool {
    spent_with(causes, next) <= ATTEMPTS
}

/// Whether dispatching **this** attempt would still leave one in hand — which is
/// what [`Driver::retry_available`](abcc_drive::Driver::retry_available) is
/// asking, and it is asked *before* the attempt starts.
///
/// 🚨 The off-by-one is here, once, deliberately, and it is carried by
/// [`spent_with`] rather than by a `+ 1`: `spent` counts what the log already
/// holds, and adding one to it is only the right answer when the dispatch
/// extends the chain.
#[must_use]
pub fn in_hand_beside(causes: &[Cause], next: &Cause) -> bool {
    spent_with(causes, next) < ATTEMPTS
}

#[cfg(test)]
mod tests {
    use super::*;
    use abcc_core::seq::{AttemptId, Seq};

    fn id(n: i64) -> AttemptId {
        AttemptId::at(Seq::new(n))
    }

    #[test]
    fn a_task_that_never_ran_has_spent_nothing_and_has_one_in_hand() {
        assert_eq!(spent(&[]), 0);
        assert!(admits(&[], &Cause::Fresh));
        assert!(
            in_hand_beside(&[], &Cause::Fresh),
            "a fresh dispatch has no retry behind it"
        );
    }

    /// 🚨 The whole ruling in one test: two attempts, and the second is the last.
    #[test]
    fn the_budget_is_two_attempts_and_the_retry_is_the_second() {
        let fresh = vec![Cause::Fresh];
        assert_eq!(spent(&fresh), 1);
        let retry = Cause::Retry { of: id(1) };
        assert!(admits(&fresh, &retry), "the retry was refused");
        assert!(
            !in_hand_beside(&fresh, &retry),
            "the retry about to be dispatched thinks it has another behind it"
        );

        let retried = vec![Cause::Fresh, retry];
        assert_eq!(spent(&retried), 2);
        assert!(
            !admits(&retried, &Cause::Retry { of: id(2) }),
            "a third attempt was allowed"
        );
    }

    /// The operator changing the question starts a new line of enquiry, which is
    /// `Cause`'s own distinction rather than a new one.
    #[test]
    fn an_edit_or_a_rescope_resets_the_chain() {
        for changed in [Cause::Edit { of: id(1) }, Cause::Rescope { of: id(1) }] {
            // 🚨 F646: asked **beside** the new question, which is the only way
            // to see the reset — asked before it, this is an exhausted chain.
            let spent_chain = vec![Cause::Fresh, Cause::Retry { of: id(1) }];
            assert!(
                admits(&spent_chain, &changed),
                "the operator's new question was refused"
            );
            let causes = vec![Cause::Fresh, Cause::Retry { of: id(1) }, changed];
            assert_eq!(spent(&causes), 1);
        }
    }

    /// ⚠ Looking at a task must not stop it being worked on.
    #[test]
    fn a_replay_spends_nothing() {
        let causes = vec![Cause::Fresh, Cause::Replay { of: id(1) }];
        assert_eq!(spent(&causes), 1);
        assert!(admits(&causes, &Cause::Retry { of: id(1) }));
    }

    /// The chain counts consecutive retries, so a long tail cannot wrap or drift
    /// below the budget.
    #[test]
    fn a_chain_longer_than_the_budget_stays_spent() {
        let mut causes = vec![Cause::Fresh];
        for n in 1..10 {
            causes.push(Cause::Retry { of: id(n) });
        }
        assert_eq!(spent(&causes), 10);
        assert!(!admits(&causes, &Cause::Retry { of: id(10) }));
    }
}
