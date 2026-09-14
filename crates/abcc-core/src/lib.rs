//! ABCC 2.0's domain vocabulary.
//!
//! This crate holds the words and nothing else: no I/O, no runtime, no model.
//! [`abcc-store`](../abcc_store/index.html) writes them down, the engine produces
//! them, and the console reads them back.
//!
//! Four rules govern everything in here, and each of them exists because Phase 1
//! measured what happens without it.
//!
//! 1. **One vocabulary per entity, never a mega-enum.** A task's lifecycle, an
//!    attempt's outcome, a phase and a mode are four types. v1's eleven-state
//!    count was the damage done by carrying scheduling, pipeline position and
//!    outcome in one enum (W3 F146).
//! 2. **Every variant carries the evidence its contract needs.** A state cannot
//!    be constructed without the clock its watchdog reads, so v1's two NULL-clock
//!    liveness bugs are not fixed here — they are unrepresentable.
//! 3. **There is no `bool` in the data.** [`outcome::Headline::is_pass`] is the
//!    only function in the workspace that produces one, and it is deliberately
//!    not named `is_ok`.
//! 4. **A model verdict is a report and never a gate.** Only deterministic rungs
//!    may refuse — see [`run::AttemptPhase::may_refuse`], where the ruling is
//!    written as code so that violating it is an edit somebody can see.
//!
//! The RTS naming is load-bearing rather than decorative. W9 scanned 340
//! comparable projects and found game, RTS, battle, isometric and sprite
//! vocabulary in exactly none of them, which makes the framing the only
//! differentiator the evidence supports — so it is the last thing to cut, and a
//! theme is a label map over these fixed names rather than a rename of them.

pub mod attempt;
pub mod climb;
pub mod event;
pub mod fun;
pub mod outcome;
pub mod redact;
pub mod replay;
pub mod run;
pub mod seq;
pub mod task;

pub use attempt::{Attempt, AttemptOutcome, Cause, NextAction};
pub use climb::{Climb, Direction, Slope, Trend};
pub use event::{CallShape, Composition, Control, Event, Finish, Logged, TraceSignal, Usage};
pub use fun::{Answer, Fun, Missing, Verdict};
pub use outcome::{Claim, Counts, Headline, Measurement, Outcome, Report, Why};
pub use replay::{AttemptTrace, PhaseTrace, Quiet, Replay, TaskTrace};
pub use run::{AttemptPhase, DowngradeReason, MissionPhase, Mode};
pub use seq::{AttemptId, CheckpointId, MissionId, PromptId, Seq, TaskId, UnitId};
pub use task::{
    AbortReason, BootAction, Bound, Command, Liveness, Refused, RequeueReason, StateContract,
    TaskState, Watchdog,
};
