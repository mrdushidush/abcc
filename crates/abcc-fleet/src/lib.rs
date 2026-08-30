//! The fleet: **admission, one slot, and the receiver for what the driver
//! recommends.**
//!
//! `abcc-drive` runs one attempt and hands back a [`NextAction`] rather than
//! acting on it (its rule 5). This crate is the thing that receives it. It is
//! deliberately small — with one slot the receiver is a **loop, not a
//! scheduler** — and everything hard about it is already decided elsewhere:
//! admission is a projection of the log, the slot count is ADR-0020's, the
//! transition table is ADR-0004's, and the budget is ADR-0010's.
//!
//! # One slot, and why the loop is not a scheduler
//!
//! 🚨 **`--parallel 1`, ratified by David 2026-08-30 (ADR-0020), which supersedes
//! ADR-0003's *count* only.** Two slots were probed against the real workload and
//! bought nothing measurable: within-arm variance on the same two tasks is
//! 2.7–2.8× against a 1.1× between-arm difference, and the two matched pairs
//! disagree in sign (F546). Memory was a wash (F547). The premise the milestone
//! was written on — *two turns in flight push each other past the 90 s idle gap*
//! — was measured and is false: the worst time to first byte of the session,
//! 30.3 s, happened **sequentially**, on a 30k-token prompt (F545).
//!
//! ⏸ **Two slots are deferred, not cancelled.** The slot abstraction stands; only
//! `N` changed. What would reopen it is named in `PLAN.md` and nothing smaller.
//!
//! # What this crate holds, and what it refuses to hold
//!
//! * **The budget, once.** [`budget::ATTEMPTS`] is the only copy of the number
//!   and the driver never sees it — it is told whether an attempt is in hand.
//!   F392 is the donor defect where two mechanisms shared one integer.
//! * **Admission as a projection**, not a queue object. There is no list of
//!   pending work anywhere: [`Fleet::admit`] folds the log and asks what is
//!   `Queued`, which is the same fact the board shows and cannot drift from it.
//! * 🚨 **No third recovery path.** `Deployed`'s contract is already *a slot is
//!   held and no attempt has started*; it is reaped on the spin-up bound and
//!   requeued by boot. F166 is v1 shipping a second recovery path that had never
//!   run when it was needed, so this crate adds none.
//! * 🚨 **The slot's tool ceiling** ([`Fleet::ceiling`]), which is the half of
//!   ADR-0014 §4 that lives here: the ADR puts a ceiling on the *role*, and this
//!   is the *slot's*, with the effective ceiling the narrower of the two. It is
//!   a [`Tier`] and never a list of tool names — W7 proved four times, with four
//!   mechanisms across three authors, that an argument check binds only the tool
//!   that has an argument. **Deny the class.**
//!
//! # What a capped slot does, and the thing it deliberately does not do
//!
//! 🚨 A cap **narrows the head, it does not arm a trap.** `Head::posted` composes
//! the prefix and the wire-level tool array from the effective ceiling, so a
//! `Builders` in a read-only slot is *told* it has read-only tools and never asks
//! for `run_tests`. The alternative — advertise the role's full set and refuse at
//! the call — would spend a round teaching the model something the prompt could
//! have said, on every attempt, forever.
//!
//! ⚠ The refusal is still there underneath, and it is what makes this a control
//! rather than a request: a model that asks for a tool outside its ceiling gets
//! `Why::Denied` in its transcript and the log gets a `ToolCallEnded` with no
//! exit status. What it does **not** get is a dead attempt — a denial is a normal
//! outcome inside a phase, not an ending.
//!
//! # The one thing a sortie stops for
//!
//! [`Landed::next`] is `None` exactly when the ending was the operator's, and a
//! [`ControlPoint`] latches the first stop and keeps it. Both say the same thing,
//! so a sortie ends when it sees the first: what happens after a person stops
//! something is that person's, and continuing to the next task would be the fleet
//! deciding it was not stopped.

pub mod budget;

use abcc_core::attempt::{Cause, NextAction};
use abcc_core::event::Event;
use abcc_core::seq::{AttemptId, TaskId, UnitId};
use abcc_core::task::TaskState;
use abcc_drive::{DriveError, Driver, Landed};
use abcc_engine::control::ControlPoint;
use abcc_engine::provider::Provider;
use abcc_engine::tools::Tier;
use abcc_engine::turn::Limits;
use abcc_engine::workspace::Toolchain;
use abcc_store::{Store, StoreError};
use abcc_vcs::Repo;

use std::path::PathBuf;

