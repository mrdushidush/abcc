//! ADR-0012's after-action view — **the log, folded back into what happened to
//! one task.**
//!
//! ADR-0005 gives the log one job beyond durability: *boot is the same code as
//! replay*. ADR-0012 §4 spends that a second time — "replay-as-primary makes the
//! after-action screen nearly free, it is the same paged read" — and this module
//! is that screen's fold. It reads [`Logged`] and produces the answer to the
//! question a board cannot answer: **not what state a task is in, but how it got
//! there.**
//!
//! # Why this exists, in one number
//!
//! On this project's own log, twenty tasks read `MISSION FAILED` and the twenty
//! attempts under them carry **five different endings** — eight
//! [`Why::TruncatedAtCap`], five [`Why::SaidNothing`], three
//! [`Why::BudgetExhausted`], two [`Why::Timeout`] and two [`Why::EngineError`].
//! Sixteen of the twenty are [`AttemptOutcome::Uncertain`], which is *the system
//! could not tell* and not *the work was wrong*. The board prints one word over
//! all of it, faithfully — a projection of a lifecycle is a lifecycle, and the
//! ending lives on the attempt. [`Replay::endings`] is that table, and it is the
//! whole reason a person opens this.
//!
//! # 🚨 This is not [`Cause::Replay`], and the two must not be confused
//!
//! [`Cause::Replay`] *forks an attempt* — it re-runs work and writes new events,
//! and `abcc-fleet`'s budget has a comment about it. This module **writes
//! nothing**. It is the reading half of ADR-0005's sentence, the same sense in
//! which boot is a replay: a fold over events that already happened. Nothing
//! here takes `&mut`, and that is the property, not an accident of the current
//! call sites.
//!
//! # The one thing folded here that is not a task
//!
//! [`Ladder`] — W13's measurement, which is **human review minutes per merged
//! change**. A review names a *change*, which lives in the version control and
//! not in the lifecycle, so it belongs to no task and no attempt and is folded
//! beside them. It is here rather than on a screen because the archive is what
//! this project reaches for when it wants a number about its own history, and
//! for eleven thousand events it could not state the one measurement the
//! milestone is judged on.
//!
//! # The two things a fold cannot recover, said here rather than discovered
//!
//! 1. 🚨 **A gap needs an event on both sides**, which is [`crate::fun`]'s first
//!    limit and it binds here identically. [`Quiet`] reports the largest silence
//!    *between* two events of an attempt; an attempt that hung and was never
//!    written to again ends its silence at [`Event::AttemptEnded`] or not at
//!    all. ⚠ And [`Event::LivenessMark`] is emitted from inside the read loop,
//!    so it marks a stream that is *speaking* — the silences it would be most
//!    useful during are exactly the ones it cannot bound. That is why [`Quiet`]
//!    names the event that closed the gap rather than reporting a bare number:
//!    *what broke this silence* is the diagnostic, and *how long* is only the
//!    size of it.
//! 2. ⚠ **An attempt's span is bounded by seq and not by the clock.** Everything
//!    between [`Event::AttemptStarted`] and its [`Event::AttemptEnded`] is
//!    attributed to it, including the operator commands a person typed at a
//!    second process — which is right, because during an attempt the driver is
//!    the writer and a foreign append is a fact about that attempt's world.
//!
//! [`Why::TruncatedAtCap`]: crate::outcome::Why::TruncatedAtCap
//! [`Why::SaidNothing`]: crate::outcome::Why::SaidNothing
//! [`Why::BudgetExhausted`]: crate::outcome::Why::BudgetExhausted
//! [`Why::Timeout`]: crate::outcome::Why::Timeout
//! [`Why::EngineError`]: crate::outcome::Why::EngineError

use std::collections::BTreeSet;

use crate::attempt::{AttemptOutcome, Cause};
use crate::event::{CallShape, Event, Logged, TraceSignal};
use crate::fun::SILENCE_BAR_MS;
use crate::outcome::{Outcome, Why};
use crate::run::AttemptPhase;
use crate::seq::{AttemptId, CheckpointId, MissionId, PromptId, Seq, TaskId, UnitId};
use crate::task::{Command, TaskState};

/// The whole log, folded into one [`TaskTrace`] per task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replay {
    /// One per [`Event::TaskCreated`], in the order the log created them.
    pub tasks: Vec<TaskTrace>,
    /// How many events the fold walked, so a report can say what it read.
    pub events: usize,
    /// 🚨 **W13's ladder**, which is the one measurement `PLAN.md` §5 requires
    /// from the first milestone — and the one thing on this log that belongs to
    /// no task. A review names a **change**, not an attempt, so it folds here
    /// beside the tasks rather than under one.
    pub ladder: Ladder,
}

impl Replay {
    /// Fold a log.
    ///
    /// Linear in the log and single-pass over it, except for the per-attempt
    /// silence scan, which re-reads each attempt's own span.
    #[must_use]
    pub fn over(log: &[Logged]) -> Self {
        let mut fold = Fold::default();
        for logged in log {
            fold.take(logged);
        }
        let mut tasks = fold.tasks;
        // The silence scan runs after the walk because an attempt's span is
        // known only once its ending is, and because a gap is a property of two
        // consecutive events rather than of either one of them.
        for task in &mut tasks {
            for attempt in &mut task.attempts {
                attempt.scan_silence(log);
            }
        }
        Self {
            tasks,
            events: log.len(),
            ladder: fold.ladder,
        }
    }

