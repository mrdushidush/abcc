//! The attempt driver: **Localize → Change**, over the log, the repository and
//! the engine.
//!
//! Four crates each hold one thing and none of them holds a run.
//! [`abcc_core`](../abcc_core/index.html) has the words, `abcc-store` writes them
//! down, `abcc-vcs` gives an attempt somewhere to work, and `abcc-engine` drives
//! one phase against a model. This crate is where those four meet, and it is the
//! last piece of the Skeleton milestone's engine: *one slot running one attempt
//! through Localize → Change on a real repo* (`PLAN.md` §3).
//!
//! It is a separate crate rather than a module of the engine on purpose. The turn
//! loop writes through [`abcc_engine::turn::Journal`] precisely so that it does
//! not depend on a `Store`; putting the composition inside `abcc-engine` would
//! put `rusqlite` behind that trait and make the seam decorative.
//!
//! # The six rules this driver holds
//!
//! 1. 🚨 **Only a measurement says `Accomplished`.** The Gate milestone made
//!    that word reachable and did not make it cheap: the driver runs
//!    [`abcc_gate::Gate`] over the snapshot pair and reads
//!    [`Headline::is_pass`], which is `Green` and nothing else — every declared
//!    rung measured, none of them red. A model's answer still reaches nothing:
//!    what it says is a `Claim`, and there is no function anywhere that turns one
//!    into an `Outcome`. ⚠ When no rung could measure — no toolchain profile in
//!    the tree — the ending is still
//!    [`AttemptOutcome::Uncertain`]`{ why: `[`Why::NoCheckerForArtifact`]` }` and
//!    the operator is still owed the question. 🚨 **And a measurement says it
//!    whether or not the model got as far as claiming to be finished** (the
//!    operator's ruling, 2026-09-10): `Green` under a `PhaseEnded::Unmeasured`
//!    lands `Accomplished` too, because the ladder reads the tree and the
//!    ending reads the conversation. The second limb of that ruling — *and
//!    updated* — needs no code: `Rung::Structural` refuses an unchanged tree
//!    and runs first, so `Green` already means the tree moved.
//! 2. **A failure before `AttemptStarted` leaves the task `Deployed`, and that is
//!    the design rather than a leak.** `Deployed`'s contract *is* "a slot is held
//!    and no attempt has started"; it is reaped on the spin-up bound and
//!    requeued by boot. Inventing a third recovery path here is how v1 ended up
//!    with a second one that had never run when it was needed (F166).
//! 3. **The checkpoint is the durable form of the work; the worktree is
//!    disposable.** Every ending except `Kill` takes a closing snapshot *in the
//!    worktree* before the worktree is removed, and writes it to a ref so
//!    `git gc --prune=now` cannot collect it (ADR-0007, F330).
//! 4. **The operator's verb is acknowledged on the log before anything acts on
//!    it.** `ControlApplied` is written first, then the work is disposed of the
//!    way [`Keep`] says.
//! 5. **What happens next is returned, not done.** [`Landed::next`] is a
//!    [`NextAction`] the fleet has not been built to receive yet — admission,
//!    slots and retry budget 2 are the Fleet milestone — so the driver runs one
//!    attempt and hands back its recommendation rather than acting on it.
//! 6. 🚨 **The Judge reports and the Judge's failure is not the attempt's.**
//!    [`Driver::judge`] runs after the ladder, reads what it measured, and
//!    attaches an [`abcc_core::outcome::Claim`] to the same [`Report`] through
//!    `Report::note` — which touches no `Outcome` and therefore no `Headline`.
//!    So [`ending`] is computed from the phase and the measurements and would be
//!    byte-for-byte the same if the review had never been asked for. A review
//!    that times out, says nothing or comes back malformed costs the operator a
//!    paragraph, never a verdict. See [`abcc_gate::judge`].

use std::fs;
use std::path::PathBuf;

use abcc_core::attempt::{AttemptOutcome, Cause, NextAction};
use abcc_core::event::Event;
use abcc_core::outcome::{Headline, Why};
use abcc_core::redact::Secrets;
use abcc_core::run::AttemptPhase;
use abcc_core::seq::{AttemptId, CheckpointId, PromptId, TaskId, UnitId};
use abcc_core::task::{AbortReason, Command, Refused, RequeueReason, TaskState};
use abcc_engine::control::{ControlPoint, Keep, Watch};
use abcc_engine::head::Head;
use abcc_engine::provider::{Body, Provider, Schema};
use abcc_engine::tools::Tier;
use abcc_engine::turn::{Limits, NoTools, PhaseEnded, PhaseReport, Tools, TurnLoop};
use abcc_engine::workspace::{Toolchain, Workspace};
use abcc_gate::{Gate, Measured, judge};
use abcc_store::{Applied, Store, StoreError, TaskRow};
use abcc_vcs::{Repo, Sha, VcsError, Worktree, checkpoint_ref};

pub mod brief;
pub mod journal;

pub use journal::StoreJournal;

