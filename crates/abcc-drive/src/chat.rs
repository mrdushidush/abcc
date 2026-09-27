//! `abcc chat`'s half of the drive: one attempt as a conversation with the
//! operator (PLAN-TOOL Phase C1).
//!
//! The lifecycle is [`Driver::run`]'s, step for step: onto a slot, a worktree at
//! the cause's checkpoint, `AttemptStarted`, `Engage`, and [`Driver::land`] at
//! the end, so the gate, the Judge, `board`, `diff`, `land` and `replay` all work
//! on a chat exactly as on a batch attempt. What differs is the middle:
//!
//! * **One conversation under Builders**, not Recon then Builders. The body is
//!   the caller's and outlives the attempt, so a chat that `/done` sends through
//!   a red gate carries on in a `Retry` with everything it already read.
//! * **The operator speaks between turns.** Each line is scrubbed, logged as
//!   [`Event::OperatorSaid`] and appended to the body; then the turn loop runs
//!   until the model answers.
//! * **Each turn gets its own [`ControlPoint`].** A control point latches its
//!   first stop for good, so one shared across the chat would end the chat at
//!   the first Ctrl-C. Per turn, an interrupt stops that turn and the operator
//!   types the redirect. ⚠ A tool child already running (a `cargo test`) watches
//!   the attempt's own point and finishes first.

use abcc_core::attempt::Cause;
use abcc_core::event::{Control, Event};
use abcc_core::run::AttemptPhase;
use abcc_core::seq::{AttemptId, TaskId, UnitId};
use abcc_core::task::{Command, TaskState};
use abcc_engine::control::{ControlHandle, ControlPoint, Keep, Stop};
use abcc_engine::head::Head;
use abcc_engine::provider::{Body, Delta, Message};
use abcc_engine::turn::{Journal, PhaseEnded, TurnLoop};

use crate::{Driver, Landed, Opened, Result, StoreJournal, brief};

/// What the operator said at the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// Words for the model.
    Ask(String),
    /// Show what this attempt has changed so far.
    Diff,
    /// Run the checks and end the attempt.
    Done,
    /// Stop and keep the work; the task stays on the board, holding.
    Quit,
}

/// The person on the other end. The drive owns the lifecycle and the log; this
/// owns the terminal.
pub trait Operator {
    /// The next thing the operator says. End of input should be [`Said::Quit`].
    fn next(&mut self) -> Said;
    /// A turn is about to run, and this is the handle that interrupts it.
    fn turn_starting(&mut self, interrupt: ControlHandle);
    /// Every event the turn writes, as it is written.
    fn event(&mut self, event: &Event);
    /// The turn is over.
    fn turn_ended(&mut self, ended: &PhaseEnded);
    /// What `/diff` asked for.
    fn diff(&mut self, patch: &str);
    /// Anything else worth saying.
    fn note(&mut self, text: &str);
}

/// Writes to the log and shows the operator the same event.
struct Tee<'s, 'o> {
    store: StoreJournal<'s>,
    operator: &'o mut dyn Operator,
}

impl Journal for Tee<'_, '_> {
    fn record(&mut self, event: Event) {
        self.operator.event(&event);
        self.store.record(event);
    }
}