    /// One task, by id.
    #[must_use]
    pub fn task(&self, id: TaskId) -> Option<&TaskTrace> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// 🚨 **What a board's one word is standing over**: every task's final state
    /// against the endings of the attempts underneath it.
    ///
    /// Sorted by how many attempts a state holds, then by the state's name, so
    /// the table is stable between runs of the same log.
    #[must_use]
    pub fn endings(&self) -> Vec<StateEndings> {
        let mut rows: Vec<StateEndings> = Vec::new();
        for task in &self.tasks {
            let Some(state) = &task.state else { continue };
            let name = state.name();
            if !rows.iter().any(|r| r.state.name() == name) {
                rows.push(StateEndings {
                    state: state.clone(),
                    tasks: Vec::new(),
                    endings: Vec::new(),
                });
            }
            let Some(row) = rows.iter_mut().find(|r| r.state.name() == name) else {
                continue;
            };
            row.tasks.push(task.id);
            for attempt in &task.attempts {
                let Some(outcome) = &attempt.outcome else {
                    continue;
                };
                let label = ending(outcome);
                if let Some(e) = row.endings.iter_mut().find(|(l, _)| *l == label) {
                    e.1 += 1;
                } else {
                    row.endings.push((label, 1));
                }
            }
            row.endings
                .sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        }
        rows.sort_by(|a, b| {
            b.tasks
                .len()
                .cmp(&a.tasks.len())
                .then(a.state.name().cmp(b.state.name()))
        });
        rows
    }
}

/// The walk's state: the traces so far, and the two indices that put an event
/// on one of them.
///
/// ⚠ `TaskId` and `AttemptId` are both the `seq` of their creating event, so an
/// index needs to hold nothing but a position — there is no allocator here to
/// disagree with the log about identity.
#[derive(Default)]
struct Fold {
    tasks: Vec<TaskTrace>,
    ladder: Ladder,
    by_task: Vec<(TaskId, usize)>,
    /// `(attempt, task position, attempt position within that task)`.
    by_attempt: Vec<(AttemptId, usize, usize)>,
}

impl Fold {
    fn take(&mut self, logged: &Logged) {
        let (seq, at) = (logged.seq, logged.at_ms);
        match &logged.event {
            Event::TaskCreated {
                mission,
                title,
                prompt,
            } => {
                self.by_task.push((TaskId::at(seq), self.tasks.len()));
                self.tasks.push(TaskTrace {
                    id: TaskId::at(seq),
                    mission: *mission,
                    title: title.clone(),
                    prompt: prompt.clone(),
                    created_ms: at,
                    state: None,
                    transitions: Vec::new(),
                    attempts: Vec::new(),
                    asked: Vec::new(),
                });
            }
            Event::TaskTransitioned {
                task,
                command,
                from,
                to,
            } => {
                if let Some(t) = self.task_mut(*task) {
                    t.transitions.push(Move {
                        seq,
                        at_ms: at,
                        command: command.clone(),
                        from: from.clone(),
                        to: to.clone(),
                    });
                    t.state = Some(to.clone());
                }
            }
            Event::AttemptStarted {
                task,
                unit,
                cause,
                checkpoint_from,
            } => self.open_attempt(seq, at, *task, *unit, cause, *checkpoint_from),
            Event::OperatorPrompted { task, question, .. } => {
                // Also carries an `attempt`, and is filed under both: the
                // question belongs to the person's view of the task, and the
                // spend belongs to the attempt that raised it.
                if let Some(t) = self.task_mut(*task) {
                    t.asked.push(Asked {
                        prompt: PromptId::at(seq),
                        question: question.clone(),
                        answer: None,
                    });
                }
                self.absorb(&logged.event, seq, at);
            }
            Event::OperatorAnswered {
                task,
                prompt,
                answer,
            } => {
                if let Some(t) = self.task_mut(*task)
                    && let Some(a) = t.asked.iter_mut().find(|a| a.prompt == *prompt)
                {
                    a.answer = Some(answer.clone());
                }
            }
            // 🚨 The one event here that names no task and no attempt. A review
            // is about a *change*, which is a thing in the version control and
            // not a thing in the lifecycle, so it folds beside the tasks.
            Event::ReviewRecorded {
                change,
                seconds,
                by,
                crossed_boundary,
            } => self.ladder.record(change, *seconds, by, *crossed_boundary),
            // Filed under both, for `OperatorPrompted`'s reason: the landing is
            // a fact about a *change*, which is where the ladder reads it, and
            // it is also the one thing that ever happened to the attempt after
            // it ended, which is where the after-action view wants it.
            Event::ChangeLanded {
                task,
                attempt,
                change,
                from,
                to,
                rungs,
            } => {
                self.ladder.record_landing(Landed {
                    change: change.clone(),
                    task: *task,
                    attempt: *attempt,
                    from: from.clone(),
                    to: to.clone(),
                    rungs: *rungs,
                    seq,
                    at_ms: at,
                });
                self.absorb(&logged.event, seq, at);
            }
            other => self.absorb(other, seq, at),
        }
    }

    fn task_mut(&mut self, id: TaskId) -> Option<&mut TaskTrace> {
        let at = self
            .by_task
            .iter()
            .find(|(t, _)| *t == id)
            .map(|(_, i)| *i)?;
        self.tasks.get_mut(at)
    }