/// Anything that stops the driver from running an attempt at all.
///
/// A *refused command* is in here and a *refused tool call* is not: the second is
/// a normal thing for a model to try and is recorded as [`Why::Denied`] on the
/// log, while the first means the caller asked the driver to run a task that was
/// not theirs to run.
#[derive(Debug, thiserror::Error)]
pub enum DriveError {
    #[error("the log: {0}")]
    Store(#[from] StoreError),
    #[error("git: {0}")]
    Vcs(#[from] VcsError),
    #[error("{what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("no task {0} in the projection")]
    NoSuchTask(TaskId),
    #[error("{command} was refused: {refusal}")]
    Refused {
        command: &'static str,
        refusal: Refused,
    },
}

type Result<T> = std::result::Result<T, DriveError>;

/// Where one attempt got to, and what it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landed {
    pub attempt: AttemptId,
    pub outcome: AttemptOutcome,
    /// The task's state after the attempt ended — read back from the projection
    /// rather than assumed, because the transition function is the authority on
    /// where a command lands (ADR-0004).
    pub state: TaskState,
    pub localize: PhaseReport,
    /// `None` when Localize produced no artifact to hand over, which is the only
    /// way the Change phase does not run.
    pub change: Option<PhaseReport>,
    /// The closing snapshot's sha. `None` only when the operator said `Kill`,
    /// which is the one verb that keeps nothing.
    pub kept: Option<String>,
    /// What the gate measured, when it ran.
    ///
    /// 🚨 `None` is not *the gate passed nothing* — it is *the gate was not
    /// asked*, and there are exactly two ways to get it: the last phase did not
    /// answer, so there is no artifact to measure, or the operator stopped the
    /// attempt. Measuring an attempt that ended in an absence would spend a cold
    /// build (55 s and 2.3 GB, F356) to learn what the ending already said.
    pub gate: Option<Measured>,
    /// 🚨 A **recommendation**, not an act. `None` when the ending was the
    /// operator's, because what happens after an operator stops something is the
    /// operator's and not the fleet's to propose.
    pub next: Option<NextAction>,
    /// How the review went, when it was asked for.
    ///
    /// 🚨 **Nothing in [`Landed`] is computed from this**, and that is the
    /// point: the review is a report. `None` is *not asked* — there was no
    /// measured tree to read, git would not produce the diff, or the diff is
    /// larger than [`judge::MAX_PATCH_CHARS`] — and an ending here that is not
    /// [`PhaseEnded::Answered`] is a review that did not happen, which is a
    /// different thing from a review that found nothing. What it *said* is on
    /// [`Measured::report`]'s claims, because that is where a `Claim` lives.
    pub judge: Option<PhaseEnded>,
}

/// One attempt, from `Queued` to wherever it honestly ends.
pub struct Driver<'a> {
    store: &'a mut Store,
    repo: &'a Repo,
    provider: &'a dyn Provider,
    model: String,
    worktrees: PathBuf,
    limits: Limits,
    /// An operator's chosen toolchain profile, overriding what the tree's
    /// witness files say.
    ///
    /// 🚨 **One profile per attempt, used by both the tool layer and the gate.**
    /// ADR-0008 calls a profile operator configuration, and two copies of it —
    /// one for the `run_tests` the model calls and one for the acceptance rung —
    /// would be two things that can disagree about what this repository's tests
    /// are. `None` is *detect it*, which is the normal case.
    toolchain: Option<Toolchain>,
    /// 🚨 **Whether the fleet still has an attempt in hand for this task,
    /// which is NOT the retry budget** — the budget is one number and it lives in
    /// one place, `abcc-fleet` (ADR-0010 §2; F392 is the donor defect where two
    /// mechanisms shared one integer and raising a per-phase budget by one
    /// silently deleted the top tier).
    ///
    /// The driver is told a derived fact rather than a count because **the
    /// landing has to be a state the recommendation can be acted on from**, and a
    /// task can only be handed to a person from inside the attempt that ran:
    /// `RequestOrders` is an edge out of `Engaged`, and by the time a fleet reads
    /// `Landed::next` the attempt is over. So a retryable ending with an attempt
    /// in hand lands `Queued` for the fleet to re-admit, and the same ending
    /// without one lands `AwaitingOrders` with a question that says the budget is
    /// spent (F548).
    ///
    /// Default `false`, which is what makes a bare `abcc run` honest: one attempt
    /// was bought, it produced an absence, and a person is asked.
    retry_available: bool,
    /// 🚨 **The slot's tool ceiling, which caps every head this attempt runs.**
    ///
    /// ADR-0014 §4 puts a ceiling on the role; this is the slot's half, and the
    /// effective ceiling is the narrower of the two. It reaches the turn loop and
    /// nothing else — there is no second copy for the gate, because the gate's
    /// rungs are the host's own checkers and are not something a model asked for.
    ///
    /// Default [`Tier::Exec`]: the slot allows whatever the role asks for, so an
    /// unset ceiling changes nothing.
    ceiling: Tier,
    /// 🚨 **The literals this process holds, on their way to the redactor**
    /// (ADR-0014 §5).
    ///
    /// The driver is the first place that has both the credential and the loop
    /// that writes the log, so it is where the exact half of the denylist is
    /// assembled. The shape half needs nothing and is on by default — a redactor
    /// configured *off* by an omitted builder call is the shape of control that
    /// protects only the paths somebody remembered.
    secrets: Secrets,
    /// 🚨 **F646: the prompt a `redirect` named, carried into both briefs.**
    ///
    /// The operator stopped an attempt and said what to do instead. `Cause::Edit`
    /// records *that* they changed the question and names the attempt it forked
    /// from; it does not carry the words, because an attempt's cause is a fact
    /// about the fork and the prompt is a fact about the log. So the fleet folds
    /// it out of the `ControlApplied` the landing already wrote and hands it here.
    ///
    /// ⚠ It is an **addendum and not a replacement**. The task's own prompt still
    /// opens both briefs: a redirect that silently dropped it would leave the
    /// model working on a sentence with no task behind it, and the operator wrote
    /// the redirect expecting the task to still be the task.
    redirect: Option<String>,
}