/// Anything that stops the fleet before an attempt could be blamed for it.
#[derive(Debug, thiserror::Error)]
pub enum FleetError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Drive(#[from] DriveError),
}

type Result<T> = std::result::Result<T, FleetError>;

/// What the fleet decided to do next, before it does it.
///
/// 🚨 A projection, computed fresh from the log every time and held nowhere. The
/// board and this cannot disagree, because they are the same fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// This task, with this cause. `Fresh` when the task has never run, and
    /// [`Cause::Retry`] naming the attempt it follows when it has.
    Run { task: TaskId, cause: Cause },
    /// Nothing is `Queued`. The board is quiet.
    Quiet,
    /// 🚨 Something is `Queued` whose line of enquiry has already had its
    /// attempts.
    ///
    /// This should be unreachable — a retryable ending with no attempt in hand
    /// lands `AwaitingOrders`, not `Queued` — and it is a variant rather than a
    /// loop or a panic because a fleet that silently re-ran such a task would
    /// spend the GPU forever, and one that panicked would take the log's word for
    /// a state it could describe instead.
    HeldBack { task: TaskId, spent: u32 },
}

/// Where a sortie stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grounded {
    /// Nothing left to admit.
    Quiet,
    /// The operator stopped an attempt, so the fleet stops too.
    Operator,
    /// A task was held back, which is a bug in the landing rather than a state
    /// the board should sit in. The sortie stops rather than looping on it.
    HeldBack { task: TaskId },
}

/// One sortie: every attempt it flew, and why it came down.
#[derive(Debug)]
pub struct Sortie {
    pub flown: Vec<Landed>,
    pub grounded: Grounded,
}

/// One slot, and the loop that feeds it.
pub struct Fleet<'a> {
    store: &'a mut Store,
    repo: &'a Repo,
    provider: &'a dyn Provider,
    model: String,
    worktrees: PathBuf,
    unit: UnitId,
    limits: Limits,
    toolchain: Option<Toolchain>,
    ceiling: Tier,
}