    fn open_attempt(
        &mut self,
        seq: Seq,
        at_ms: i64,
        task: TaskId,
        unit: UnitId,
        cause: &Cause,
        checkpoint_from: Option<CheckpointId>,
    ) {
        let Some(ti) = self
            .by_task
            .iter()
            .find(|(t, _)| *t == task)
            .map(|(_, i)| *i)
        else {
            return;
        };
        let Some(trace) = self.tasks.get_mut(ti) else {
            return;
        };
        self.by_attempt
            .push((AttemptId::at(seq), ti, trace.attempts.len()));
        trace.attempts.push(AttemptTrace {
            id: AttemptId::at(seq),
            unit,
            cause: cause.clone(),
            checkpoint_from,
            started: seq,
            started_ms: at_ms,
            ended: None,
            ended_ms: None,
            outcome: None,
            phases: Vec::new(),
            tools: Vec::new(),
            spend: Spend::default(),
            made: Made::default(),
            discarded: Vec::new(),
            nudges: 0,
            marks: 0,
            claims: 0,
            rungs: Vec::new(),
            quiet: None,
            over_bar: 0,
            open_phase: None,
            landed: None,
        });
    }

    /// File an attempt-scoped event on the attempt that owns it.
    ///
    /// ⚠ An event naming an attempt this fold never saw start is dropped
    /// rather than invented: a log read from the middle is a normal thing to be
    /// handed, and a synthesised attempt would be a row nothing on disk backs.
    fn absorb(&mut self, event: &Event, seq: Seq, at_ms: i64) {
        let Some(id) = attempt_of(event) else { return };
        let Some(&(_, ti, ai)) = self.by_attempt.iter().find(|(a, ..)| *a == id) else {
            return;
        };
        if let Some(attempt) = self.tasks.get_mut(ti).and_then(|t| t.attempts.get_mut(ai)) {
            attempt.absorb(seq, at_ms, event);
        }
    }
}

// ---------------------------------------------------------------------------
// W13's ladder
// ---------------------------------------------------------------------------

/// 🚨 **Human review minutes per merged change** — W13's ladder, and the one
/// measurement `PLAN.md` §5 requires from the first milestone rather than from
/// Self-Host, because a ladder whose baseline starts at the finish is
/// unfalsifiable.
///
/// 🚨 **The unit is the change, and a change reviewed twice is one change.**
/// `abcc review` takes a `--by`, so two people reading one change is ordinary
/// use and so is a second pass over it. Counting the *events* would report two
/// changes at half the minutes each — the ladder moving in its good direction
/// as a reward for reviewing more. So rows are keyed on the change, and
/// [`Ladder::recordings`] is kept beside [`Ladder::changes`] for the same reason
/// `seeds` is kept apart from `seeded_calls` (F722): the gap between the two is
/// the second pass, and only a reading that keeps both can say so.
///
/// ⚠ **The denominator is not on this log.** These are the changes somebody
/// recorded a review of. A change that was merged and never reviewed writes no
/// event at all, so no coverage figure can be derived here — and none is
/// offered, because *reviewed 5 of 5* would be true of this fold and false of
/// the repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ladder {
    /// One row per distinct change, in the order the log first reviewed each.
    pub changes: Vec<Reviewed>,
    /// How many [`Event::ReviewRecorded`] events the fold walked — passes, not
    /// changes.
    pub recordings: u32,
    /// 🚨 **The denominator, for the changes that came through `abcc land`.**
    /// One row per landing, in log order.
    ///
    /// ⚠ It does **not** repair F727 in general — a change merged by hand still
    /// writes nothing, so this is *of the changes abcc landed* and never *of the
    /// repository*. What it does make answerable is the narrower question that
    /// was previously unaskable: of the work this tool put on the branch, how
    /// much has a person actually read.
    pub landings: Vec<Landed>,
}

impl Ladder {
    /// Every minute anybody recorded, in seconds.
    #[must_use]
    pub fn total_seconds(&self) -> u64 {
        self.changes.iter().map(|c| c.seconds).sum()
    }

    /// The median change's seconds, which is the figure a trend is read off.
    ///
    /// ⚠ The median **per change**, never per recording: a change reviewed
    /// twice contributes its total once, because that is what the ladder's unit
    /// means. `None` for an empty ladder — an absence rather than a zero, since
    /// zero is also a legal median.
    #[must_use]
    pub fn median_seconds(&self) -> Option<u64> {
        if self.changes.is_empty() {
            return None;
        }
        let mut all: Vec<u64> = self.changes.iter().map(|c| c.seconds).collect();
        all.sort_unstable();
        let mid = all.len() / 2;
        Some(if all.len().is_multiple_of(2) {
            u64::midpoint(all[mid - 1], all[mid])
        } else {
            all[mid]
        })
    }

    /// How many changes crossed a module boundary — M3 counts ten consecutive
    /// of those, so it is the count that milestone is read against.
    #[must_use]
    pub fn crossed(&self) -> usize {
        self.changes.iter().filter(|c| c.crossed_boundary).count()
    }

