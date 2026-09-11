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
//! # The two things a sortie stops for
//!
//! [`Landed::next`] is `None` exactly when the ending was the operator's, and a
//! [`ControlPoint`] latches the first stop and keeps it. Both say the same thing,
//! so a sortie ends when it sees the first: what happens after a person stops
//! something is that person's, and continuing to the next task would be the fleet
//! deciding it was not stopped.
//!
//! 🚨 The second is [`StandDown`], and it is **the only verb that belongs to the
//! sortie rather than to a task.** ADR-0012 §4 makes the task the unit of control
//! because every verb ends in *"...and then what happens to its model slot and
//! its workspace lock?"* — and this one ends in *"and then the slot goes empty"*,
//! which no task can answer. It is read before admission and never inside an
//! attempt, so the most it can cost is the wait for one landing, and the work in
//! flight is kept rather than thrown away.
//!
//! ⚠ [`Fleet::in_flight`] is the other half of the same surface and holds no
//! policy at all: it publishes **which task the slot is on, and the channel that
//! reaches it**, so that a console spanning the sortie can check the name the
//! operator typed against the task that would actually receive it. Nothing in
//! this crate reads it back.
//!
//! 🚨 **Every attempt gets its own [`ControlPoint`], and that is why
//! [`Fleet::sortie`] takes none.** A channel that outlives an attempt is a queue
//! the *next* attempt drains: `check` latches the first stop it finds, so a verb
//! that reached the channel a moment after its own attempt stopped reading would
//! be honoured by whatever task the slot picked up next. That is the same
//! mis-delivery the desk exists to prevent, one layer below where any desk can
//! see it, so it is fixed here — the point is created per attempt and dropped
//! with it.

pub mod breaker;
pub mod budget;

use abcc_core::attempt::{Cause, NextAction};
use abcc_core::event::{Control, Event};
use abcc_core::redact::Secrets;
use abcc_core::seq::{AttemptId, Seq, TaskId, UnitId};
use abcc_core::task::TaskState;
use abcc_drive::{DriveError, Driver, Landed};
use abcc_engine::control::{ControlPoint, InFlight};
use abcc_engine::provider::Provider;
use abcc_engine::tools::Tier;
use abcc_engine::turn::Limits;
use abcc_engine::workspace::Toolchain;
use abcc_store::{Store, StoreError};
use abcc_vcs::Repo;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    /// 🚨 **F646: a `Holding` task the operator asked for back.**
    ///
    /// A hold is the operator having stopped this task, so it is never admitted
    /// on its own — [`Fleet::held`] is the fold that says one was asked for, and
    /// what with. `Queued` is scanned first, because `Queued` is the state whose
    /// contract *is* eligible for admission and a held task re-enters behind the
    /// board rather than in front of it.
    Resume { task: TaskId, cause: Cause },
    /// Nothing is `Queued`, and nothing held has been asked for. The board is
    /// quiet.
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

/// 🚨 **F646: what the operator asked for while a task was held.**
///
/// `pause`, `halt` and `redirect` all park a task in `Holding` and until now
/// nothing put it back to work — [`Command::Resume`](abcc_core::task::Command),
/// the lifecycle's only edge out of `Holding`, had no caller in the binary at
/// all. The edge was never missing; **the reader was**. `FleetDesk::request`
/// writes `ControlRequested` to the log before it pokes the channel (ADR-0006's
/// ordering), so the operator's `resume` has been on the log the whole time with
/// nothing folding over it.
///
/// ⚠ And it could never have been a channel verb.
/// [`ControlPoint::check`](abcc_engine::control::ControlPoint::check) drops
/// `Control::Resume` on purpose — *it belongs to a task in `Holding`, which by
/// definition has no worker to receive it.* So the whole of `resume` lives on
/// the log side, which is why this is a fold and not a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Held {
    /// Nobody has asked for it. The hold stands.
    Stays,
    /// `resume <task>`, typed after the hold. The same question again.
    Asked,
    /// The hold came from `redirect <task> <prompt>` — the operator saying what
    /// to do *instead*, which is an answer rather than a stop, so it needs no
    /// second verb to pick it back up.
    ///
    /// ⚠ It carries no prompt. F647: **the words belong to the line of enquiry
    /// and not to the admission that noticed them**, so they are folded out
    /// separately by [`Fleet::redirect_in_force`] — which every admission asks,
    /// including the `Queued` one this variant is not.
    Redirected,
}