impl<'a> Driver<'a> {
    /// `worktrees` is the directory attempt worktrees are made under, and it must
    /// be outside `repo`'s working tree — git refuses to nest one otherwise, and
    /// a worktree inside the tree it snapshots would be captured by the next
    /// `git add -A`.
    #[must_use]
    pub fn new(
        store: &'a mut Store,
        repo: &'a Repo,
        provider: &'a dyn Provider,
        model: impl Into<String>,
        worktrees: impl Into<PathBuf>,
    ) -> Driver<'a> {
        Driver {
            store,
            repo,
            provider,
            model: model.into(),
            worktrees: worktrees.into(),
            limits: Limits::default(),
            toolchain: None,
            retry_available: false,
            ceiling: Tier::Exec,
            secrets: Secrets::default(),
            redirect: None,
        }
    }

    #[must_use]
    pub fn limits(mut self, limits: Limits) -> Driver<'a> {
        self.limits = limits;
        self
    }

    /// Use this toolchain profile instead of detecting one. See
    /// [`Driver::toolchain`](Driver#structfield.toolchain) — it reaches the tool
    /// layer and the gate together, on purpose.
    #[must_use]
    pub fn toolchain(mut self, toolchain: Toolchain) -> Driver<'a> {
        self.toolchain = Some(toolchain);
        self
    }

    /// Tell the driver whether the fleet has another attempt in hand for this
    /// task. See [`Driver::retry_available`](Driver#structfield.retry_available)
    /// — it decides the landing of a retryable ending and nothing else.
    #[must_use]
    pub fn retry_available(mut self, yes: bool) -> Driver<'a> {
        self.retry_available = yes;
        self
    }

    /// Give the redactor the values this process holds. See
    /// [`Driver::secrets`](Driver#structfield.secrets).
    #[must_use]
    pub fn secrets(mut self, secrets: Secrets) -> Driver<'a> {
        self.secrets = secrets;
        self
    }

    /// Cap every head this attempt runs at `ceiling`. See
    /// [`Driver::ceiling`](Driver#structfield.ceiling).
    #[must_use]
    pub fn ceiling(mut self, ceiling: Tier) -> Driver<'a> {
        self.ceiling = ceiling;
        self
    }

    /// Carry a redirect's prompt into this attempt's briefs. See
    /// [`Driver::redirect`](Driver#structfield.redirect).
    #[must_use]
    pub fn redirect(mut self, prompt: Option<String>) -> Driver<'a> {
        self.redirect = prompt;
        self
    }

    /// Run one attempt on `task` in `unit`, and land the task where its ending
    /// implies.
    ///
    /// The sequence is checkpoint → worktree → `AttemptStarted` → `Engage`, in
    /// that order, because everything before `AttemptStarted` can fail with
    /// nothing to tombstone: rule 2 in the module docs.
    ///
    /// # Errors
    ///
    /// [`DriveError`] — the log would not take a write, git would not give us a
    /// worktree, or a lifecycle command was refused.
    pub fn run(
        &mut self,
        task: TaskId,
        unit: UnitId,
        cause: Cause,
        control: &mut ControlPoint,
    ) -> Result<Landed> {
        let row = self.row(task)?;
        // 🚨 **F646: the way onto a slot depends on where the task was.**
        // `Deploy` is `Queued -> Deployed` and `Resume` is `Holding -> Deployed`;
        // they land on the same state, and this chooses which to *ask* rather
        // than whether it is allowed — `TaskState::apply` stays the one authority
        // on legality, which is F629's rule about a second `match` that can drift.
        let onto_slot = match row.state {
            TaskState::Holding { .. } => Command::Resume { unit },
            _ => Command::Deploy { unit },
        };
        self.command(task, onto_slot)?;

        let opened = self.open_workspace(&row, control.watch())?;

        let started = self.store.append(Event::AttemptStarted {
            task,
            unit,
            cause,
            checkpoint_from: Some(opened.opening.id),
        })?;
        let attempt = AttemptId::at(started.seq);
        self.command(task, Command::Engage { attempt })?;

        // Two phases, two heads, two bodies. The body cannot be shared across
        // them: a phase is a different frozen prefix, so continuing one body into
        // the other would be a cold prefill wearing a warm one's clothes.
        let mut recon = Body::opening(brief::localize(&row, self.redirect.as_deref()));
        let localize = self.phase(
            Call {
                attempt,
                phase: AttemptPhase::Localize,
                head: Head::Recon,
                tools: &opened.workspace,
                schema: None,
            },
            &mut recon,
            control,
        )?;

        let change = match &localize {
            PhaseEnded::Answered { text, .. } => {
                let mut builders =
                    Body::opening(brief::change(&row, text, self.redirect.as_deref()));
                Some(self.phase(
                    Call {
                        attempt,
                        phase: AttemptPhase::Change,
                        head: Head::Builders,
                        tools: &opened.workspace,
                        schema: None,
                    },
                    &mut builders,
                    control,
                )?)
            }
            // No artifact to hand over. Running Builders on an absence would
            // spend a second model call to produce a second absence.
            PhaseEnded::Stopped { .. } | PhaseEnded::Unmeasured { .. } => None,
        };

        self.land(&row, attempt, opened, &localize, change.as_ref(), control)
    }

    // -- the pieces --------------------------------------------------------

    /// How many attempts this task has had, counted from the log.
    ///
    /// ⚠ A count, not a budget. `AttemptStarted` is written once per attempt
    /// and is never mutated (ADR-0004), so this cannot drift the way v1's four
    /// retry mechanisms did — each of which mutated the row it retried, which is
    /// why *what did the previous attempt do* is unanswerable there (F150).
    fn attempts_so_far(&self, task: TaskId) -> Result<u32> {
        let n = self
            .store
            .task_history(task)?
            .into_iter()
            .filter(|l| matches!(l.event, Event::AttemptStarted { .. }))
            .count();
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Take the opening snapshot, cut a worktree at it, and open the tool layer
    /// over that worktree.
    fn open_workspace(&mut self, row: &TaskRow, watch: Watch) -> Result<Opened> {
        let taken = self.checkpoint(row, "before")?;

        fs::create_dir_all(&self.worktrees).map_err(|source| DriveError::Io {
            what: "creating the worktree directory",
            source,
        })?;
        let path = self
            .worktrees
            .join(format!("{}-{}", row.id, taken.id.born()));
        let worktree = self.repo.open_worktree(&path, &taken.sha)?;
        self.store.append(Event::WorktreeOpened {
            task: row.id,
            path: path.display().to_string(),
            sha: taken.sha.to_string(),
        })?;

        let mut workspace = Workspace::open(worktree.path())
            .map_err(|source| DriveError::Io {
                what: "opening the attempt's workspace",
                source,
            })?
            // The operator's urgent verb reaches a running tool child through
            // this, which is the whole of `shared_child`'s justification and was
            // measured at 4 ms to the child (F491).
            .watching(watch.clone());
        if let Some(toolchain) = self.toolchain {
            workspace = workspace.with_toolchain(toolchain);
        }

        Ok(Opened {
            opening: taken,
            worktree,
            workspace,
            watch,
        })
    }

    /// One phase: enter it on the log, run the turn loop, and insist that every
    /// event the loop produced actually landed.
    fn phase(
        &mut self,
        call: Call<'_>,
        body: &mut Body,
        control: &mut ControlPoint,
    ) -> Result<PhaseEnded> {
        let Call {
            attempt,
            phase,
            head,
            tools,
            schema,
        } = call;
        self.store
            .append(Event::AttemptPhaseEntered { attempt, phase })?;

        let provider = self.provider;
        let model = self.model.clone();
        let limits = self.limits;
        let ceiling = self.ceiling;
        let secrets = self.secrets.clone();

        let mut journal = StoreJournal::new(self.store);
        // 🚨 `None` for the two phases whose artifact is prose an operator
        // reads, and [`judge::REVIEW`] for the one whose artifact has a declared
        // shape. Schema-constrained decoding works and the donor uses it nowhere
        // (W1 F86); what it buys here is that a finding without something
        // runnable in it is unrepresentable rather than discouraged — and what
        // it costs is 2.8x the decode and a token cap that has already eaten 17
        // of 57 calls (ADR-0008).
        let ended = TurnLoop::new(provider, tools, model)
            .limits(limits)
            .ceiling(ceiling)
            .secrets(secrets)
            .run(head, attempt, schema, body, control, &mut journal);
        journal.into_result()?;
        Ok(ended)
    }

    /// End the attempt and put the task where its ending implies.
    fn land(
        &mut self,
        row: &TaskRow,
        attempt: AttemptId,
        opened: Opened,
        localize: &PhaseEnded,
        change: Option<&PhaseEnded>,
        control: &mut ControlPoint,
    ) -> Result<Landed> {
        let task = row.id;
        let last = change.unwrap_or(localize);

        // Rule 4: the verb is acknowledged before anything acts on it.
        if let PhaseEnded::Stopped { stop, .. } = last {
            self.store.append(Event::ControlApplied {
                task,
                control: stop.control.clone(),
            })?;
        }

        let keep = keep_for(last);
        let kept = match &keep {
            // 🚨 The only verb that keeps nothing. Everything else snapshots
            // first: 0.16 s (F490) against a tree nobody can look at afterwards.
            Keep::Nothing => None,
            Keep::AtCheckpoint | Keep::AndFork { .. } => {
                Some(self.checkpoint_worktree(row, &opened.worktree)?)
            }
        };

        // 🚨 **The gate runs here and it cannot run anywhere else.** It needs the
        // closing snapshot to exist — a measurement is stamped with the sha it
        // was taken at (ADR-0009 §6) — and it needs the worktree to still be
        // there, because that is the tree the checkers run in and the operator's
        // checkout must never be the thing a rung compiles.
        let mut gate = match (kept.as_ref(), last) {
            // 🚨🚨 **F655: `Unmeasured` is here because the tree does not care
            // why the model stopped talking.** This arm used to be `Answered`
            // alone, and the comment beside it — *an attempt that ended in an
            // absence has nothing to measure* — folded two different absences
            // into one word. `SaidNothing` really is nothing to measure;
            // `BudgetExhausted` and `TruncatedAtCap` are *the model was still
            // working when the clock ran out*, and DEBUG-P4 measured what that
            // costs: across five sorties on one subject, **six of ten attempts
            // changed the tree and the gate was asked about one**, while two of
            // the five it skipped passed all 544 tests when rebuilt by hand.
            //
            // ⚠ **It cannot cost a cold build on an unchanged tree**, which is
            // the objection this arm has to answer. It does not, and not by a
            // check written here: [`Gate::measure`] runs [`Rung::Structural`]
            // first, that rung *owns* the empty diff, and a refusal breaks the
            // walk — so an attempt that wrote nothing pays 0.024 s and never
            // reaches the acceptance command. Asking the ladder rather than
            // re-deriving *did anything change* is deliberate: `rung.rs` is
            // explicit that a second copy of that rule is a second thing that
            // can drift from the first.
            //
            // 🚨 **And since 2026-09-11 it promotes as well as measures.** F655
            // left that open on purpose — whether a gate-green tree the model
            // never declared finished may be `Accomplished` is a question about
            // who is allowed to stop, and it was not this arm's to answer. The
            // operator answered it: [`ending`] now reads `Headline::Green` under
            // `Unmeasured` too, so a `BudgetExhausted` attempt over a tree every
            // declared rung passed lands `Accomplished` rather than `Uncertain`
            // with its rungs sitting unread on the log.
            (Some(closing), PhaseEnded::Answered { .. } | PhaseEnded::Unmeasured { .. }) => {
                Some(self.gate(attempt, &opened, closing)?)
            }
            // An attempt the operator stopped is not ours to judge — and that is
            // now the *only* way here, because `keep_for` hands `Keep::Nothing`
            // to a stop and to nothing else.
            _ => None,
        };

        // 🚨 **The Judge runs here, and nothing below reads what it said.** It
        // needs the worktree alive for the diff and the measurements to already
        // exist, which puts it in exactly one place; and `ending` a few lines
        // down is computed from `last` and `gate.headline`, both of which are
        // already fixed by the time this is called. `Report::note` adds a
        // `Claim` and a `Claim` is not an `Outcome`, so this call cannot move
        // the attempt however it goes.
        //
        // ⚠ **F655 widened the gate and deliberately did not widen this.** The
        // Judge is a model call — a minute and a few thousand tokens — and it
        // decides nothing by construction, so running it on every attempt that
        // ran out of rounds would buy prose at the price of the thing the round
        // budget was already short of. The measurement is what was missing; the
        // review was not.
        let judged = match (gate.as_mut(), kept.as_ref(), last) {
            (Some(measured), Some(closing), PhaseEnded::Answered { .. }) => {
                self.judge(row, attempt, &opened, closing, measured, control)?
            }
            // ⚠ Stated rather than omitted, like every other absence here — a
            // gate with rungs on it and no review beside them should say which
            // of the two was skipped and why, rather than leave an operator to
            // infer it from a gap.
            (Some(_), Some(_), _) => self.not_asked(
                "the model never said the change was finished, so there is a measurement to \
                 read but no completed change to review",
            )?,
            _ => None,
        };

        let path = opened.worktree.path().display().to_string();
        match opened.worktree.close() {
            Ok(()) => {
                self.store.append(Event::WorktreeClosed { task, path })?;
            }
            // A worktree that will not go is worth saying out loud and is not
            // worth failing an attempt over — the work is already in the
            // checkpoint. `close` prunes even when the remove fails, so the
            // metadata does not accumulate the way BCF's does (F325).
            Err(e) => {
                self.store.append(Event::Note {
                    text: format!("the worktree at {path} would not close: {e}"),
                })?;
            }
        }

        // A fact about the log, not a budget: how many attempts this task has
        // had, the one that just ended included. The *policy* — how many it may
        // have — is the fleet's and is not read here.
        let spent = self.attempts_so_far(task)?;
        let ending = ending(
            last,
            attempt,
            kept.as_ref(),
            gate.as_ref(),
            self.retry_available,
            spent,
        );
        self.store.append(Event::AttemptEnded {
            task,
            attempt,
            outcome: ending.outcome.clone(),
        })?;

        let state = match ending.landing {
            Landing::Accomplished => self.command(task, Command::Accomplish { attempt })?,
            Landing::HandToOperator { question } => {
                let asked = self.store.append(Event::OperatorPrompted {
                    task,
                    attempt,
                    question,
                })?;
                self.command(
                    task,
                    Command::RequestOrders {
                        attempt,
                        prompt: PromptId::at(asked.seq),
                    },
                )?
            }
            Landing::Failed => self.command(task, Command::Fail { attempt })?,
            Landing::Requeue { of } => self.command(
                task,
                Command::Requeue {
                    why: RequeueReason::AttemptRetryable { of },
                },
            )?,
            Landing::Hold { checkpoint } => self.command(task, Command::Hold { checkpoint })?,
            Landing::Abort => self.command(
                task,
                Command::Abort {
                    reason: AbortReason::Operator {
                        by: OPERATOR.to_owned(),
                    },
                },
            )?,
        };

        Ok(Landed {
            attempt,
            outcome: ending.outcome,
            state,
            localize: localize.report().clone(),
            change: change.map(|c| c.report().clone()),
            kept: kept.map(|k| k.sha.to_string()),
            gate,
            next: ending.next,
            judge: judged,
        })
    }

    /// Walk the deterministic ladder over the attempt's own snapshot pair, and
    /// put every rung on the log.
    ///
    /// ADR-0009 §7: **every rung, measured or not, is an event.** That is what
    /// makes the report a projection of the log rather than a second source of
    /// truth, and it is why this writes `RungRecorded` for the absences too — a
    /// rung that could not run is the fact an operator most needs and the one a
    /// gate is most tempted to drop.
    ///
    /// ⚠ The gate watches the control point, so an operator's `Halt` reaches a
    /// running cold build instead of waiting it out. A ten-minute rung with no
    /// stop verb would be the one place in the system where the console goes
    /// deaf.
    fn gate(&mut self, attempt: AttemptId, opened: &Opened, closing: &Kept) -> Result<Measured> {
        // 🚨 `Repo::open` on the *worktree*, for `checkpoint_worktree`'s reason:
        // it shares the git directory, so both snapshot shas resolve, and every
        // path the gate touches is inside the attempt's own tree.
        let inner = Repo::open(opened.worktree.path())?;
        let mut gate = Gate::open(&inner, opened.worktree.path()).watching(opened.watch.clone());
        if let Some(toolchain) = self.toolchain {
            gate = gate.with_toolchain(toolchain);
        }
        let measured = gate.measure(&opened.opening.sha, &closing.sha);

        for outcome in measured.report.outcomes() {
            self.store.append(Event::RungRecorded {
                attempt,
                outcome: outcome.clone(),
            })?;
        }
        Ok(measured)
    }

    /// **A4 Judge: one model call, no tools, and it sees the measurements.**
    ///
    /// 🚨 **It cannot refuse, and not by discipline.** What it produces is an
    /// [`abcc_core::outcome::Claim`], attached with `Report::note`, which touches
    /// no `Outcome` and therefore no `Headline`; `AttemptPhase::may_refuse` is
    /// `!uses_model()`; and there is no function in this workspace that converts
    /// a `Claim` into an `Outcome`. There is nothing here to wire a vote into.
    ///
    /// 🚨 **And it cannot fail the attempt either**, which is the half that is
    /// easy to get wrong: every way this can go badly returns `Ok`, because the
    /// caller's `ending` was already decided by the ladder. A review that times
    /// out, fills the window or comes back malformed leaves the attempt exactly
    /// where the measurements put it.
    ///
    /// **Two things stop the call from being made at all**, and both are written
    /// on the log rather than passed over in silence:
    ///
    /// * **an empty diff** — the structural rung has already refused, and there
    ///   is nothing to review. It is not a saving invented here: on this
    ///   project's 25 logged attempts the structural rung refuses **16** (F518),
    ///   so this is the common case rather than the corner one.
    /// * **a diff over [`judge::MAX_PATCH_CHARS`]** — see that constant for why
    ///   it is not cut down to fit.
    fn judge(
        &mut self,
        row: &TaskRow,
        attempt: AttemptId,
        opened: &Opened,
        closing: &Kept,
        measured: &mut Measured,
        control: &mut ControlPoint,
    ) -> Result<Option<PhaseEnded>> {
        // 🚨 `Repo::open` on the worktree, for `Driver::gate`'s reason: it shares
        // the git directory, so both snapshot shas resolve there.
        let patch = Repo::open(opened.worktree.path())
            .and_then(|repo| repo.patch_between(&opened.opening.sha, &closing.sha));
        let patch = match patch {
            Ok(patch) => patch,
            // ⚠ Not an error and not an attempt-level `Why`. Nothing about the
            // work is different because we could not produce a diff of it, and
            // the ladder has already measured the same pair.
            Err(e) => return self.not_asked(&format!("git would not produce the diff — {e}")),
        };
        if patch.trim().is_empty() {
            return self
                .not_asked("the two snapshots are identical, so there is no change to review");
        }
        if patch.len() > judge::MAX_PATCH_CHARS {
            return self.not_asked(&format!(
                "the diff is {} characters against a {}-character ceiling, and a review of \
                 part of a change is a review of a different change",
                patch.len(),
                judge::MAX_PATCH_CHARS
            ));
        }

        // 🚨 A **fresh body**, holding the task, the diff and the measurements —
        // and nothing either model wrote in prose. Same model, fresh call:
        // 11/12 reading the diff against 4/12 continuing the author's own
        // conversation, with five empty payloads (F282).
        let mut body = Body::opening(judge::brief(&judge::Dossier {
            title: &row.title,
            prompt: &row.prompt,
            patch: &patch,
            measured,
        }));
        let ended = self.phase(
            Call {
                attempt,
                phase: AttemptPhase::Judge,
                head: Head::Commandos,
                tools: &NoTools::for_head(Head::Commandos),
                schema: Some(judge::REVIEW),
            },
            &mut body,
            control,
        )?;

        // The claim goes on the report and the report's headline was computed
        // before this ran. Nothing else is done with it here.
        if let PhaseEnded::Answered { text, .. } = &ended {
            measured.report.note(judge::read(text));
        }
        Ok(Some(ended))
    }

    /// Say on the log why the one call was not made, and hand back `None`.
    ///
    /// A phase that did not run and a phase that ran and found nothing are two
    /// different facts, and this is the first one. It is a `Note` rather than a
    /// `PhaseEnded` because the phase was never entered: writing
    /// `AttemptPhaseEntered` for a phase that did not happen would put a
    /// contradiction on a log whose whole value is that it does not have any.
    fn not_asked(&mut self, why: &str) -> Result<Option<PhaseEnded>> {
        self.store.append(Event::Note {
            text: format!("the Judge was not asked: {why}"),
        })?;
        Ok(None)
    }

    /// The closing snapshot, taken **in the worktree**.
    ///
    /// [`Repo::open`] asks `rev-parse --show-toplevel`, so it treats "this is a
    /// worktree" as the normal case — which under ADR-0007 it is — and the
    /// scratch index lands in the shared git directory, outside this working
    /// tree, which is what keeps `git add -A` from being asked to stage the file
    /// it is writing.
    fn checkpoint_worktree(&mut self, row: &TaskRow, worktree: &Worktree) -> Result<Kept> {
        let inner = Repo::open(worktree.path())?;
        self.snapshot(&inner, row, "after")
    }

    fn checkpoint(&mut self, row: &TaskRow, when: &str) -> Result<Kept> {
        let repo = self.repo.clone();
        self.snapshot(&repo, row, when)
    }

    fn snapshot(&mut self, repo: &Repo, row: &TaskRow, when: &str) -> Result<Kept> {
        snapshot(self.store, repo, row, when)
    }

    // -- the store, through one door ---------------------------------------

    fn row(&self, task: TaskId) -> Result<TaskRow> {
        self.store.task(task)?.ok_or(DriveError::NoSuchTask(task))
    }

    /// Send a lifecycle command and read back where it landed.
    ///
    /// The state comes from the projection rather than from the command, because
    /// [`TaskState::apply`] is the authority on where a command goes and a second
    /// copy of that table here is a second copy that can drift.
    fn command(&mut self, task: TaskId, command: Command) -> Result<TaskState> {
        let name = command.name();
        match self.store.apply(task, command)? {
            Applied::Moved(_) => Ok(self.row(task)?.state),
            Applied::Refused { refusal, .. } => Err(DriveError::Refused {
                command: name,
                refusal,
            }),
        }
    }
}