    /// The landings nobody has recorded a review of yet.
    ///
    /// 🚨 **This is the one coverage figure the ladder may state**, and it is
    /// stated in the only direction that is sound: a landing is on the log, a
    /// review of it either is or is not, so *these have not been reviewed* is a
    /// fact about rows rather than an estimate about the repository. The inverse
    /// — *what fraction of merges were reviewed* — stays unanswerable for
    /// F727's reason, and asking this function for it would be reading a
    /// denominator that is not here.
    ///
    /// The match is on the recorded string and nothing resolves it, for
    /// [`Reviewed::change`]'s reason: two spellings of one commit are two
    /// things, because the log holds a string and this crate cannot ask git.
    #[must_use]
    pub fn unreviewed(&self) -> Vec<&Landed> {
        self.landings
            .iter()
            .filter(|l| !self.changes.iter().any(|c| c.change == l.change))
            .collect()
    }

    /// Take one landing.
    pub fn record_landing(&mut self, landed: Landed) {
        self.landings.push(landed);
    }

    /// Take one recording.
    ///
    /// 🚨 **Public because it is the one place the ladder's unit is decided**,
    /// and there are two readers of it: this crate's after-action fold and
    /// `abcc-tui`'s live status bar. When each kept its own tally they could
    /// disagree about the denominator without either being visibly wrong — and
    /// for the whole life of the project neither had ever seen a row, so nothing
    /// would have shown it.
    pub fn record(&mut self, change: &str, seconds: u32, by: &str, crossed_boundary: bool) {
        self.recordings += 1;
        let at = self
            .changes
            .iter()
            .position(|c| c.change == change)
            .unwrap_or_else(|| {
                self.changes.push(Reviewed {
                    change: change.to_owned(),
                    seconds: 0,
                    by: Vec::new(),
                    crossed_boundary: false,
                    recordings: 0,
                });
                self.changes.len() - 1
            });
        let row = &mut self.changes[at];
        row.seconds += u64::from(seconds);
        row.recordings += 1;
        // 🚨 Sticky, and only in the true direction: whether a change crossed a
        // module boundary is a property of its diff. One reviewer noticing it
        // does not stop being true because the next one left the flag off.
        row.crossed_boundary |= crossed_boundary;
        if !row.by.iter().any(|w| w == by) {
            row.by.push(by.to_owned());
        }
    }
}

/// One change, and every review anybody recorded against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reviewed {
    /// Whatever the operator named it — a sha, a task id, a tag. ⚠ **The fold
    /// does not resolve it**: two spellings of one commit are two rows, because
    /// the log holds the string and nothing here can ask the version control
    /// what it meant.
    pub change: String,
    /// Summed over every recording of this change. Seconds, because the record's
    /// unit is seconds — the minutes a person types are converted once, at the
    /// argument edge.
    pub seconds: u64,
    /// Who recorded a review of it, distinct, in first-recording order.
    pub by: Vec<String>,
    /// True if **any** recording said so. See [`Ladder::record`].
    pub crossed_boundary: bool,
    /// How many passes those minutes were.
    pub recordings: u32,
}

/// One change that left a worktree and became a commit on the branch — W13's
/// M1, which is the rung the whole ladder starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landed {
    /// The commit `abcc land` made. ⚠ Compared with [`Reviewed::change`] as a
    /// string, so an operator who reviews a landing must name it the way the
    /// landing printed it.
    pub change: String,
    pub task: TaskId,
    /// The attempt whose gate entitled it to land.
    pub attempt: AttemptId,
    /// The checkpoint pair the change was taken between.
    pub from: String,
    pub to: String,
    /// How many rungs were green when it was measured.
    pub rungs: usize,
    pub seq: Seq,
    pub at_ms: i64,
}

/// One final state, and the attempt endings that reached it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateEndings {
    /// The **first** task's state, kept whole so a caller can put it through a
    /// label map and print the word the board prints.
    ///
    /// ð¨ Grouping is on [`TaskState::name`] and never on the value: every
    /// variant but `Queued` carries a `since`, so two tasks in the same state
    /// are not equal and rows keyed by value would be one row per task.
    pub state: TaskState,
    /// The tasks in this state, in log order â so a reader can go and replay one.
    pub tasks: Vec<TaskId>,
    /// `(ending, count)`, commonest first.
    pub endings: Vec<(String, usize)>,
}

impl StateEndings {
    /// How many attempts this row counted.
    ///
    /// â  Not `tasks.len()`: a task may have run several attempts, and a task
    /// that never started one contributes none.
    #[must_use]
    pub fn attempts(&self) -> usize {
        self.endings.iter().map(|(_, n)| *n).sum()
    }

    /// ð¨ **How many of those endings were [`AttemptOutcome::Uncertain`]** â
    /// the count that says how much of a one-word state is *the system could not
    /// tell* rather than *the work was wrong*.
    #[must_use]
    pub fn uncertain(&self) -> usize {
        self.endings
            .iter()
            .filter(|(l, _)| l.starts_with("Uncertain/"))
            .map(|(_, n)| *n)
            .sum()
    }
}

/// How an attempt ended, as the one string a histogram groups on.
///
/// ⚠ The [`Why`]'s *variant*, never its `Display`: the payloads carry budgets,
/// binaries and detail strings, so displaying them would make every row unique
/// and the histogram would count to one forever.
#[must_use]
pub fn ending(outcome: &AttemptOutcome) -> String {
    let name = outcome.name();
    match outcome {
        AttemptOutcome::Success => name.to_owned(),
        AttemptOutcome::Refused { rung, .. } => format!("{name}/{rung}"),
        AttemptOutcome::SoftFailure { why }
        | AttemptOutcome::HardFailure { why }
        | AttemptOutcome::Uncertain { why } => format!("{name}/{}", why_name(why)),
    }
}