/// The operator's stand-down: **fly out what is in flight, then admit nothing
/// more.**
///
/// 🚨 The one control verb whose scope is the sortie. Every other verb is the
/// task's (ADR-0012 §4), and before this the only ways to end a sortie early were
/// to stop the attempt that happened to be running — which throws away a landing
/// nobody objected to — or `Ctrl-C`, which throws away the process.
///
/// ⚠ **It is not a stop and does not pretend to be one.** It is sampled once per
/// loop, before admission, so an operator who also wants the attempt in flight
/// stopped types `halt` as well; the two compose, and each says exactly what it
/// does. Sampling it inside an attempt would make it a third thing that can end
/// one, beside the [`ControlPoint`] and the budget.
#[derive(Debug, Clone, Default)]
pub struct StandDown(Arc<AtomicBool>);

impl StandDown {
    /// Stand the sortie down. Idempotent, and there is no way back: an operator
    /// who changes their mind starts a sortie, which is one command.
    pub fn order(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether the operator has stood this sortie down.
    #[must_use]
    pub fn ordered(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Where a sortie stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grounded {
    /// Nothing left to admit.
    Quiet,
    /// The operator stopped an attempt, so the fleet stops too.
    Operator,
    /// The operator stood the sortie down. Whatever was in flight was flown out
    /// and landed normally; nothing after it was admitted.
    StoodDown,
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
    /// The denylist, handed to every driver this fleet dispatches (ADR-0014 §5).
    /// `Secrets::default()` is the shapes and no literals, so a fleet nobody
    /// configured still scrubs.
    secrets: Secrets,
    in_flight: InFlight,
    stand_down: StandDown,
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
            secrets: Secrets::default(),
            // Both default to a surface nobody is holding: a fleet with no
            // console publishes where the slot is to nothing and is never stood
            // down, which is what `abcc fleet` did before either existed.
            in_flight: InFlight::default(),
            stand_down: StandDown::default(),
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
    /// Give every driver this fleet dispatches the values this process holds.
    #[must_use]
    pub fn secrets(mut self, secrets: Secrets) -> Fleet<'a> {
        self.secrets = secrets;
        self
    }

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

    /// Publish which task holds the slot, for a console that spans the sortie.
    ///
    /// The fleet only ever writes to it. [`InFlight`] is read by the thing taking
    /// the operator's verbs, so that a name typed while one task was flying
    /// cannot be delivered to the next one; a fleet given none publishes to
    /// nothing.
    #[must_use]
    pub fn in_flight(mut self, in_flight: InFlight) -> Fleet<'a> {
        self.in_flight = in_flight;
        self
    }

    /// Watch this flag before every admission.
    ///
    /// ⚠ The flag belongs to the caller, because the thing that raises it is the
    /// console and the thing that reads it is this loop, and a flag owned by
    /// either would have to be handed to the other anyway.
    #[must_use]
    pub fn stand_down(mut self, stand_down: StandDown) -> Fleet<'a> {
        self.stand_down = stand_down;
        self
    }

    /// What the fleet would run next, folded out of the log.
    ///
    /// # Errors
    ///
    /// [`FleetError::Store`] if the projection will not read.
    pub fn admit(&self) -> Result<Admission> {
        let rows = self.store.tasks()?;

        if let Some(row) = rows.iter().find(|row| row.state == TaskState::Queued) {
            // The cause is derived, never chosen: a task with attempts behind it
            // is being retried, and one without is fresh. There is no third
            // answer here — `Edit` and `Rescope` reach the log through the
            // operator's verbs, and `Edit` reaches admission below.
            let cause = match self.last_attempt(row.id)? {
                Some(of) => Cause::Retry { of },
                None => Cause::Fresh,
            };
            return self.affordable(row.id, |task, cause| Admission::Run { task, cause }, cause);
        }

        // 🚨 **F646, and the order is the design.** A held task re-enters behind
        // the board: `Queued` is the state whose contract is *eligible for
        // admission*, and `Holding` is the operator having stopped this one. So
        // nothing here can delay fresh work, and a redirect issued mid-sortie is
        // picked up on the next turn of this loop rather than jumping a queue.
        for row in &rows {
            if !matches!(row.state, TaskState::Holding { .. }) {
                continue;
            }
            let cause = match self.held(row.id, row.since)? {
                Held::Stays => continue,
                // The same question, from the checkpoint — so it spends a retry,
                // which is exactly what the budget is for. ⚠ That makes `pause`
                // cost something, and it should: the fleet is being asked for a
                // second run at one question.
                Held::Asked => match self.last_attempt(row.id)? {
                    Some(of) => Cause::Retry { of },
                    None => Cause::Fresh,
                },
                // 🚨 A different question, so `Cause::Edit` — which resets the
                // chain (`budget::spent`) rather than spending from it. The
                // operator changed what is being asked, and ADR-0010's budget is
                // spent on *one question asked repeatedly*.
                Held::Redirected => match self.last_attempt(row.id)? {
                    Some(of) => Cause::Edit { of },
                    // Unreachable — a task cannot be `Holding` without an attempt
                    // having been stopped — and stated rather than asserted,
                    // because a redirect with nothing to fork from is a fresh
                    // start on the new prompt and not a panic.
                    None => Cause::Fresh,
                },
            };
            return self.affordable(
                row.id,
                |task, cause| Admission::Resume { task, cause },
                cause,
            );
        }

        Ok(Admission::Quiet)
    }