/// Who a stop is attributed to. One constant, because every verb on the control
/// channel came from the console and there is nobody else it could be.
const OPERATOR: &str = "operator";

/// One phase's worth of arguments, in one place.
///
/// It is a struct because the list stopped fitting: the Gate milestone added the
/// schema, and a phase is now *which phase, whose head, what tools it may reach
/// and what shape its answer has to be* — four facts that have to agree with
/// each other, and a positional list of them is four chances to pass Recon's
/// head with Builders' tools.
#[derive(Clone, Copy)]
struct Call<'a> {
    attempt: AttemptId,
    phase: AttemptPhase,
    head: Head,
    tools: &'a dyn Tools,
    /// `None` for an artifact that is prose, [`judge::REVIEW`] for one with a
    /// declared shape. See [`Driver::phase`].
    schema: Option<Schema>,
}

/// The attempt's isolation, alive for as long as the attempt is.
struct Opened {
    /// 🚨 The whole opening snapshot and not only its id, because the gate's
    /// free rung is the diff between this sha and the closing one — and a
    /// `CheckpointId` is a position in the log, which git cannot diff.
    opening: Kept,
    worktree: Worktree,
    workspace: Workspace,
    /// The same watch the workspace got, kept so the gate's rungs are as
    /// stoppable as the tool children were. A cold build is the longest thing
    /// this system does, and it would be the one place an operator's verb goes
    /// unanswered.
    watch: Watch,
}