/// The `Why`'s variant name. See [`ending`] for why the payload is dropped.
#[must_use]
pub const fn why_name(why: &Why) -> &'static str {
    match why {
        Why::NoCheckerForArtifact { .. } => "NoCheckerForArtifact",
        Why::CheckerNotOnHost { .. } => "CheckerNotOnHost",
        Why::SpawnFailed { .. } => "SpawnFailed",
        Why::NothingToRun { .. } => "NothingToRun",
        Why::FailedBeforeRunning { .. } => "FailedBeforeRunning",
        Why::Timeout { .. } => "Timeout",
        Why::BudgetExhausted { .. } => "BudgetExhausted",
        Why::Cancelled { .. } => "Cancelled",
        Why::TruncatedAtCap { .. } => "TruncatedAtCap",
        Why::StaleMeasurement { .. } => "StaleMeasurement",
        Why::Denied { .. } => "Denied",
        Why::EngineError { .. } => "EngineError",
        Why::SaidNothing { .. } => "SaidNothing",
        Why::ContextOverflow { .. } => "ContextOverflow",
    }
}

/// One task, and everything that happened to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTrace {
    pub id: TaskId,
    pub mission: MissionId,
    pub title: String,
    pub prompt: String,
    pub created_ms: i64,
    /// The `to` of the last transition. `None` for a task nothing ever moved,
    /// which is [`TaskState::Queued`] by construction and is left as an absence
    /// rather than assumed.
    pub state: Option<TaskState>,
    pub transitions: Vec<Move>,
    pub attempts: Vec<AttemptTrace>,
    /// Questions put to the operator, with their answers where they came.
    pub asked: Vec<Asked>,
}

/// One lifecycle transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub seq: Seq,
    pub at_ms: i64,
    pub command: Command,
    pub from: TaskState,
    pub to: TaskState,
}

/// A question put to a person, and what they said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    pub prompt: PromptId,
    pub question: String,
    /// `None` while it is still owed to somebody.
    pub answer: Option<String>,
}

/// One attempt, and everything it spent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptTrace {
    pub id: AttemptId,
    pub unit: UnitId,
    pub cause: Cause,
    pub checkpoint_from: Option<CheckpointId>,
    pub started: Seq,
    pub started_ms: i64,
    pub ended: Option<Seq>,
    pub ended_ms: Option<i64>,
    /// `None` for an attempt that is still flying, or one a crash abandoned.
    pub outcome: Option<AttemptOutcome>,
    pub phases: Vec<PhaseTrace>,
    /// One row per distinct tool, in first-call order.
    pub tools: Vec<ToolTally>,
    pub spend: Spend,
    /// F511: what the completions were made of.
    pub made: Made,
    /// 🚨 F506: the tool calls of turns that were thrown away, with whatever of
    /// their arguments the log kept. **The log is the only copy** — a turn cut at
    /// the cap never runs its calls, so nothing in the tree will ever say what
    /// was being written.
    pub discarded: Vec<CallShape>,
    pub nudges: u32,
    pub marks: u32,
    pub claims: u32,
    pub rungs: Vec<Outcome>,
    /// The longest silence inside this attempt's span.
    pub quiet: Option<Quiet>,
    /// How many silences ran past [`SILENCE_BAR_MS`].
    pub over_bar: usize,
    /// A phase entered and never ended — where the attempt was when it stopped.
    pub open_phase: Option<AttemptPhase>,
    /// The commit this attempt's work became, if anybody landed it. 🚨 The one
    /// field here written after the attempt ended, and the only evidence on the
    /// log that a green tree was ever taken up.
    pub landed: Option<String>,
}