    /// The budget check both admissions share, so there is one place that can
    /// say `HeldBack` and one definition of what a spent line of enquiry is.
    ///
    /// ⚠ The cause is passed in rather than derived here: a redirect's `Edit`
    /// resets the chain and a resume's `Retry` extends it, and which of those it
    /// is has to be settled **before** the budget is asked about it.
    fn affordable(
        &self,
        task: TaskId,
        admit: impl FnOnce(TaskId, Cause) -> Admission,
        cause: Cause,
    ) -> Result<Admission> {
        let causes = self.causes(task)?;
        if !budget::admits(&causes, &cause) {
            return Ok(Admission::HeldBack {
                task,
                spent: budget::spent(&causes),
            });
        }
        Ok(admit(task, cause))
    }

    /// Fly attempts until the board is quiet or the operator stops one.
    ///
    /// ⚠ It takes no [`ControlPoint`]: a sortie makes one per attempt, for the
    /// reason in this module's docs, and publishes each handle through
    /// [`Fleet::in_flight`]. A caller that wants to reach a running attempt reads
    /// it from there, which is also the only way to know which task it would
    /// reach.
    ///
    /// # Errors
    ///
    /// [`FleetError`] if the projection will not read or the driver could not run
    /// an attempt at all.
    pub fn sortie(&mut self) -> Result<Sortie> {
        let mut flown = Vec::new();
        loop {
            // 🚨 Before admission, which is the whole of what a stand-down is: it
            // does not reach into an attempt, so the one that was in flight when
            // the operator typed it has already landed by the time this is read.
            if self.stand_down.ordered() {
                return Ok(Sortie {
                    flown,
                    grounded: Grounded::StoodDown,
                });
            }

            let (task, cause) = match self.admit()? {
                // 🚨 F646: one arm, and that is the finding rather than a tidy-up.
                // The driver picks `Resume` over `Deploy` from the task's own
                // state and `redirect_in_force` is asked of every admitted task,
                // so a resumed attempt and a fresh one are dispatched by exactly
                // the same code. What the two variants carry is *why* the task is
                // flying — which the log wants and the dispatch does not.
                Admission::Run { task, cause } | Admission::Resume { task, cause } => (task, cause),
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
            // driver is told about the budget. `spent_with` holds the
            // off-by-one: the attempt about to be dispatched is not on the log
            // yet.
            // 🚨 F647: asked of every admitted task and not only a resumed one.
            // A redirect is an instruction to the task, so it outlives the one
            // attempt the fold that found it admitted.
            let redirect = self.redirect_in_force(task)?;
            let causes = self.causes(task)?;
            // 🚨 F646: asked **beside the cause**. A redirect's `Cause::Edit`
            // resets the line of enquiry, so a cause-blind answer — which
            // assumes the dispatch extends it — would tell the driver no retry
            // was left on the very attempt the operator had just bought.
            let retry_available = budget::in_hand_beside(&causes, &cause);

            let mut driver = Driver::new(
                self.store,
                self.repo,
                self.provider,
                self.model.clone(),
                self.worktrees.clone(),
            )
            .limits(self.limits)
            .retry_available(retry_available)
            .ceiling(self.ceiling)
            .secrets(self.secrets.clone())
            .redirect(redirect);
            if let Some(toolchain) = &self.toolchain {
                driver = driver.toolchain(*toolchain);
            }
            // 🚨 This attempt's own channel, published before it starts and
            // dropped when it ends. ⚠ The release is **not** behind `?`: a
            // `driver.run` that fails leaves the slot empty just as surely as one
            // that lands, and an `InFlight` still naming a dead task would take a
            // poke and report it as delivered.
            let (mut control, handle) = ControlPoint::new();
            self.in_flight.takes(task, handle);
            let ran = driver.run(task, self.unit, cause, &mut control);
            self.in_flight.released();
            let landed = ran?;

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

    /// 🚨 **F646: whether a held task has been asked for, and what with.**
    ///
    /// Two readings, and the order between them is the whole of it:
    ///
    /// 1. **A `resume` typed after the hold.** `since` is the seq of the
    ///    transition that produced this `Holding`, so a `ControlRequested` newer
    ///    than it belongs to *this* hold. An older one belonged to a hold the
    ///    task has already left, and honouring it would resume a task the
    ///    operator has since stopped again — which is also why this can never
    ///    loop: a task that holds again gets a newer `since` and the request
    ///    falls behind it.
    /// 2. **The hold's own cause.** A redirect names what to do instead, so it
    ///    needs no second verb; the verb that caused the current hold is the last
    ///    one the driver acknowledged with `ControlApplied`, because the
    ///    transition into `Holding` follows it immediately and a later `resume`
    ///    writes `ControlRequested` rather than `ControlApplied`.
    ///
    /// ⚠ The request is read and never consumed. There is no acknowledgement
    /// row, because `since` already moves — an event that had to be marked as
    /// used would be a mutation of the log, which ADR-0004 does not have.
    fn held(&self, task: TaskId, since: Seq) -> Result<Held> {
        let history = self.store.task_history(task)?;

        if history.iter().any(|logged| {
            logged.seq > since
                && matches!(
                    &logged.event,
                    Event::ControlRequested {
                        control: Control::Resume,
                        ..
                    }
                )
        }) {
            return Ok(Held::Asked);
        }

        let caused_by = history.iter().rev().find_map(|logged| match &logged.event {
            Event::ControlApplied { control, .. } => Some(control),
            _ => None,
        });
        Ok(match caused_by {
            Some(Control::Redirect { .. }) => Held::Redirected,
            // `pause`, `halt`, or a hold with no verb behind it at all. All three
            // are the operator having stopped this task, and they wait.
            _ => Held::Stays,
        })
    }

    /// 🚨 **F647: the redirect this task is working under, if any — and it
    /// outlives the attempt that was admitted for it.**
    ///
    /// Public for the same reason [`Fleet::admit`] is: it is a projection over the
    /// log, computed fresh and held nowhere, and a console that wants to show
    /// what a task is working under reads the same fold the driver is handed.
    ///
    /// The first version of this carried the prompt on `Admission::Resume`, which
    /// is where the fold that found it happened to be standing. That is one
    /// attempt long. A redirected attempt that ends in an absence lands `Queued`,
    /// is re-admitted through the `Queued` pass — which has no redirect field and
    /// could not have one without saying that a re-route is a property of an
    /// admission — and the retry runs on the **original** prompt. The operator's
    /// instruction would survive exactly one attempt, and the log would show two
    /// attempts under `Cause::Edit`'s line of enquiry that were asked different
    /// questions.
    ///
    /// So the words belong to the *task*, folded out of the log beside whatever
    /// admitted it. **The most recent `ControlApplied { Redirect }` is the
    /// instruction in force**, and nothing supersedes it but another redirect: a
    /// pause does not withdraw it, a retry does not exhaust it, and `abcc
    /// release` handing a commandeered task back does not either. What ends it is
    /// the task ending.
    /// # Errors
    ///
    /// [`FleetError::Store`] if the log will not read.
    pub fn redirect_in_force(&self, task: TaskId) -> Result<Option<String>> {
        Ok(self
            .store
            .task_history(task)?
            .iter()
            .rev()
            .find_map(|logged| match &logged.event {
                Event::ControlApplied {
                    control: Control::Redirect { prompt },
                    ..
                } => Some(prompt.clone()),
                _ => None,
            }))
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