/// Snapshot `repo`, write it to a ref, and record it on the log.
///
/// 🚨 The ref name is disambiguated by the log head at the moment it is taken.
/// Every checkpoint appends this event, so two of them always have an event
/// between them and two names cannot collide — and the name then says *where in
/// the replay* the snapshot was taken, which under ADR-0005 is the same fact as
/// when.
///
/// 🚨 **It is a free function rather than a method because the driver is not the
/// only thing that takes checkpoints any more.** `abcc take` cuts the operator a
/// worktree and `abcc release` snapshots what they did in it, and both are a
/// second process with its own [`Store`] and no [`Driver`] at all. Two copies of
/// these six lines would be two places that could disagree about what a
/// checkpoint ref is called — and the ref name is the thing that stops
/// `git gc --prune=now` collecting the snapshot (F330), so a disagreement there
/// is work quietly lost rather than a message somebody reads.
///
/// # Errors
///
/// [`DriveError::Store`] if the log will not take the event, [`DriveError::Vcs`]
/// if git will not take the snapshot.
pub fn snapshot(store: &mut Store, repo: &Repo, row: &TaskRow, when: &str) -> Result<Kept> {
    let at = store.head()?;
    let git_ref = checkpoint_ref(&row.mission.to_string(), at.get());
    let sha = repo.checkpoint(&git_ref, &format!("abcc {} {when} {at}", row.id))?;
    let logged = store.append(Event::CheckpointTaken {
        task: row.id,
        sha: sha.to_string(),
        git_ref,
    })?;
    Ok(Kept {
        id: CheckpointId::at(logged.seq),
        sha,
    })
}