impl AttemptTrace {
    /// Whether this attempt has an ending. An open one is either in flight or
    /// was abandoned by a crash, and the log cannot tell those apart.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.ended.is_none()
    }

    /// Wall time, when the attempt has both ends.
    #[must_use]
    pub fn elapsed_ms(&self) -> Option<i64> {
        self.ended_ms.map(|e| e - self.started_ms)
    }

    /// Take one of this attempt's events into the tally.
    fn absorb(&mut self, seq: Seq, at_ms: i64, event: &Event) {
        match event {
            Event::AttemptEnded { outcome, .. } => {
                self.ended = Some(seq);
                self.ended_ms = Some(at_ms);
                self.outcome = Some(outcome.clone());
                // A phase still open when the attempt ended is where it stopped.
            }
            Event::AttemptPhaseEntered { phase, .. } => {
                self.open_phase = Some(*phase);
                self.phases.push(PhaseTrace {
                    phase: *phase,
                    entered: seq,
                    entered_ms: at_ms,
                    by: None,
                    turns: 0,
                    tool_calls: 0,
                    denials: 0,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    reasoning_tokens: None,
                    trace: None,
                    elapsed_ms: None,
                });
            }
            Event::PhaseEnded { .. } => self.close_phase(event),
            Event::ModelCallStarted { budget, seed, .. } => {
                self.spend.calls += 1;
                self.spend.budget = self.spend.budget.max(*budget);
                // 🚨 **Zero is *not recorded*, and it is a sentinel rather than
                // an absence** (F718). `seed` is `#[serde(default)]` over a
                // `u32`, a type with no vacant value, so each of the 1,910 model
                // calls logged before F715 replays as `seed: 0` — and counting
                // those as seeded would report the whole archive as pinned to
                // one value, which is the exact inverse of what F715 found.
                //
                // ⚠ The read is a convention, not a proof: `seed_for` maps
                // llama.cpp's *choose your own* sentinel to 0, so a derived seed
                // really can be zero — once in 2^32 calls. At this log's 1,910
                // it has never happened and would take a century of sorties to,
                // but the honest statement is *this attempt recorded no seed*
                // and never *this attempt was unseeded*.
                if *seed != 0 {
                    self.spend.seeds.insert(*seed);
                    self.spend.seeded_calls += 1;
                }
            }
            Event::ModelCallEnded { .. } => self.close_call(event),
            Event::PromptCut {
                reported,
                high_water,
                ..
            } => {
                self.spend.prompt_cuts += 1;
                self.spend.prompt_cut_worst = self
                    .spend
                    .prompt_cut_worst
                    .max(high_water.saturating_sub(*reported));
            }
            Event::ToolCallStarted { tool, tier, .. } => {
                if let Some(t) = self.tools.iter_mut().find(|t| t.tool == *tool) {
                    t.calls += 1;
                } else {
                    self.tools.push(ToolTally {
                        tool: tool.clone(),
                        tier: tier.clone(),
                        calls: 1,
                        failures: 0,
                        unmeasured: 0,
                        elapsed_ms: 0,
                        output_bytes: 0,
                        output_unrecorded: 0,
                    });
                }
            }
            Event::ToolCallEnded {
                tool,
                exit,
                elapsed_ms,
                unmeasured,
                output,
                ..
            } => {
                if let Some(t) = self.tools.iter_mut().find(|t| t.tool == *tool) {
                    t.elapsed_ms += *elapsed_ms;
                    if exit.is_some_and(|e| e != 0) {
                        t.failures += 1;
                    }
                    if unmeasured.is_some() {
                        t.unmeasured += 1;
                    }
                    // F718: the two are counted apart because `None` is *nobody
                    // wrote it down* and `Some("")` is *the tool said nothing*,
                    // and a tally that added the first as a zero would report the
                    // second about 2,147 calls that predate F713.
                    match output {
                        Some(o) => t.output_bytes += o.len() as u64,
                        None => t.output_unrecorded += 1,
                    }
                }
            }
            Event::ChangeLanded { change, .. } => self.landed = Some(change.clone()),
            Event::RungRecorded { outcome, .. } => self.rungs.push(outcome.clone()),
            Event::ClaimRecorded { .. } => self.claims += 1,
            Event::PhaseNudged { .. } => self.nudges += 1,
            Event::LivenessMark { .. } => self.marks += 1,
            _ => {}
        }
    }

    /// 🚨 Pair a [`Event::PhaseEnded`] with the phase it ended.
    ///
    /// The phase is the last [`Event::AttemptPhaseEntered`] before it, and that
    /// pairing happens once, here. `PhaseEnded` deliberately does not repeat the
    /// phase — a second copy is a second thing that can disagree — so a consumer
    /// that does not pair them has no phase at all.
    fn close_phase(&mut self, event: &Event) {
        let Event::PhaseEnded {
            by,
            turns,
            tool_calls,
            denials,
            prompt_tokens,
            completion_tokens,
            reasoning_tokens,
            trace,
            elapsed_ms,
            ..
        } = event
        else {
            return;
        };
        self.open_phase = None;
        let Some(p) = self.phases.iter_mut().rev().find(|p| p.by.is_none()) else {
            return;
        };
        p.by = Some(by.clone());
        p.turns = *turns;
        p.tool_calls = *tool_calls;
        p.denials = *denials;
        p.prompt_tokens = *prompt_tokens;
        p.completion_tokens = *completion_tokens;
        p.reasoning_tokens = *reasoning_tokens;
        p.trace = Some(*trace);
        p.elapsed_ms = Some(*elapsed_ms);
    }

    /// Take one finished model call into [`Spend`] and [`Made`].
    fn close_call(&mut self, event: &Event) {
        let Event::ModelCallEnded {
            usage,
            finish,
            ttfb_ms,
            composition,
            ..
        } = event
        else {
            return;
        };
        self.spend.prompt_tokens += u64::from(usage.prompt_tokens);
        self.spend.completion_tokens += u64::from(usage.completion_tokens);
        if let Some(r) = usage.reasoning_tokens {
            self.spend.reasoning_tokens =
                Some(self.spend.reasoning_tokens.unwrap_or(0) + u64::from(r));
        }
        self.spend.ttfb_ms_max = self.spend.ttfb_ms_max.max(*ttfb_ms);
        let name = finish_name(finish);
        if let Some(f) = self.spend.finishes.iter_mut().find(|(f, _)| *f == name) {
            f.1 += 1;
        } else {
            self.spend.finishes.push((name, 1));
        }
        let Some(c) = composition else { return };
        self.made.counted += 1;
        self.made.text_chars += u64::from(c.text_chars);
        self.made.reasoning_chars += u64::from(c.reasoning_chars);
        for call in &c.calls {
            self.made.argument_chars += u64::from(call.argument_chars);
            if call.arguments.is_some() {
                self.discarded.push(call.clone());
            }
        }
    }

    /// The silences inside this attempt's span.
    ///
    /// ⚠ Over **every** event in `[started, ended]` rather than only this
    /// attempt's own, because during an attempt the driver is the log's writer:
    /// a checkpoint or a worktree event is this attempt's process working, and
    /// excluding it would invent silence that did not happen.
    fn scan_silence(&mut self, log: &[Logged]) {
        let last = self.ended.unwrap_or(Seq::new(i64::MAX));
        let mut prev: Option<&Logged> = None;
        for logged in log
            .iter()
            .filter(|l| l.seq >= self.started && l.seq <= last)
        {
            if let Some(p) = prev {
                let gap = logged.at_ms - p.at_ms;
                if gap > SILENCE_BAR_MS {
                    self.over_bar += 1;
                }
                if self.quiet.as_ref().is_none_or(|q| gap > q.gap_ms) {
                    self.quiet = Some(Quiet {
                        gap_ms: gap,
                        after: p.event.kind(),
                        broken_by: logged.event.kind(),
                        at: p.seq,
                    });
                }
            }
            prev = Some(logged);
        }
    }
}