impl Driver<'_> {
    /// Run one attempt of `task` as a conversation, and land it when the
    /// operator says `/done` or `/quit`.
    ///
    /// `body` is the conversation. Empty, it is opened with the chat brief and
    /// the model starts on the task at once; carried over from an attempt the
    /// gate refused, it gets that refusal and the operator speaks first.
    ///
    /// # Errors
    ///
    /// [`crate::DriveError`] — the log, git, or a lifecycle command refused.
    pub fn chat(
        &mut self,
        task: TaskId,
        unit: UnitId,
        cause: Cause,
        body: &mut Body,
        operator: &mut dyn Operator,
        deltas: &dyn Fn(&Delta),
    ) -> Result<Landed> {
        let row = self.row(task)?;
        let onto_slot = match row.state {
            TaskState::Holding { .. } => Command::Resume { unit },
            _ => Command::Deploy { unit },
        };
        self.command(task, onto_slot)?;

        let (mut control, _attempt_handle) = ControlPoint::new();
        let opened = self.open_workspace(&row, &cause, control.watch())?;
        let started = self.store.append(Event::AttemptStarted {
            task,
            unit,
            cause,
            checkpoint_from: Some(opened.opening.id),
        })?;
        let attempt = AttemptId::at(started.seq);
        self.command(task, Command::Engage { attempt })?;
        self.store.append(Event::AttemptPhaseEntered {
            attempt,
            phase: AttemptPhase::Change,
        })?;

        let mut last: Option<PhaseEnded> = None;
        if body.is_empty() {
            let brief = self.secrets.scrub(brief::chat(&row));
            *body = Body::opening(brief.text.as_str());
            self.store.append(Event::BriefRecorded {
                attempt,
                text: brief.text,
            })?;
            last = self.chat_turn(attempt, &opened, body, operator, deltas)?;
        } else if let Some(refusal) = self.refusal_under(&opened)? {
            let said = self.secrets.scrub(brief::chat_refused(&refusal));
            body.append(Message::user(said.text.as_str()));
            self.store.append(Event::OperatorSaid {
                attempt,
                text: said.text,
            })?;
            operator.note(&format!(
                "the {} check refused the last tree; that is now in the conversation",
                refusal.rung
            ));
        }

        loop {
            match operator.next() {
                Said::Ask(text) => {
                    let said = self.secrets.scrub(text);
                    body.append(Message::user(said.text.as_str()));
                    self.store.append(Event::OperatorSaid {
                        attempt,
                        text: said.text,
                    })?;
                    if let Some(ended) = self.chat_turn(attempt, &opened, body, operator, deltas)? {
                        last = Some(ended);
                    }
                }
                Said::Diff => {
                    let now = self.checkpoint_worktree(&row, &opened.worktree)?;
                    let patch = self.repo.patch_between(&opened.opening.sha, &now.sha)?;
                    operator.diff(&patch);
                }
                Said::Done => {
                    let Some(ended) = last.take() else {
                        operator.note("nothing to check yet: no turn has finished");
                        continue;
                    };
                    return self.land(&row, attempt, opened, &ended, None, &mut control);
                }
                Said::Quit => {
                    let report = last
                        .as_ref()
                        .map(|e| e.report().clone())
                        .unwrap_or_default();
                    let paused = PhaseEnded::Stopped {
                        stop: Stop {
                            control: Control::Pause,
                            keep: Keep::AtCheckpoint,
                        },
                        report,
                    };
                    return self.land(&row, attempt, opened, &paused, None, &mut control);
                }
            }
        }
    }

    /// The operator, at the chat, says *keep going* to a task a check handed
    /// back to them.
    ///
    /// A refused tree lands `AwaitingOrders`, whose exits are all the
    /// operator's (F732). The one used here is `Hold` at the task's latest
    /// checkpoint, answering the standing question on the log first; the next
    /// [`Driver::chat`] then resumes from that tree. A task in any other state
    /// is left alone.
    ///
    /// # Errors
    ///
    /// [`crate::DriveError`] — the log, or a refused command.
    pub fn keep_going(&mut self, task: TaskId, answer: &str) -> Result<TaskState> {
        let row = self.row(task)?;
        let TaskState::AwaitingOrders { prompt, .. } = row.state else {
            return Ok(row.state);
        };
        let history = self.store.task_history(task)?;
        let Some(checkpoint) = crate::last_checkpoint(&history) else {
            return Ok(row.state);
        };
        self.store.append(Event::OperatorAnswered {
            task,
            prompt,
            answer: answer.to_owned(),
        })?;
        self.command(task, Command::Hold { checkpoint })
    }

    /// One turn of the conversation: the loop runs until the model answers, or
    /// runs out, or is interrupted. `None` for an interrupted turn, which does
    /// not replace the last ending `/done` would check.
    fn chat_turn(
        &mut self,
        attempt: AttemptId,
        opened: &Opened,
        body: &mut Body,
        operator: &mut dyn Operator,
        deltas: &dyn Fn(&Delta),
    ) -> Result<Option<PhaseEnded>> {
        let (mut turn, interrupt) = ControlPoint::new();
        operator.turn_starting(interrupt);

        let provider = self.provider;
        let model = self.model.clone();
        let (limits, ceiling, secrets) = (self.limits, self.ceiling, self.secrets.clone());
        let mut journal = Tee {
            store: StoreJournal::new(self.store),
            operator: &mut *operator,
        };
        let ended = TurnLoop::new(provider, &opened.workspace, model)
            .limits(limits)
            .ceiling(ceiling)
            .secrets(secrets)
            .watch(deltas)
            .run(Head::Builders, attempt, None, body, &mut turn, &mut journal);
        journal.store.into_result()?;
        if let PhaseEnded::Stopped { .. } = &ended {
            self.store.append(Event::Note {
                text: format!("{attempt} the operator stopped the turn; the chat goes on"),
            })?;
        }

        operator.turn_ended(&ended);
        Ok(match ended {
            PhaseEnded::Stopped { .. } => None,
            other => Some(other),
        })
    }
}