/// A snapshot that was taken and recorded.
///
/// Public because [`snapshot`] is: the operator's take-over is a second caller
/// of the same recipe, in another crate.
#[derive(Debug, Clone)]
pub struct Kept {
    pub id: CheckpointId,
    pub sha: Sha,
}

/// Where the driver takes the task when the attempt is over.
///
/// 🚨 **`Accomplished` is here now, and the Gate milestone is what put it
/// there.** It was absent for the whole of Skeleton because there was nothing
/// entitled to say it; the entitlement is [`Headline::Green`], which requires
/// every declared rung to have produced a measurement and none of them to be
/// red. What still cannot reach it is anything a model said.
enum Landing {
    /// Every declared rung measured, and none of them refused.
    Accomplished,
    /// Nothing measured it, so a human is owed the question.
    HandToOperator { question: String },
    /// The attempt produced no artifact, and no attempt would.
    Failed,
    /// 🚨 **The attempt produced no artifact and another one plausibly
    /// would — so the task goes back on the board rather than into a terminal
    /// state (F548).**
    ///
    /// This is the landing that makes rule 5 true. The driver returns
    /// [`NextAction::Attempt`] here and does not act on it; landing `Failed`
    /// alongside that recommendation *was* acting on it, because `Failed` is
    /// terminal and `TaskState::apply` refuses every command on a terminal state
    /// before it reaches the transition table. The retry budget is the fleet's
    /// (ADR-0010), and a budget that cannot be spent is not a budget.
    Requeue { of: AttemptId },
    /// The operator stopped it and the work is at a checkpoint.
    Hold { checkpoint: CheckpointId },
    /// The operator stopped it and kept nothing.
    Abort,
}

