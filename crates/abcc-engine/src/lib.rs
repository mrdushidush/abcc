//! The engine: the turn loop, the seam it talks to a model through, and the tool
//! layer that decides what a role may reach for.
//!
//! [`abcc-core`](../abcc_core/index.html) holds the words, `abcc-store` writes
//! them down, `abcc-vcs` gives an attempt somewhere to work — and this crate is
//! the thing that actually runs. It is also the crate the whole plan rests on:
//! ADR-0001 ruled **rewrite, do not port**, which turned 93 modules and 65,519
//! lines of working donor engine from a source tree into a specification and a
//! test corpus. The falsifier that ruling created is *the rewrite does not
//! re-earn the engine's working behaviour*, and this is where it is answered.
//!
//! Four rules govern the crate, and each is a Phase 1 measurement rather than a
//! preference.
//!
//! 1. **The role carries the ceiling, and the class is denied rather than
//!    argued with.** An argument check binds one tool and a shell walks past it —
//!    demonstrated four times by four mechanisms across three authors — and a
//!    prompt binds only as far as the model complies, measured at 39 of 50. So
//!    [`tools::Policy`] is the enforcement and everything else is a backstop that
//!    says so in its own doc comment (ADR-0014).
//! 2. **The system prefix cannot vary.** [`Posting::prefix`] has no per-call
//!    input — a posting is a head and a four-valued ceiling, and the ceiling is
//!    operator configuration fixed for a sortie — because the prefix cache saves
//!    79.7% of TTFT and one token changed at the front annihilates it (F81).
//!    Failure context is *appended*, which is why [`provider::Body`] has no
//!    operation but `append` (ADR-0010, ADR-0011).
//! 3. **Cancellation is a socket close.** [`provider::TurnStream`] has no
//!    `cancel()`: the worker samples its control channel between deltas and drops
//!    the stream, which stopped a live generation at 4–14 ms (F200). This is why
//!    threads own the work and one runtime sits at the console edge, rather than
//!    every function between `main` and the socket taking a colour (ADR-0006).
//! 4. **What the model says is never what the host measured.** A turn produces a
//!    [`abcc_core::outcome::Claim`]; only a checker the host watched produces an
//!    [`abcc_core::outcome::Outcome`], and there is no function anywhere that
//!    converts one into the other (ADR-0009).
//!
//! The RTS call-signs on [`Head`] are the same load-bearing vocabulary as the
//! lifecycle names: Engineering, Recon, Builders and Commandos are §10's four
//! unit names surviving as *call-signs on phases*, which is the one part of that
//! table the reconciliation kept.

pub mod child;
pub mod control;
pub mod edit;
pub mod evict;
pub mod head;
pub mod openai;
pub mod patch;
pub mod provider;
pub mod scripted;
pub mod tools;
pub mod turn;
pub mod workspace;

pub use child::{Finished, Killer, Spawn, ToolChild};
pub use control::{ControlHandle, ControlPoint, Disposition, Gone, Keep, Stop, Urgency, Watch};
pub use head::{Head, Posting, Serves};
pub use openai::OpenAiCompat;
pub use provider::{
    ApiRequest, Body, Delta, Message, Provider, ProviderClass, ProviderError, ProviderId, Role,
    Schema, Temperature, ToolCall, TraceSignal, Turn, TurnStream,
};
pub use tools::{Confinement, Denied, Destructive, Policy, Reach, Tier, ToolSpec, UnknownTier};
pub use turn::{Journal, Limits, NoTools, PhaseEnded, PhaseReport, ToolResult, Tools, TurnLoop};
pub use workspace::{Standard, Toolchain, Workspace};