impl<'a> Fleet<'a> {
    /// `worktrees` is handed to the driver unchanged and carries its constraint:
    /// it must be outside `repo`'s working tree.
    #[must_use]
    pub fn new(
        store: &'a mut Store,
        repo: &'a Repo,
        provider: &'a dyn Provider,
        model: impl Into<String>,
        worktrees: impl Into<PathBuf>,
    ) -> Fleet<'a> {
        Fleet {
            store,
            repo,
            provider,
            model: model.into(),
            worktrees: worktrees.into(),
            // ADR-0020: one slot. The id is still carried because a slot is a
            // thing rather than a number, and two slots are deferred.
            unit: UnitId(0),
            limits: Limits::default(),
            toolchain: None,
            // The slot allows whatever a role asks for, so the effective ceiling
            // is the role's own and an operator who sets nothing sees no change.
            ceiling: Tier::Exec,
        }
    }

    #[must_use]
    pub fn limits(mut self, limits: Limits) -> Fleet<'a> {
        self.limits = limits;
        self
    }

    /// Use this toolchain profile instead of detecting one, for every attempt the
    /// sortie flies. ADR-0008 calls a profile operator configuration, so it is set
    /// per fleet and not per task.
    #[must_use]
    pub fn toolchain(mut self, toolchain: Toolchain) -> Fleet<'a> {
        self.toolchain = Some(toolchain);
        self
    }

    /// 🚨 **Cap every head that runs in this slot at `ceiling`.**
    ///
    /// The effective ceiling for a phase is the narrower of its role's and this,
    /// so this can only ever take capability away: setting [`Tier::Exec`] is the
    /// same as setting nothing, and setting [`Tier::NoTools`] leaves a fleet that
    /// can read a repository and change nothing in it.
    ///
    /// It belongs to the fleet and not to a task for the same reason a toolchain
    /// profile does — ADR-0008 calls that operator configuration — and it is one
    /// value rather than one per role because the role already has its own and
    /// two dials on the same quantity is F392's shape.
    #[must_use]
    pub fn ceiling(mut self, ceiling: Tier) -> Fleet<'a> {
        self.ceiling = ceiling;
        self
    }

    /// The ceiling this slot caps its heads at.
    #[must_use]
    pub fn slot_ceiling(&self) -> Tier {
        self.ceiling
    }

    /// What the fleet would run next, folded out of the log.
    ///
    /// # Errors
    ///
    /// [`FleetError::Store`] if the projection will not read.
    pub fn admit(&self) -> Result<Admission> {
        let Some(row) = self
            .store
            .tasks()?
            .into_iter()
            .find(|row| row.state == TaskState::Queued)
        else {
            return Ok(Admission::Quiet);
        };

        let causes = self.causes(row.id)?;
        let spent = budget::spent(&causes);
        if !budget::available(&causes) {
            return Ok(Admission::HeldBack {
                task: row.id,
                spent,
            });
        }

        // The cause is derived, never chosen: a task with attempts behind it is
        // being retried, and one without is fresh. There is no third answer, and
        // `Edit` and `Rescope` reach the log through the operator's verbs rather
        // than through admission.
        let cause = match self.last_attempt(row.id)? {
            Some(of) => Cause::Retry { of },
            None => Cause::Fresh,
        };
        Ok(Admission::Run {
            task: row.id,
            cause,
        })
    }

    /// Fly attempts until the board is quiet or the operator stops one.
    ///
    /// # Errors
    ///
    /// [`FleetError`] if the projection will not read or the driver could not run
    /// an attempt at all.
    pub fn sortie(&mut self, control: &mut ControlPoint) -> Result<Sortie> {
        let mut flown = Vec::new();
        loop {
            let (task, cause) = match self.admit()? {
                Admission::Run { task, cause } => (task, cause),
                Admission::Quiet => {
                    return Ok(Sortie {
                        flown,
                        grounded: Grounded::Quiet,
                    });
                }
                Admission::HeldBack { task, spent } => {
                    // Said out loud on the log rather than only in a return
                    // value, because the log is what a person reads afterwards.
                    self.store.append(Event::Note {
                        text: format!(
                            "{task} is standing by with {spent} attempts already spent, which \
                             the landing should have made impossible. Not admitted."
                        ),
                    })?;
                    return Ok(Sortie {
                        flown,
                        grounded: Grounded::HeldBack { task },
                    });
                }
            };

            // 🚨 Asked before the attempt runs, and it is the only thing the
            // driver is told about the budget. `in_hand_after` holds the
            // off-by-one: the attempt about to be dispatched is not on the log
            // yet.
            let causes = self.causes(task)?;
            let retry_available = budget::in_hand_after(&causes);

            let mut driver = Driver::new(
                self.store,
                self.repo,
                self.provider,
                self.model.clone(),
                self.worktrees.clone(),
            )
            .limits(self.limits)
            .retry_available(retry_available)
            .ceiling(self.ceiling);
            if let Some(toolchain) = &self.toolchain {
                driver = driver.toolchain(*toolchain);
            }
            let landed = driver.run(task, self.unit, cause, control)?;

            // `None` is the operator's ending, and it is the one thing a sortie
            // does not fly past.
            let stopped = landed.next.is_none();
            flown.push(landed);
            if stopped {
                return Ok(Sortie {
                    flown,
                    grounded: Grounded::Operator,
                });
            }
        }
    }

    // -- the projection ----------------------------------------------------

    /// This task's attempt causes, in the order the log wrote them.
    fn causes(&self, task: TaskId) -> Result<Vec<Cause>> {
        Ok(self
            .store
            .task_history(task)?
            .into_iter()
            .filter_map(|logged| match logged.event {
                Event::AttemptStarted { cause, .. } => Some(cause),
                _ => None,
            })
            .collect())
    }

    /// The last attempt this task had, which is the one a retry follows.
    ///
    /// ⚠ The id **is** the `AttemptStarted` event's own `seq`, which is why this
    /// reads the log rather than the projection: the projection holds the task's
    /// state, and a `Queued` task's state names no attempt at all.
    fn last_attempt(&self, task: TaskId) -> Result<Option<AttemptId>> {
        Ok(self
            .store
            .task_history(task)?
            .into_iter()
            .rfind(|logged| matches!(logged.event, Event::AttemptStarted { .. }))
            .map(|logged| AttemptId::at(logged.seq)))
    }
}

/// What the fleet would do with a recommendation, stated so it can be read
/// without running one.
///
/// 🚨 There is no `Escalate`, because there is no second tier to escalate to —
/// ADR-0010, and W4's every item inverting its brief to get there. The only
/// purchase the architecture offers is another attempt, and the only rung above
/// the worker is a person.
#[must_use]
pub fn reading(next: Option<&NextAction>) -> &'static str {
    match next {
        Some(NextAction::Attempt { .. }) => "another attempt, if the budget has one",
        Some(NextAction::Stop) => "nothing more to try",
        Some(NextAction::HandToOperator { .. }) => "a person is owed the question",
        None => "the operator's, and not the fleet's to propose",
    }
}