/// The three answers one ending produces: what the attempt was, where the task
/// goes, and what the fleet should do about it. They are computed together
/// because they are one judgement, and splitting them is how two of them start
/// disagreeing.
struct Ending {
    outcome: AttemptOutcome,
    next: Option<NextAction>,
    landing: Landing,
}

/// What to do with the work, given how the phase ended.
fn keep_for(last: &PhaseEnded) -> Keep {
    match last {
        PhaseEnded::Stopped { stop, .. } => stop.keep.clone(),
        // Not the operator's ending, so the work is kept either way. A failed
        // attempt whose tree nobody can look at is a failure report with the
        // evidence deleted.
        PhaseEnded::Answered { .. } | PhaseEnded::Unmeasured { .. } => Keep::AtCheckpoint,
    }
}

/// The one landing a measurement is entitled to produce.
///
/// 🚨 It is a function and not two literals because there are now **two** ways
/// to reach it — the model said it was finished, or it never got to say so and
/// the ladder came back green anyway — and rule 1 is that *only a measurement
/// says `Accomplished`*. Two copies of the landing would be two places for that
/// rule to be edited into disagreement.
fn accomplished() -> Ending {
    Ending {
        outcome: AttemptOutcome::Success,
        next: Some(NextAction::Stop),
        landing: Landing::Accomplished,
    }
}

fn ending(
    last: &PhaseEnded,
    attempt: AttemptId,
    kept: Option<&Kept>,
    gate: Option<&Measured>,
    retry_available: bool,
    spent: u32,
) -> Ending {
    match last {
        // The model answered. What the attempt *is* now depends on what the
        // ladder found, and on nothing the model wrote.
        PhaseEnded::Answered { .. } => {
            let artifact = kept.map_or_else(
                || "the attempt's worktree".to_owned(),
                |k| k.sha.as_str().to_owned(),
            );
            match gate.map(|g| &g.headline) {
                // 🚨 The one path to success in the whole system, and it is a
                // conjunction of measurements: `Green` means every declared rung
                // ran and none of them refused.
                Some(Headline::Green { .. }) => accomplished(),
                // 🚨 A deterministic rung refused. The task goes to the operator
                // rather than to `Failed`, and the recommendation is another
                // attempt — the two do not disagree, because the rule this
                // driver holds is *a task may not go terminal while something is
                // owed to a person*, and a refusal that is one line from landing
                // is exactly what a person should get to look at. F512 is that
                // situation six times over.
                Some(Headline::Red { rung, detail }) => {
                    let question = brief::refused(rung, detail, &artifact);
                    Ending {
                        outcome: AttemptOutcome::Refused {
                            rung: rung.clone(),
                            detail: detail.clone(),
                        },
                        next: Some(NextAction::Attempt {
                            cause: Cause::Retry { of: attempt },
                        }),
                        landing: Landing::HandToOperator { question },
                    }
                }
                // The ladder could not see enough to say either way, and it says
                // exactly what was missing. `Green` is a claim about coverage,
                // so this is neither a pass nor a failure.
                Some(Headline::Unverified { missing }) => {
                    let question = brief::unverified(&artifact, missing);
                    let why = missing.first().map_or_else(
                        || Why::NoCheckerForArtifact {
                            artifact: artifact.clone(),
                        },
                        |(_, why)| why.clone(),
                    );
                    Ending {
                        outcome: classify(&why),
                        next: Some(NextAction::HandToOperator {
                            question: question.clone(),
                        }),
                        landing: Landing::HandToOperator { question },
                    }
                }
                // ⚠ The gate was not asked. Reachable only when the closing
                // snapshot was not taken, which `keep_for` makes impossible for
                // an `Answered` phase — stated rather than asserted, because an
                // unmeasured artifact has an honest sentence and a panic does
                // not.
                None => {
                    let question = brief::unverified(&artifact, &[]);
                    Ending {
                        outcome: AttemptOutcome::Uncertain {
                            why: Why::NoCheckerForArtifact { artifact },
                        },
                        next: Some(NextAction::HandToOperator {
                            question: question.clone(),
                        }),
                        landing: Landing::HandToOperator { question },
                    }
                }
            }
        }
        // 🚨 **The model did not choose this ending, and a green ladder ends
        // it anyway.** David's ruling, 2026-09-10: *the model is allowed to
        // accomplish only if the gate tree is green — and updated.* F655 made the
        // gate get *asked* for an ending the model never chose and deliberately
        // did not make this function read it; the question it left open —
        // whether a gate-green tree the model never declared finished may be
        // called done — is a question about who is allowed to stop, and the
        // operator has answered it. The measurement is.
        //
        // ⚠ **There is nothing here for *and updated*, and writing it would be a
        // defect.** [`abcc_gate::Rung::Structural`] returns `exit: 1` on a tree
        // that changed no tracked file, it runs first, and a refusal breaks the
        // walk — so `Green`, which is every declared rung measured and none of
        // them red, already means the tree carries a change. A driver that
        // re-derived *did anything change* for itself would be a second copy of
        // that rule, and two copies are what drift.
        //
        // ⚠ It reads the headline and not `why`, because `why` says why the
        // *conversation* stopped and the ladder is about the *tree*.
        // [`Why::Denied`] cannot reach here at all — a refused tool call is a
        // `ToolCallEnded` and the loop carries on — and a [`Why::EngineError`]
        // over a tree every declared rung passed is still on the log as itself.
        PhaseEnded::Unmeasured { .. }
            if matches!(gate.map(|g| &g.headline), Some(Headline::Green { .. })) =>
        {
            accomplished()
        }
        PhaseEnded::Unmeasured { why, .. } => {
            let next = next_after(why, attempt);
            Ending {
                outcome: classify(why),
                // ⚠ The landing *follows* the next action rather than being
                // asserted beside it. `Landing::Failed` under a
                // `HandToOperator` would be the two disagreeing, which is the
                // thing this struct's doc comment exists to prevent.
                landing: match &next {
                    NextAction::HandToOperator { question } => Landing::HandToOperator {
                        question: question.clone(),
                    },
                    // 🚨 F548. `Attempt` and `Failed` are the pair that used
                    // to disagree, and the disagreement was invisible because
                    // nothing had ever tried to act on the recommendation. The
                    // recommendation still stands either way — what changes is
                    // whether the state it is returned beside can be acted on.
                    NextAction::Attempt { .. } if retry_available => {
                        Landing::Requeue { of: attempt }
                    }
                    // The budget is gone, and ADR-0010's rule is `Attempt`
                    // twice then `HandToOperator`. This is the only place the
                    // hand-off can happen: `RequestOrders` is an edge out of
                    // `Engaged`, and the fleet reads `next` after the attempt.
                    NextAction::Attempt { .. } => Landing::HandToOperator {
                        question: brief::exhausted(spent, why),
                    },
                    NextAction::Stop => Landing::Failed,
                },
                next: Some(next),
            }
        }
        PhaseEnded::Stopped { stop, .. } => Ending {
            outcome: AttemptOutcome::Uncertain {
                why: Why::Cancelled {
                    by: OPERATOR.to_owned(),
                },
            },
            // ⚠ `Cause::Edit` names no prompt, and the redirect's prompt is not
            // lost by that: it is on the log, in the `ControlApplied` this
            // landing already wrote.
            next: match &stop.keep {
                Keep::AndFork { .. } => Some(NextAction::Attempt {
                    cause: Cause::Edit { of: attempt },
                }),
                Keep::AtCheckpoint | Keep::Nothing => None,
            },
            landing: match (&stop.keep, kept) {
                (Keep::Nothing, _) => Landing::Abort,
                (_, Some(k)) => Landing::Hold { checkpoint: k.id },
                // Unreachable through `keep_for`, and stated rather than
                // asserted: a hold whose checkpoint was not taken is not a hold.
                (_, None) => Landing::Failed,
            },
        },
    }
}