/// One phase of an attempt, entered and — usually — ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseTrace {
    pub phase: AttemptPhase,
    pub entered: Seq,
    pub entered_ms: i64,
    /// The head's call sign. `None` for a phase that never ended.
    pub by: Option<String>,
    pub turns: u32,
    pub tool_calls: u32,
    pub denials: u32,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// `None` when no call in the phase reported one, which is not zero.
    pub reasoning_tokens: Option<u32>,
    pub trace: Option<TraceSignal>,
    pub elapsed_ms: Option<u64>,
}

impl PhaseTrace {
    /// Whether this phase ever ended.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.by.is_none()
    }
}

/// What an attempt asked the model for, and what it got back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spend {
    pub calls: u32,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// `None` when nothing counted any, which is different from zero.
    pub reasoning_tokens: Option<u64>,
    /// The widest ceiling any call in this attempt declared.
    pub budget: u32,
    pub ttfb_ms_max: u64,
    /// `(finish, count)`, in first-seen order.
    pub finishes: Vec<(&'static str, usize)>,
    /// 🚨 **F718: every distinct seed this attempt drew, and it is a set on
    /// purpose.**
    ///
    /// F715 derives a call's seed from `(attempt, head digest, round)`, and
    /// `round` restarts at zero with each phase — so two phases of one attempt
    /// that posted the *same* head would repeat the whole triple and hand two
    /// different prompts one sampler draw. Nothing enforces that they cannot:
    /// it holds because the roster gives each phase its own head, which is a
    /// property of the charter and not of the derivation. Over 96 attempts and
    /// 1,910 model calls on this project's log a head has never been posted in
    /// two phases of one attempt, so the collision has never happened — and
    /// `seeds.len()` against [`Spend::seeded_calls`] is the reading that would
    /// notice the day it does.
    pub seeds: BTreeSet<u32>,
    /// How many calls carried a seed at all. ⚠ Zero over a populated attempt
    /// means the events predate F715, not that the sampler was pinned to one
    /// value — and an attempt flown *unseeded* is the state every rate this
    /// project has published was measured in.
    pub seeded_calls: u32,
    /// 🚨 **F748: how many of this attempt's calls the server cut the prompt
    /// of**, folded from [`Event::PromptCut`].
    ///
    /// ⚠ **Zero means no cut was *recorded*, which over an old attempt means
    /// nobody was looking** — the same reading [`Spend::seeded_calls`] needs, and
    /// for the same reason. The detector arrived with F748; on the 1,977 model
    /// calls logged before it, **74 were shown a cut prompt** (F749) and not one
    /// of them can say so.
    pub prompt_cuts: u32,
    /// The deepest single cut: the largest `high_water - reported` seen.
    ///
    /// ⚠ **A floor, never the amount.** Everything appended to the body since the
    /// high-water call is missing from that prompt too, and nothing measures that
    /// part.
    pub prompt_cut_worst: u32,
}

/// 🚨 **F511: where the completion went**, which a token total cannot say.
///
/// A turn at the cap with 8,192 completion tokens and ~400 of them reasoning
/// looks like 7,700 tokens of prose in the usage block; in five live turns it
/// was 7,700 tokens of **one tool call's arguments**, the opposite failure with
/// the opposite fix.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Made {
    /// How many calls reported a composition at all. ⚠ Zero over a populated
    /// attempt means the events predate the field, not that nothing was made.
    pub counted: u32,
    pub text_chars: u64,
    pub reasoning_chars: u64,
    pub argument_chars: u64,
}

impl Made {
    /// Every character the composition counted.
    ///
    /// 🚨 **Compare it against [`Spend::completion_tokens`], always.** On this
    /// project's log all three full-cap truncations account for **1.4-1.9% of
    /// the tokens the server billed** at this stack's documented ~4 chars per
    /// token, and in each the single call is an `apply_patch` whose
    /// `argument_chars` is **zero** — the field F511 added to answer *where did
    /// the completion go* reading nothing on the case it was written for. A
    /// composition presented without the total it is a fraction of is the same
    /// mistake F511 was fixing one level down.
    #[must_use]
    pub const fn chars(&self) -> u64 {
        self.text_chars + self.reasoning_chars + self.argument_chars
    }

    /// The share of the produced characters that were the trace.
    ///
    /// `None` when nothing was counted — the absence, never a zero.
    #[must_use]
    pub fn reasoning_share(&self) -> Option<f64> {
        let total = self.text_chars + self.reasoning_chars + self.argument_chars;
        if self.counted == 0 || total == 0 {
            return None;
        }
        // The counts are character totals of one attempt; f64 holds them exactly
        // far past any completion this stack can produce.
        #[allow(clippy::cast_precision_loss)]
        Some(self.reasoning_chars as f64 / total as f64)
    }
}

