//! The control point: where an operator's verb reaches a running attempt.
//!
//! ADR-0012 §4 reduces eight console verbs to three mechanisms, and this is the
//! first of them — **a control channel with a step boundary the worker checks**.
//! The unit of control is the *task*, because the unit of control must be the
//! unit that holds resources: every verb ends in *"and then what happens to its
//! model slot and its workspace lock?"*, and only a task can answer that.
//!
//! # Two sampling points, because the verbs have two urgencies
//!
//! [`Control::Pause`] means *stop at the next step boundary*; [`Control::Halt`],
//! [`Control::Kill`] and [`Control::Redirect`] mean *stop now*. So there are two
//! places a worker looks:
//!
//! * [`ControlPoint::check`] at a step boundary — between tool rounds, between
//!   phases — which is where a `Pause` is honoured.
//! * [`ControlPoint::interrupted`] between stream deltas, which is one atomic
//!   load and is where the urgent verbs are seen. **The sampling interval is one
//!   SSE line — 13–18 ms at measured decode rates.** Against an operator's
//!   reaction time and a 23.77 s model swap, that interval does not exist.
//!
//! 🚨 This is the residue ADR-0006 states honestly rather than hides: `select!`
//! would let a worker wait on the stream and the channel *simultaneously*, and
//! the thread design samples the channel between lines instead. That difference
//! is what an all-tokio core would have bought, and it is 13–18 ms.
//!
//! # Stopping is a socket close
//!
//! Nothing here reaches into a stream. A worker that sees [`ControlPoint::
//! interrupted`] **drops the stream**, and the socket closes with it — measured
//! at 4 ms and 14 ms to stop a live generation, with the server observing the
//! dead socket one write cadence later (F200). The cancellation argument for a
//! runtime was never a runtime capability.
//!
//! # This channel is not the record
//!
//! ⚠ ADR-0006: the console writes a `ControlRequested` row **first** and pokes
//! this channel second. **The poke is a latency optimisation and never the
//! truth** — boot replay is the backstop, so a verb that reaches the log and not
//! the channel is still honoured, and one that reaches the channel and not the
//! log never happened.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use abcc_core::event::Control;

/// How soon a verb takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    /// Honoured at the next step boundary, so the work in flight completes.
    AtStepBoundary,
    /// Honoured between stream deltas: the worker drops the stream and the
    /// socket closes.
    Now,
}

/// What a stop does with the work.
///
/// 🚨 [`Control::Pause`] and [`Control::Halt`] produce the *same* disposition and
/// differ only in [`Urgency`]. That is the honest shape: the operator's choice is
/// about when, not about what is kept, and giving them two dispositions would
/// invent a distinction the verbs do not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keep {
    /// Checkpoint the worktree and go to `Holding`. The slot is released; the
    /// workspace is kept.
    AtCheckpoint,
    /// Nothing survives. The task goes to `Aborted`.
    Nothing,
    /// Checkpoint, then fork a new attempt carrying this prompt — because
    /// attempts are immutable, so a redirect is a fork with a [`abcc_core::
    /// attempt::Cause::Edit`] rather than an edit of the attempt in flight.
    AndFork { prompt: String },
}

/// A stop the worker has accepted, and what it owes the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    /// The verb, verbatim, so the driver can write `ControlApplied` with it.
    pub control: Control,
    pub keep: Keep,
}

/// What a worker should do at a sampling point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Carry on.
    Carry,
    Stop(Stop),
}

/// The console's end of the channel. Cheap to clone and safe to hold anywhere.
#[derive(Debug, Clone)]
pub struct ControlHandle {
    tx: Sender<Control>,
    urgent: Arc<AtomicBool>,
}

impl ControlHandle {
    /// Poke the worker.
    ///
    /// 🚨 The verb goes on the **channel first** and the flag second. A worker
    /// samples the flag far more often than the channel, so the reverse order
    /// admits a wake with nothing to read — which would read as a spurious
    /// interrupt rather than as the verb it was.
    ///
    /// # Errors
    ///
    /// The worker is gone. That is not a fault: an attempt that ended before the
    /// verb arrived is the ordinary race, and the durable `ControlRequested` row
    /// is what makes it recoverable.
    pub fn request(&self, control: Control) -> Result<(), Gone> {
        let urgency = urgency(&control);
        self.tx.send(control).map_err(|_| Gone)?;
        if urgency == Urgency::Now {
            self.urgent.store(true, Ordering::Release);
        }
        Ok(())
    }
}

/// The worker has finished; there is nobody to poke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the attempt this control was for is no longer running")]
pub struct Gone;

/// The worker's end. Owned by the thread running the attempt, and never shared.
#[derive(Debug)]
pub struct ControlPoint {
    rx: Receiver<Control>,
    urgent: Arc<AtomicBool>,
    /// The first stop wins and stays won. A second verb arriving while the
    /// worker unwinds must not change what it is unwinding into.
    latched: Option<Stop>,
}

impl ControlPoint {
    /// A fresh channel and the handle that pokes it.
    #[must_use]
    pub fn new() -> (ControlPoint, ControlHandle) {
        let (tx, rx) = mpsc::channel();
        let urgent = Arc::new(AtomicBool::new(false));
        (
            ControlPoint {
                rx,
                urgent: Arc::clone(&urgent),
                latched: None,
            },
            ControlHandle { tx, urgent },
        )
    }

    /// One atomic load, for between stream deltas.
    ///
    /// It answers *is there an urgent verb waiting*, not *which one*: the worker
    /// drops the stream first and asks [`ControlPoint::check`] second, because
    /// closing the socket is the thing with a deadline and reading the verb is
    /// not.
    #[must_use]
    pub fn interrupted(&self) -> bool {
        self.urgent.load(Ordering::Acquire)
    }

    /// The step boundary. Drains everything waiting and latches the first stop.
    ///
    /// [`Control::Resume`] is not a stop and is ignored here — it belongs to a
    /// task in `Holding`, which by definition has no worker to receive it.
    pub fn check(&mut self) -> Disposition {
        // A disconnected channel and an empty one are the same answer here: the
        // console is gone, or it has said nothing. Neither is a stop.
        while let Ok(control) = self.rx.try_recv() {
            if self.latched.is_none()
                && let Some(keep) = disposition(&control)
            {
                self.latched = Some(Stop { control, keep });
            }
        }
        match &self.latched {
            Some(stop) => Disposition::Stop(stop.clone()),
            None => Disposition::Carry,
        }
    }

    /// The stop this point has latched, if any. Idempotent, so a worker unwinding
    /// through several boundaries reads one answer.
    #[must_use]
    pub fn latched(&self) -> Option<&Stop> {
        self.latched.as_ref()
    }
}

/// How soon a verb takes effect. Free-standing so the sender can classify without
/// owning a control point.
#[must_use]
pub fn urgency(control: &Control) -> Urgency {
    match control {
        Control::Pause | Control::Resume => Urgency::AtStepBoundary,
        Control::Halt | Control::Kill | Control::Redirect { .. } => Urgency::Now,
    }
}

/// What a verb does with the work, or `None` if it is not a stop.
fn disposition(control: &Control) -> Option<Keep> {
    match control {
        // Same disposition, different urgency. The operator chose when, not what.
        Control::Pause | Control::Halt => Some(Keep::AtCheckpoint),
        Control::Kill => Some(Keep::Nothing),
        Control::Redirect { prompt } => Some(Keep::AndFork {
            prompt: prompt.clone(),
        }),
        Control::Resume => None,
    }
}