/// Which of the four attempt outcomes a phase's `Why` is.
///
/// 🚨 Exhaustive on purpose. `Uncertain` is retryable and `HardFailure` is not,
/// which is the distinction v1's single boolean could not carry — and a
/// thirteenth `Why` should not be able to arrive here and be quietly called a
/// soft failure.
fn classify(why: &Why) -> AttemptOutcome {
    match why {
        // Absences. None of them is a statement about the work.
        Why::NoCheckerForArtifact { .. }
        | Why::CheckerNotOnHost { .. }
        | Why::SpawnFailed { .. }
        | Why::NothingToRun { .. }
        | Why::FailedBeforeRunning { .. }
        | Why::BudgetExhausted { .. }
        | Why::Cancelled { .. }
        | Why::TruncatedAtCap { .. }
        | Why::StaleMeasurement { .. }
        // 🚨 F497 and F496 are absences for the same reason as the nine above:
        // neither says anything about the work. A phase that said nothing was
        // not measured and did not fail, and a conversation that filled the
        // window was cut off rather than judged.
        | Why::SaidNothing { .. }
        | Why::ContextOverflow { .. } => AttemptOutcome::Uncertain { why: why.clone() },
        // The stream went quiet. Another attempt on a less loaded box is a
        // plausible fix, which is what makes it soft.
        Why::Timeout { .. } => AttemptOutcome::SoftFailure { why: why.clone() },
        // A role reaching for a tool its ceiling does not admit, and a fault in
        // this engine, both repeat on the next attempt unchanged.
        Why::Denied { .. } | Why::EngineError { .. } => {
            AttemptOutcome::HardFailure { why: why.clone() }
        }
    }
}

/// What the fleet should do about it, which is a *separate* judgement from what
/// the attempt was: `CheckerNotOnHost` is uncertain and retrying it installs
/// nothing.
fn next_after(why: &Why, attempt: AttemptId) -> NextAction {
    match why {
        Why::BudgetExhausted { .. }
        | Why::TruncatedAtCap { .. }
        | Why::FailedBeforeRunning { .. }
        | Why::StaleMeasurement { .. }
        // The budget went into the reasoning trace and the answer never
        // arrived. The trace's size varies 9,942–16,564 characters on
        // *identical* input (F246), so a second sample is a plausible fix in
        // exactly the way it is for `TruncatedAtCap` above.
        | Why::SaidNothing { .. }
        | Why::Timeout { .. } => NextAction::Attempt {
            cause: Cause::Retry { of: attempt },
        },
        // 🚨 The one `Why` whose fix is neither another attempt nor giving up:
        // the same body works against a larger window, and loading one is the
        // operator's to do. Retrying against the same server repeats it
        // unchanged; stopping throws away work that is one setting from
        // running.
        Why::ContextOverflow {
            window,
            prompt_tokens,
        } => NextAction::HandToOperator {
            question: brief::overflowed(*window, *prompt_tokens),
        },
        Why::NoCheckerForArtifact { .. }
        | Why::CheckerNotOnHost { .. }
        | Why::SpawnFailed { .. }
        | Why::NothingToRun { .. }
        | Why::Cancelled { .. }
        | Why::Denied { .. }
        | Why::EngineError { .. } => NextAction::Stop,
    }
}