/// One tool, over one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTally {
    pub tool: String,
    /// The tier it was admitted at (ADR-0014: the class is the control).
    pub tier: String,
    pub calls: u32,
    /// Calls that ended with a non-zero exit.
    pub failures: u32,
    /// Calls that produced no measurable ending at all.
    pub unmeasured: u32,
    pub elapsed_ms: u64,
    /// 🚨 **F718: how many bytes this tool handed the model**, summed over
    /// the attempt. F713 put a tool's output on the log because it is the
    /// prompt surface nothing could read; this is the only number that says
    /// what that surface *weighed*, and F717 is the reason it is worth saying —
    /// the field roughly quadruples an attempt's record.
    pub output_bytes: u64,
    /// Calls whose output was never written down. ⚠ The distinction is the
    /// point: `output` is `serde(default)`, so a call logged before F713 reads
    /// back as `None`, and folding that into `output_bytes` as a zero would
    /// report *this tool returned nothing* about a call nobody recorded.
    pub output_unrecorded: u32,
}

/// The longest silence inside an attempt, and what ended it.
///
/// 🚨 `broken_by` is the field that matters. [`Event::LivenessMark`] is written
/// from inside the stream's read loop, so a mark closing a long gap means the
/// stream was slow and the run knew it — while any *other* event closing one
/// means nothing was watching for that whole stretch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quiet {
    pub gap_ms: i64,
    /// The event the silence began after, as [`Event::kind`] names it.
    ///
    /// ⚠ `Event::kind` and not a second match over `Event`: the log's own
    /// serde tag is already an exhaustive naming of every variant, and a private
    /// copy of it here would be a second thing to keep in step with the first.
    pub after: &'static str,
    /// The event that ended it, same naming.
    pub broken_by: &'static str,
    /// Where the silence began, so a reader can scrub to it.
    pub at: Seq,
}

impl Quiet {
    /// Whether a liveness mark closed this silence — the run saying *still
    /// here* — as opposed to the next piece of work simply arriving.
    #[must_use]
    pub fn was_marked(&self) -> bool {
        self.broken_by == "liveness_mark"
    }
}

/// The attempt an event belongs to, for the events that name one.
///
/// ⚠ Not a wildcard: every variant is listed, so a new event carrying an
/// `attempt` does not silently fall out of every after-action view. This one has
/// to be a match — it reads a *field*, which [`Event::kind`] cannot give it.
const fn attempt_of(event: &Event) -> Option<AttemptId> {
    match event {
        Event::AttemptEnded { attempt, .. }
        | Event::AttemptPhaseEntered { attempt, .. }
        | Event::BriefRecorded { attempt, .. }
        | Event::ModelCallStarted { attempt, .. }
        | Event::ModelCallEnded { attempt, .. }
        | Event::PromptCut { attempt, .. }
        | Event::PhaseEnded { attempt, .. }
        | Event::ToolCallStarted { attempt, .. }
        | Event::ToolCallEnded { attempt, .. }
        | Event::RungRecorded { attempt, .. }
        | Event::ClaimRecorded { attempt, .. }
        | Event::LivenessMark { attempt, .. }
        | Event::PhaseNudged { attempt, .. }
        | Event::OperatorPrompted { attempt, .. }
        // ⚠ **After the attempt's span, and deliberately still filed under it.**
        // Every other event here happened between `AttemptStarted` and
        // `AttemptEnded`; a landing happens whenever a person gets to it. It is
        // listed because the question this function answers is *which attempt is
        // this about*, and the answer is not `None` — a reading that wants only
        // the flight has `AttemptTrace::ended` to bound it with.
        | Event::ChangeLanded { attempt, .. } => Some(*attempt),
        Event::RunStarted { .. }
        | Event::ModeDowngraded { .. }
        | Event::MissionCreated { .. }
        | Event::TaskCreated { .. }
        | Event::TaskDependsOn { .. }
        | Event::MissionPhaseEntered { .. }
        | Event::TaskTransitioned { .. }
        | Event::CommandRefused { .. }
        | Event::AttemptStarted { .. }
        | Event::CheckpointTaken { .. }
        | Event::WorktreeOpened { .. }
        | Event::WorktreeClosed { .. }
        | Event::OperatorAnswered { .. }
        | Event::ControlRequested { .. }
        | Event::ControlApplied { .. }
        | Event::ReviewRecorded { .. }
        // The weights are checked before the first attempt is opened — it is a
        // fact about the run, not about an attempt.
        | Event::WeightsChecked { .. }
        | Event::Note { .. } => None,
    }
}

/// A finish reason's name, for the per-attempt histogram.
///
/// 🚨 **`length` splits on `content_empty`, because that bit is the whole
/// diagnostic.** A turn cut at the cap having produced nothing is F506's case —
/// none of its tool calls run, and both of this project's contentless HTTP 500s
/// came from appending one. A turn cut at the cap that *did* produce something
/// is a long answer. The usage block cannot tell them apart and this can.
const fn finish_name(finish: &crate::event::Finish) -> &'static str {
    use crate::event::Finish;
    match finish {
        Finish::Stop => "stop",
        Finish::Length {
            content_empty: true,
        } => "length/empty",
        Finish::Length {
            content_empty: false,
        } => "length",
        Finish::ToolCalls => "tool_calls",
        Finish::Truncated { .. } => "truncated",
    }
}
