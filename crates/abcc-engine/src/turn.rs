//! The turn loop: one phase, one frozen head, N rounds of tools, one artifact.
//!
//! This is the function ADR-0001's falsifier is about. The donor engine is 93
//! modules and 65,519 lines and it works; the ruling made it a **specification
//! and a test corpus** rather than a source tree, and *"the rewrite does not
//! re-earn the engine's working behaviour"* is the observation that would
//! overturn it. So the loop is written against the contracts and not against the
//! code, and every rule it holds is one a measurement produced:
//!
//! * **The head never moves.** [`Posting::prefix`] is `&'static str` and the loop
//!   appends to a [`Body`] — the same body, growing — so every round after the
//!   first is a prefix-cache hit rather than a cold prefill. One token changed at
//!   the front costs the whole 79.7% saving (F81).
//! * **A refused tool is told to the model, in its own transcript.** The denial
//!   goes on the log as [`Why::Denied`] and into the body as that call's result,
//!   because a model that cannot see the refusal asks again, and asking again is
//!   the round budget spent on nothing.
//! * **Silence is marked.** A stream that says nothing for
//!   [`Limits::liveness_gap`] gets an [`Event::LivenessMark`]. The harness this
//!   project came from once sat silent for forty minutes — 240× the attention
//!   limit — and could not tell that from a hang (ADR-0012 §5).
//! * **An empty payload at the cap is an absence.** `finish_reason == "length"`
//!   with nothing in it is [`Why::TruncatedAtCap`], never a verdict and never a
//!   zero; 17 of 57 judge calls were lost that way.
//! * **Stopping is dropping.** The loop samples [`ControlPoint::interrupted`]
//!   between deltas and drops the stream, which closes the socket (F200). No
//!   cancel token reaches into the provider.

use std::time::{Duration, Instant};

use abcc_core::event::{Event, Finish, Usage};
use abcc_core::outcome::{Claim, Why};
use abcc_core::redact::{Scrubbed, Secrets};
use abcc_core::seq::AttemptId;

use crate::control::{ControlPoint, Disposition, Stop};
use crate::head::{Head, Posting};
use crate::provider::{
    ApiRequest, Body, Delta, Message, Provider, ProviderError, Role, Schema, ToolCall, TraceSignal,
    Turn, TurnStream,
};
use crate::tools::{Reach, Tier, ToolSpec};

/// What a phase says to a model that answered with nothing (F503).
///
/// 🚨 **It names the mechanism rather than scolding.** The observed failure is
/// not refusal or confusion — the model reasons to the end and emits five to
/// nine tokens that trim to an empty string, having apparently treated the
/// thinking as the deliverable. So the sentence that matters is *the reasoning
/// is not visible and the reply is*.
///
/// ⚠ It goes on the **end** of the body, never into the head. One token changed
/// at the front costs the whole 79.7% prefix-cache saving (F81), which is the
/// same reason [`crate::head::Posting::prefix`] carries no per-call input.
const NO_ANSWER: &str = "Your last turn produced no reply text at all: the reasoning ended and \
                         nothing was said. Only the reply is visible to anyone — the reasoning \
                         is not, and it is not kept. Say the answer now, in the reply itself.";

// ---------------------------------------------------------------------------
// The re-read guard
// ---------------------------------------------------------------------------

/// The first line of every substituted duplicate result, and the string the
/// occurrence count below is taken over.
///
/// It is a header rather than prose so that the model can find the earlier copy
/// by searching its own transcript for the same sixteen characters, and so the
/// count has something to match on that does not move when the wording of the
/// note does.
const DUPLICATE_HEADER: &str = "abcc: duplicate tool result";

/// Every Nth repeat of one identical result is served whole anyway.
///
/// 🚨 **A design guess with no rate behind it, and it is written down as one.**
/// [`Body`] is abcc's local record and is **not** guaranteed to equal what the
/// server retained: [`Event::PromptCut`] fires, and LM Studio truncates the
/// *middle* of a conversation at HTTP 200. So this guard can correctly detect a
/// repeat and still strand a model whose visible copy the server has since
/// dropped. This is the escape valve for that case — the second and third
/// repeats are substituted, the fourth is served whole. **Zero instances of the
/// failure it guards against have been observed**, because zero were possible
/// before the guard existed. ▶ Measure it; do not trust it.
///
/// Over this project's whole log the valve costs 16 of 131 substitutions and
/// 233,283 of 1,785,975 duplicate bytes — 13%, against the 87% it still saves.
const DUPLICATE_ESCAPE_EVERY: usize = 4;

/// A 64-bit FNV-1a over the bytes, as sixteen lowercase hex characters.
///
/// ⚠ **Not a cryptographic digest and nothing rests on it being one.** The
/// equality test is the full content compare in [`duplicates_so_far`]; this only
/// has to name one result stably enough that the model can search back for it,
/// and that repeats of the *same* content produce the *same* header. A collision
/// would join two counts, and joining two counts lets an extra copy through,
/// which is the safe side of the guard.
fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// How many times this exact result has already been served *or* substituted in
/// this body.
///
/// 🚨 **Both halves of the filter are needed.** A substituted repeat no longer
/// carries the content, so counting full copies alone would say *once* forever
/// and the fourth occurrence the escape valve is about would never arrive.
/// Counting headers alone would miss the first, real copy.
fn duplicates_so_far(body: &Body, content: &str, header: &str) -> usize {
    body.messages()
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter(|m| m.content == content || m.content.starts_with(header))
        .count()
}

/// What the model reads instead of bytes it already has.
///
/// ⚠ It names the mechanism and gives the model somewhere to go, in the same
/// spirit as [`NO_ANSWER`] — but unlike a paragraph in the brief, **none of the
/// saving depends on the model believing it.** F827 measured a brief paragraph
/// written against this exact behaviour at 89.9% → 88.0%: a prompt can remove a
/// read the model does not need and cannot remove one it believes it needs. The
/// bytes are withheld here whether or not the sentence lands.
fn duplicate_note(header: &str, occurrence: usize, bytes: usize) -> String {
    format!(
        "{header}\nThis call returned the same {bytes} bytes as an earlier call in this same \
         conversation, byte for byte. This is occurrence {occurrence}, and the content is not \
         repeated here: repeating it is what fills the context window. Search this conversation \
         for the header line above, or for the earlier copy itself, and read it there. The bytes \
         cannot have gone stale — they are identical, so nothing you did changed them."
    )
}

/// Where the loop writes what happened.
///
/// A trait rather than a `Store`, because ADR-0006 makes the log the only thing
/// that crosses between a worker and anything else — so the loop's dependency is
/// *there is somewhere to write*, and the durable half is the driver's business.
pub trait Journal {
    fn record(&mut self, event: Event);
}

impl<F: FnMut(Event)> Journal for F {
    fn record(&mut self, event: Event) {
        self(event);
    }
}

/// What a tool did, in the shape `ToolCallEnded` wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// What the model is shown. It is the tool's own output rather than a
    /// summary of it, because a summary is a claim about a measurement.
    pub text: String,
    pub exit: Option<i32>,
    pub elapsed_ms: u64,
    /// The class, when the tool produced no measurable ending.
    pub unmeasured: Option<Why>,
}

/// Something that can run an admitted tool call.
///
/// The policy check has already happened when this is called — the `spec` is the
/// proof of it — so an implementation never re-decides whether the role may do
/// this. **Two places deciding one thing is how the donor ended up with a path
/// check that is not in the path.**
pub trait Tools {
    fn run(&self, spec: &'static ToolSpec, call: &ToolCall) -> ToolResult;
}

/// The tool layer for a head that has none.
///
/// 🚨 **It exists so that "the Judge has no tools" is a fact about the call and
/// not a fact about the argument somebody remembered to pass.**
/// [`Head::Commandos`] is capped at [`Tier::NoTools`], so
/// [`Policy::admits`](crate::tools::Policy::admits) refuses every name before
/// the loop reaches this — handing the phase a real workspace would behave
/// identically today and would put a tool layer within one edit of a role that
/// is defined by not having one.
///
/// It answers rather than panicking, for the same reason `rung::undeclared`
/// does: an unreachable arm that is somehow reached should cost a sentence an
/// operator can read, not a worker.
pub struct NoTools {
    role: &'static str,
}

impl NoTools {
    #[must_use]
    pub const fn for_head(head: Head) -> NoTools {
        NoTools {
            role: head.call_sign(),
        }
    }
}

impl Tools for NoTools {
    fn run(&self, spec: &'static ToolSpec, _call: &ToolCall) -> ToolResult {
        let why = Why::Denied {
            role: self.role.to_owned(),
            tool: spec.name.to_owned(),
            ceiling: Tier::NoTools.to_string(),
        };
        ToolResult {
            text: why.to_string(),
            exit: None,
            elapsed_ms: 0,
            unmeasured: Some(why),
        }
    }
}

/// The stops that are not the operator's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Rounds of tool calls before the phase gives up.
    ///
    /// ⚠ A **stop, not a tier**. There is one tier and the only purchase
    /// available is another attempt (ADR-0010), so exhausting this escalates
    /// nothing — it ends the phase with `BudgetExhausted`, which is neither a
    /// pass nor a failure.
    pub rounds: u32,
    /// The per-read budget on the stream, which is therefore also the idle-gap
    /// timeout: a stream silent this long is a hang (F198, F199).
    ///
    /// ⚠ It bounds *a server that has stopped talking*. It does **not** bound a
    /// server writing one tool call — see [`Limits::tool_call_gap`], which is
    /// F625 and is the reason the two are separate numbers.
    pub idle_gap: Duration,
    /// 🚨 **F625: the per-read budget while a tool call is open.**
    ///
    /// **400 s, and it is derived rather than chosen.** LM Studio delivers a tool
    /// call's arguments in one delta at the end (F622), so the silence between the
    /// announcement and the call is the time the model spends writing it, and that
    /// is bounded by [`Head::budget`](crate::Head::budget) — 16,384 tokens.
    ///
    /// 🚨 **F645: the first derivation rested on two wire captures; this one rests
    /// on 833 logged calls, and the two disagree about where the floor is.** Every
    /// row below is that budget divided by a decode rate, so they are the same
    /// arithmetic over different evidence:
    ///
    /// | basis | n | slowest decode | whole budget at it |
    /// |---|---|---|---|
    /// | F622's `bigarg` capture — *the old basis* | 1 | **43.1 tok/s** | **380 s** |
    /// | the log, any call size | 833 | 48.5 tok/s | 338 s |
    /// | the log, calls ≥ 8,000 tokens | 15 | 72.2 tok/s | 227 s |
    /// | the log's **longest call ever**, measured | — | — | **138.5 s** |
    ///
    /// 🚨 **F645's finding is that decode does not degrade with size — it
    /// improves.** The 48.5 tok/s floor comes from a **721-token** call; every call
    /// that approaches the budget runs at 72—140 tok/s. So a bound of *the whole
    /// budget at the slowest rate ever seen* composes the largest size with a rate
    /// that only occurs at small ones, and is pessimistic by construction.
    ///
    /// ⚠ **The capture is still the binding number, and deliberately so.** At 43.1
    /// tok/s it is 1.7× slower than anything in the log's own 8,000+ band, and the
    /// conditions that produced it were not recorded — but it is a real observation
    /// on this machine, and `ssecapture.py` is a direct client rather than a proxy,
    /// so it is not an instrument-in-the-path artifact. 400 s covers its 380 s with
    /// 5% margin. Anything below that tightens past a rate this machine has shown.
    ///
    /// 🚨 **Covering the whole budget is the point, not generosity.** Any value
    /// below it leaves the timeout capping how large a patch this system can
    /// write, which is the defect F625 names — it would only move the ceiling
    /// rather than remove it. The budget is already the stop; a second, tighter
    /// stop hidden inside the transport is the thing that made the largest patch
    /// ever seen (17,157 chars) a measurement of the timeout.
    ///
    /// ⚠ **What it costs, stated plainly: a server that dies *during* a tool call
    /// takes 6m40s to detect instead of ninety seconds** — 80 s better than the
    /// eight minutes this shipped with. That is survivable only because the silence
    /// is no longer unobserved: the liveness mark names the tool and the elapsed
    /// quiet every [`Limits::liveness_gap`], and the desk's `kill` reaches a worker
    /// between deltas.
    pub tool_call_gap: Duration,
    /// How long a stream may say nothing before the log says it is alive.
    /// ADR-0012 §5's bar is that no gap over ten seconds goes unmarked.
    ///
    /// 🚨 **F592: this is now also the wait slice**, which is what makes the bar
    /// reachable. The mark is written from inside the read loop, and that loop
    /// used to block for the whole `idle_gap` — so the detector could not observe
    /// the silence it existed to report, and 25 gaps of up to 73 s went unmarked.
    /// The provider now returns [`Delta::Waiting`](crate::provider::Delta) every
    /// slice, which wakes the check without changing any budget.
    pub liveness_gap: Duration,
    /// 🚨 **F503: how many times a phase asks again when the model answers with
    /// nothing.**
    ///
    /// A repair inside the phase, which is ADR-0010's shape — the body is
    /// borrowed by [`TurnLoop::run`] precisely so a retry is *the same body with
    /// the failure appended*. It is small because the failure it repairs is a
    /// missing closing sentence rather than a missing capability: across seven
    /// runs of one task the model produced no closing answer **five times**, and
    /// twice it produced a 2,008- and a 3,431-character one from the same head,
    /// brief, server and model. Exhausting it ends the phase
    /// [`Why::SaidNothing`], which is the ruling this does not overturn.
    pub nudges: u8,
    /// 🚨 **F829: how many characters of reasoning one turn may spend before
    /// abcc stops it.**
    ///
    /// **50,000, and it is derived rather than chosen.** The published rule was
    /// *12,288 reasoning tokens*, which cannot be wired: `reasoning_tokens` is
    /// the server's count and arrives only in the closing usage block, while the
    /// only quantity this loop holds per delta is
    /// [`Accumulator::reasoning_chars`]. Re-derived over **3,048 turns**:
    ///
    /// | ceiling (chars) | catches | false positives |
    /// |---:|---:|---:|
    /// | 30,000 | 6 | **4** |
    /// | 40,000 | 5 | **1** |
    /// | **41,000 – 60,000** | **5** | **0** |
    /// | 70,000 | 0 | 0 |
    ///
    /// ✅ A **plateau, not a knife edge**, which is the property that makes it
    /// safe. 50,000 is the point with the most margin on both sides at once:
    /// 9,372 above the most-reasoning turn that produced something, 10,760 below
    /// the least-reasoning turn that produced nothing.
    ///
    /// ⚠ **It is a stop, not a tier**, like [`Limits::rounds`]: crossing it ends
    /// the phase [`Why::ReasoningRunaway`], which is neither a pass nor a
    /// failure, and the purchase available is another attempt.
    ///
    /// ⚠ **It catches one shape.** One turn that reasons itself to the cap is
    /// caught; twenty-four small unproductive rounds are not, and that attempt
    /// still ends at [`Limits::rounds`]. 5 for 5 on turns, 0 for 1 on attempts.
    pub reasoning_ceiling: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            rounds: 24,
            // The champion's rung: ~2.4x its measured worst-case TTFB, and
            // 1.7-3.3x tighter than the 300 s inherited (F199).
            idle_gap: Duration::from_secs(90),
            // F625/F645. 400 s = the 16,384-token budget at 43.1 tok/s, the
            // slowest decode this machine has produced, plus 5%. Derived from
            // 833 logged calls, not chosen; see the field.
            tool_call_gap: Duration::from_secs(400),
            liveness_gap: Duration::from_secs(10),
            nudges: 2,
            // F829. 50,000 characters is the middle of the 41,000-60,000
            // plateau that catches 5 and costs 0 over 3,048 logged turns.
            // Derived, not chosen; see the field.
            reasoning_ceiling: 50_000,
        }
    }
}

/// What one phase cost, whether or not it produced anything.
///
/// Returned on every ending, including a stop, because a phase the operator
/// cancelled still spent tokens and an accounting that counts only successes is
/// an accounting that under-reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseReport {
    pub turns: u32,
    pub tool_calls: u32,
    pub denials: u32,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// `None` when no call reported one, which is different from zero.
    pub reasoning_tokens: Option<u32>,
    /// The most concerning trace signal any turn in this phase produced.
    ///
    /// ADR-0010 §7 asks *at token 200, has the reasoning trace closed?* and says
    /// the answer is a stop rather than a score. ⚠ **At Skeleton it is recorded
    /// and not acted on.** Stopping a generation on this signal is a
    /// behavioural change that needs a population to justify it, and the
    /// population is what recording it produces. It never becomes a number shown
    /// next to an answer either way.
    pub trace: TraceSignal,
    pub elapsed_ms: u64,
    /// The highest `prompt_tokens` any turn of this phase has reported.
    ///
    /// 🚨 **The phase is the unit, and that is the whole of why this lives here**
    /// (F748). The body is reset by `Body::opening` at every phase and appended to
    /// within one, so *monotone* is a property of a phase and not of an attempt —
    /// a high water carried across a phase boundary would report the next phase's
    /// opening brief as a 38,006-token cut, which is the reading F750 corrects.
    pub prompt_high_water: u32,
}

impl Default for PhaseReport {
    fn default() -> PhaseReport {
        PhaseReport {
            turns: 0,
            tool_calls: 0,
            denials: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            reasoning_tokens: None,
            trace: TraceSignal::Absent,
            elapsed_ms: 0,
            prompt_high_water: 0,
        }
    }
}

impl PhaseReport {
    /// Fold one turn's cost in, and answer whether the server cut its prompt.
    ///
    /// `reasoning_tokens` stays `None` until a provider reports one, because none
    /// reported is not the same as none spent.
    ///
    /// 🚨 **The detection is returned from the fold rather than left to the
    /// caller** (F748). The high water it compares against is updated in the same
    /// three lines, so there is no ordering in which the loop can read a stale
    /// one — and the loop cannot forget to ask, because the answer arrives with
    /// the count it already takes. `Some(high_water)` means *this turn's prompt
    /// was measured smaller than an earlier turn of this phase*, which a body
    /// that only grows cannot do.
    fn count(&mut self, turn: &Turn) -> Option<u32> {
        self.turns += 1;
        self.prompt_tokens = self.prompt_tokens.saturating_add(turn.usage.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(turn.usage.completion_tokens);
        if let Some(r) = turn.usage.reasoning_tokens {
            self.reasoning_tokens = Some(self.reasoning_tokens.unwrap_or(0).saturating_add(r));
        }
        if concern(turn.trace) > concern(self.trace) {
            self.trace = turn.trace;
        }
        let cut =
            (turn.usage.prompt_tokens < self.prompt_high_water).then_some(self.prompt_high_water);
        self.prompt_high_water = self.prompt_high_water.max(turn.usage.prompt_tokens);
        cut
    }
}

/// How much a trace signal is worth worrying about. Absent is not the same as
/// closed and neither is a problem; an open trace at token 200 is the one the
/// ADR is about.
fn concern(signal: TraceSignal) -> u8 {
    match signal {
        TraceSignal::Absent => 0,
        TraceSignal::Closed => 1,
        TraceSignal::OpenAt200 => 2,
    }
}

/// How a phase ended. Three ways, and only one of them is an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseEnded {
    /// The model stopped asking for tools and said something.
    Answered { text: String, report: PhaseReport },
    /// The operator stopped it. Not a failure of the work.
    Stopped { stop: Stop, report: PhaseReport },
    /// 🚨 No artifact, and here is the class. Never a score, and never an empty
    /// string standing in for one.
    Unmeasured { why: Why, report: PhaseReport },
}

impl PhaseEnded {
    #[must_use]
    pub fn report(&self) -> &PhaseReport {
        match self {
            PhaseEnded::Answered { report, .. }
            | PhaseEnded::Stopped { report, .. }
            | PhaseEnded::Unmeasured { report, .. } => report,
        }
    }
}

/// One phase's worth of driving.
pub struct TurnLoop<'a> {
    provider: &'a dyn Provider,
    tools: &'a dyn Tools,
    model: String,
    limits: Limits,
    /// 🚨 **The slot's ceiling, which caps every head that runs in it.**
    ///
    /// ADR-0014 §4 puts the ceiling on the role; this is the other half, and the
    /// effective ceiling is the narrower of the two. It is set once per loop
    /// rather than per phase because it is operator configuration about a *slot*
    /// — a per-phase cap would be a dial, and the whole point of the frozen head
    /// is that a phase's request shape is not one.
    ///
    /// ⚠ It must stay a [`Tier`] and never a list of tool names. W7 proved four
    /// times, with four mechanisms across three authors, that an argument check
    /// binds only the tool that has an argument; `Reach` is the property every
    /// tool declares and [`Tier::Exec`] *is* the class.
    ///
    /// Default [`Tier::Exec`] — the slot allows everything a role asks for, so
    /// the ceiling is the role's own and nothing changed for a caller that never
    /// sets one.
    ceiling: Tier,
    /// 🚨 **The denylist, and its default is ON** (ADR-0014 §5).
    ///
    /// `Secrets::default()` carries no literals and every shape, so a caller who
    /// never sets one still gets the shape half. A redactor whose default was
    /// *nothing* would be a control that protects the code paths somebody
    /// remembered, which is the donor defect one layer up: a good denylist hung
    /// off a function the shell never calls (F416).
    ///
    /// ⚠ It is not the control. See [`abcc_core::redact`].
    secrets: Secrets,
}

impl<'a> TurnLoop<'a> {
    #[must_use]
    pub fn new(provider: &'a dyn Provider, tools: &'a dyn Tools, model: impl Into<String>) -> Self {
        TurnLoop {
            provider,
            tools,
            model: model.into(),
            limits: Limits::default(),
            ceiling: Tier::Exec,
            secrets: Secrets::default(),
        }
    }

    /// Give the loop the literals this process holds — the model API key, today.
    ///
    /// The shapes are on either way; this adds the exact half, which is the only
    /// half with no false-positive story.
    #[must_use]
    pub fn secrets(mut self, secrets: Secrets) -> Self {
        self.secrets = secrets;
        self
    }

    #[must_use]
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Cap every head this loop runs at `ceiling`. See
    /// [`TurnLoop::ceiling`](TurnLoop#structfield.ceiling).
    #[must_use]
    pub fn ceiling(mut self, ceiling: Tier) -> Self {
        self.ceiling = ceiling;
        self
    }

    /// Run one phase to an ending, and write that ending down.
    ///
    /// `body` is borrowed rather than owned because a retry is the same body with
    /// the failure appended (ADR-0010) — handing it back is what makes the fix
    /// round *this phase with different feedback* rather than a new phase.
    ///
    /// 🚨 **The [`Event::PhaseEnded`] is recorded HERE, at the single exit, and
    /// not at the four places a phase can end** (F513). The report used to be
    /// returned and printed and nothing else, so `trace` — which exists only in
    /// flight — was lost on every run. One writer at one exit is also why the
    /// count cannot drift from the ending it describes.
    pub fn run(
        &self,
        head: Head,
        attempt: AttemptId,
        schema: Option<Schema>,
        body: &mut Body,
        control: &mut ControlPoint,
        journal: &mut dyn Journal,
    ) -> PhaseEnded {
        // 🚨 The posting is composed **here, once**, and everything downstream
        // reads it. The prefix, the wire tool array and `Policy::admits` are
        // three renderings of one list, and composing them from a head and a
        // ceiling separately is exactly how two of them come to disagree.
        let posting = head.posted(self.ceiling);
        let ended = self.drive(posting, attempt, schema, body, control, journal);
        let r = ended.report();
        journal.record(Event::PhaseEnded {
            attempt,
            by: posting.call_sign().to_owned(),
            turns: r.turns,
            tool_calls: r.tool_calls,
            denials: r.denials,
            prompt_tokens: r.prompt_tokens,
            completion_tokens: r.completion_tokens,
            reasoning_tokens: r.reasoning_tokens,
            trace: r.trace,
            elapsed_ms: r.elapsed_ms,
        });
        ended
    }

    /// The phase itself. Separate from [`run`](Self::run) only so that every one
    /// of its exits is funnelled through a single recording point.
    fn drive(
        &self,
        posting: Posting,
        attempt: AttemptId,
        schema: Option<Schema>,
        body: &mut Body,
        control: &mut ControlPoint,
        journal: &mut dyn Journal,
    ) -> PhaseEnded {
        let started = Instant::now();
        let mut report = PhaseReport::default();
        let mut nudges = self.limits.nudges;

        for round in 0..self.limits.rounds {
            let asking = Asking { posting, schema };
            let turn = match self.one_turn(asking, attempt, round, body, control, journal) {
                Ok(turn) => turn,
                Err(ending) => return ending.into_phase(report, started),
            };
            // 🚨 **F748: the server cut the prompt and said nothing.** Recorded
            // here because this is the only scope that has the phase's *sequence*
            // of calls — `Turn::uncertain` sees one turn and the silent cut is
            // invisible inside one (F751) — and recorded *before* the endings
            // below, because it is evidence about the call that just happened
            // whether or not this round is the phase's last.
            if let Some(high_water) = report.count(&turn) {
                journal.record(Event::PromptCut {
                    attempt,
                    reported: turn.usage.prompt_tokens,
                    high_water,
                });
            }

            // 🚨 Asked before the text is read, because a payload that is not
            // there is an absence rather than a short answer.
            if let Some(why) = turn.uncertain() {
                return Ending::Unmeasured(why).into_phase(report, started);
            }

            // 🚨 F497. Asked here rather than beside `uncertain` above because
            // nothing about the turn is wrong: the model stopped cleanly, and
            // the absence only exists once this text is about to become the
            // phase's artifact. Writing the `ClaimRecorded` first and ending
            // `Answered` afterwards is what put two zero-character claims on the
            // log and handed one of them to the next phase as its input.
            if let Some(why) = turn.said_nothing(posting.call_sign()) {
                // 🚨 F503: ask again before giving up. The model answers this
                // brief sometimes and not others — 2 of 7 — so the first
                // absence is a sample rather than a verdict, and the repair
                // belongs *inside* the phase for the same reason a failed tool
                // call does: the body is borrowed so that a retry is this
                // conversation with the failure appended, not a new one.
                if nudges > 0 {
                    nudges -= 1;
                    journal.record(Event::PhaseNudged {
                        attempt,
                        by: posting.call_sign().to_owned(),
                        left: nudges,
                    });
                    // The empty turn goes on the body as what it was. Hiding it
                    // would ask the model to answer a question it cannot see it
                    // has already failed.
                    body.append(Message::assistant(turn.text.clone()));
                    body.append(Message::user(NO_ANSWER));
                    continue;
                }
                return Ending::Unmeasured(why).into_phase(report, started);
            }

            if !turn.wants_tools() {
                let text = turn.text;
                journal.record(Event::ClaimRecorded {
                    attempt,
                    claim: Claim {
                        by: posting.call_sign().to_owned(),
                        text: text.clone(),
                    },
                });
                report.elapsed_ms = elapsed_ms(started);
                return PhaseEnded::Answered { text, report };
            }

            self.tool_round(posting, attempt, &turn, body, journal, &mut report);
        }

        Ending::Unmeasured(Why::BudgetExhausted {
            which: format!("{} rounds", self.limits.rounds),
        })
        .into_phase(report, started)
    }

    /// One model call: the step boundary, the request, the stream, the drain.
    fn one_turn(
        &self,
        asking: Asking,
        attempt: AttemptId,
        round: u32,
        body: &Body,
        control: &mut ControlPoint,
        journal: &mut dyn Journal,
    ) -> Result<Turn, Ending> {
        let Asking { posting, schema } = asking;
        // The step boundary, before any work is committed to.
        if let Disposition::Stop(stop) = control.check() {
            return Err(Ending::Stopped(stop));
        }

        let seed = seed_for(attempt, posting, round);
        let request = ApiRequest {
            model: &self.model,
            posting,
            body,
            schema,
            idle_gap: self.limits.idle_gap,
            tool_call_gap: self.limits.tool_call_gap,
            liveness_slice: self.limits.liveness_gap,
            seed,
        };
        journal.record(Event::ModelCallStarted {
            attempt,
            provider: self.provider.id().to_string(),
            model: self.model.clone(),
            head: posting.key().to_owned(),
            head_digest: posting.digest().to_owned(),
            // 🚨 Without this the log cannot say which policy was in force. A
            // capped slot changes what the model is *told* it has, so a run under
            // a cap and the same run without one differ in the prompt and agree
            // in every event — and status is a projection of the log alone.
            ceiling: posting.ceiling().to_string(),
            budget: posting.budget(),
            seed,
        });

        let mut stream = self
            .provider
            .start(&request)
            .map_err(|e| Ending::Unmeasured(e.why()))?;
        let drained = self.drain(&mut *stream, attempt, posting, control, journal);
        // Dropping the stream is the cancellation, so it happens here rather than
        // at the end of a scope somebody might later widen.
        drop(stream);

        match drained {
            Drained::Turn(turn) => Ok(turn),
            Drained::Failed(e) => Err(Ending::Unmeasured(e.why())),
            Drained::Runaway { chars } => Err(Ending::Unmeasured(Why::ReasoningRunaway {
                chars: chars as u64,
                ceiling: self.limits.reasoning_ceiling as u64,
            })),
            Drained::Interrupted => Err(match control.check() {
                Disposition::Stop(stop) => Ending::Stopped(stop),
                // The flag was set and the verb was not there. It cannot happen
                // through `ControlHandle`, which writes the verb first — so if it
                // does, say so rather than carry on with a dropped stream.
                Disposition::Carry => Ending::Unmeasured(Why::EngineError {
                    detail: "interrupted with no control verb on the channel".to_owned(),
                }),
            }),
        }
    }

    /// Decide what an inspecting tool's result should say when this body already
    /// carries it, byte for byte.
    ///
    /// `None` means serve the bytes. `Some((note, occurrence))` is the
    /// back-reference to send instead, and the occurrence number for the log.
    ///
    /// 🚨 **The state is the [`Body`], and that is what makes it phase-correct
    /// by construction.** A `Body` is fresh per phase — this module's own opening
    /// line is *two phases, two heads, two bodies* — while [`Workspace`] is one
    /// `&self`-shared instance reused across Localize and Change. Dedup state on
    /// the workspace would let a Localize read turn into a substitution in a
    /// Change phase whose body never saw it, which is a back-reference to
    /// nothing. Scoping to the body needs no new state, no new field and no
    /// cross-crate change, and cannot make that mistake.
    ///
    /// ✅ **A false positive on content is structurally impossible.** A file
    /// mutated between two reads no longer returns the same bytes, so a changed
    /// file is never substituted — the equality test is the check.
    ///
    /// 🚨 **Inspecting tools only, and that is measured rather than tidy.** An
    /// [`Reach::Edits`] or [`Reach::SpawnsChild`] result reports *what just
    /// happened*, never *what is in the workspace*: two identical
    /// `applied 1 hunk to 1 file` lines are two applied patches, and answering
    /// the second with *you already have this* would be a false statement about
    /// an event rather than a back-reference to a fact. **F830**: of the 150
    /// byte-identical repeats in this project's whole log, **12 are
    /// `apply_patch`** — ten failure messages and **two**
    /// `applied 1 hunk to 1 file` lines — and excluding the whole class costs
    /// 2,466 bytes of the 1,791,455 that repeat, 0.14%.
    ///
    /// ⚠ **The size floor is arithmetic, not a constant.** Substituting only
    /// pays when the note is smaller than what it stands in for; there is no
    /// measured size threshold and inventing one would be a number with nothing
    /// behind it. In the log this excludes exactly one repeat, a 78-byte
    /// `search` result that the note would have made *bigger*.
    fn already_have(
        &self,
        spec: &ToolSpec,
        output: &Scrubbed,
        body: &Body,
    ) -> Option<(Scrubbed, usize)> {
        if !matches!(spec.reach, Reach::Inspects) {
            return None;
        }
        let header = format!("{DUPLICATE_HEADER} {}", fingerprint(output.as_str()));
        let occurrence = duplicates_so_far(body, output.as_str(), &header) + 1;
        if occurrence == 1 || occurrence.is_multiple_of(DUPLICATE_ESCAPE_EVERY) {
            return None;
        }
        // ⚠ Scrubbed like any other result even though the text is ours, for the
        // reason the denial below is: the type is what makes *every* path to the
        // context go through the boundary, and an exemption for the strings we
        // wrote is the first of the exemptions.
        let note = self
            .secrets
            .scrub(duplicate_note(&header, occurrence, output.len()))
            .text;
        // The header has to survive scrubbing or the count above cannot find
        // this message next round — and a guard that cannot count its own notes
        // would refuse forever with no escape valve. Serving the bytes is the
        // safe side of that, so it is the fallback.
        let usable = note.as_str().starts_with(&header) && note.len() < output.len();
        usable.then_some((note, occurrence))
    }

    /// Admit, run and append every tool the turn asked for.
    fn tool_round(
        &self,
        posting: Posting,
        attempt: AttemptId,
        turn: &Turn,
        body: &mut Body,
        journal: &mut dyn Journal,
        report: &mut PhaseReport,
    ) {
        // The assistant's own turn goes on the body before its tool results, so
        // the transcript reads in the order it happened — and it carries the
        // calls it asked for, not only whatever it said alongside them. A tool
        // result whose question is missing is a message the OpenAI dialect
        // rejects and a lenient template renders as an answer from nowhere.
        body.append(Message::assistant_calling(
            turn.text.clone(),
            turn.tool_calls.clone(),
        ));
        let policy = posting.policy();
        for call in &turn.tool_calls {
            match policy.admits(&call.tool) {
                Ok(spec) => {
                    report.tool_calls += 1;
                    journal.record(Event::ToolCallStarted {
                        attempt,
                        tool: spec.name.to_owned(),
                        tier: spec.required_tier().to_string(),
                    });
                    let result = self.tools.run(spec, call);
                    // 🚨 **The redaction boundary, and it is one place** (ADR-0014
                    // §5). Everything a tool produces reaches its three sinks
                    // through these four lines: the log below, the model's
                    // context under it, and the console that projects the log.
                    // Scrubbing at each sink instead would be three denylists
                    // that agree until the day one of them is edited.
                    let output = self.secrets.scrub(result.text);
                    let asked = self.secrets.scrub(call.arguments.clone());
                    // The record says a class was removed and never what it was:
                    // a note quoting the match would put the secret back on the
                    // log one field to the left.
                    for note in [output.note(), asked.note()].into_iter().flatten() {
                        journal.record(Event::Note { text: note });
                    }
                    // 🚨 **The re-read guard, and it stands here on purpose** —
                    // upstream of the record below, so the log says what the
                    // model was actually sent rather than what the tool
                    // produced. Thirteen byte-identical whole-file reads filled
                    // a 40,960 window inside one attempt (F828) and the attempt
                    // died having changed nothing; whole-file reads are 54.1% of
                    // calls and **92.5% of the bytes** (F826). The measured
                    // saving on the two `SEC-06` attempts is ~29,000 and
                    // ~36,000 prompt tokens, both before their `prompt_cut`
                    // even fired.
                    let shown = match self.already_have(spec, &output.text, body) {
                        Some((note, occurrence)) => {
                            // ⚠ A `Note` and not a field: nothing branches on
                            // it, and the *measurement* the falsifier needs is
                            // already in `ToolCallEnded::output` — the duplicate
                            // byte share has to collapse there or the guard is
                            // not doing what it claims. This says how much was
                            // withheld, which that record can no longer show.
                            journal.record(Event::Note {
                                text: format!(
                                    "re-read guard: {} returned the same {} bytes again \
                                     (occurrence {occurrence}); the model was sent a {}-byte \
                                     back-reference instead",
                                    spec.name,
                                    output.text.len(),
                                    note.len(),
                                ),
                            });
                            note
                        }
                        None => output.text,
                    };
                    journal.record(Event::ToolCallEnded {
                        attempt,
                        tool: spec.name.to_owned(),
                        exit: result.exit,
                        elapsed_ms: result.elapsed_ms,
                        unmeasured: result.unmeasured.clone(),
                        // F505: what was refused, kept only when it was.
                        arguments: result.unmeasured.is_some().then_some(asked.text),
                        // 🚨 **F713, and the clone is the point.** The log
                        // and the context are fed from one binding, two lines
                        // apart, so *what the record says the model was shown*
                        // and *what the model was shown* cannot drift. Recording
                        // a re-scrub, or a rendering, would be a field true of
                        // itself. ⚠ That binding is now `shown` and not
                        // `output.text`, which is the whole reason the re-read
                        // guard runs above this line: a guard that substituted
                        // *after* the record would make this field say the model
                        // read 14,017 bytes it never saw.
                        output: Some(shown.clone()),
                    });
                    body.append(Message::tool_result(&call.id, shown));
                }
                Err(denied) => {
                    report.denials += 1;
                    // ⚠ Scrubbed like any other result even though the text is
                    // ours: the type is what makes *every* path to the context
                    // go through the boundary, and an exemption for the strings
                    // we wrote is the first of the exemptions.
                    let refusal = self.secrets.scrub(denied.to_string()).text;
                    journal.record(Event::ToolCallEnded {
                        attempt,
                        tool: call.tool.clone(),
                        exit: None,
                        elapsed_ms: 0,
                        unmeasured: Some(Why::Denied {
                            role: posting.call_sign().to_owned(),
                            tool: call.tool.clone(),
                            ceiling: posting.ceiling().to_string(),
                        }),
                        // ⚠ `None`, and not an oversight. A denial is about the
                        // *class* — ADR-0014's control is that the role does not
                        // have this tool at all — and `ToolCall`'s own doc says
                        // the arguments of a tool a role may not have are not
                        // even parsed. Recording them here would invite exactly
                        // the argument-level reasoning W7 ruled against.
                        arguments: None,
                        // ⚠ A denial's text is a result like any other and is
                        // recorded like one (F713). `Why::Denied` already carries
                        // the role, the tool and the ceiling — but *the class was
                        // denied* and *this is the sentence the model read* are
                        // different facts, and the second one is the prompt
                        // surface. Deriving it from the first is what F708 was.
                        output: Some(refusal.clone()),
                    });
                    // The model is told, in its own transcript. A refusal it
                    // cannot see is a refusal it asks for again.
                    body.append(Message::tool_result(&call.id, refusal));
                }
            }
        }
    }

    /// Read one turn out of a stream, sampling the control channel between deltas
    /// and marking the log when the stream goes quiet.
    fn drain(
        &self,
        stream: &mut dyn TurnStream,
        attempt: AttemptId,
        posting: Posting,
        control: &ControlPoint,
        journal: &mut dyn Journal,
    ) -> Drained {
        let started = Instant::now();
        let mut acc = Accumulator::default();
        let mut last_mark = Instant::now();

        loop {
            // 🚨 Between deltas, not inside the read. One atomic load, and the
            // caller drops the stream on the way out.
            if control.interrupted() {
                return Drained::Interrupted;
            }
            // 🚨 **F829, and it is the second thing sampled between deltas for
            // the same reason as the first: stopping is dropping.** Returning
            // here drops the stream, which closes the socket in 3–14 ms (F200);
            // no cancel token reaches into the provider and none is needed.
            //
            // ⚠ **Characters, not tokens.** `usage.reasoning_tokens` arrives
            // only in the closing usage block, so in flight there is nothing
            // else to read. See [`Limits::reasoning_ceiling`] for the 3,048-turn
            // re-derivation that produced the number.
            //
            // 🚨 **Both halves, and the second one is F831.** The ceiling alone
            // cannot be made safe: `a2065` holds a barren turn at 26,275 chars
            // and a productive one at 34,760, in one attempt. So this never
            // throws away a turn that has already produced something — see
            // [`Accumulator::produced_nothing`], including what it does not buy.
            if acc.reasoning_chars >= self.limits.reasoning_ceiling && acc.produced_nothing() {
                return Drained::Runaway {
                    chars: acc.reasoning_chars,
                };
            }
            if last_mark.elapsed() >= self.limits.liveness_gap {
                journal.record(Event::LivenessMark {
                    attempt,
                    note: acc.mark(posting.key()),
                });
                last_mark = Instant::now();
            }

            let Some(next) = stream.next_delta() else {
                break;
            };
            match next {
                Err(e) => return Drained::Failed(e),
                Ok(delta) => acc.take(delta),
            }
        }

        let Some((usage, finish)) = acc.ended.clone() else {
            // The stream ended with no ending. A proxy that answers 200 and then
            // nothing looks exactly like this, so it is named rather than guessed
            // at.
            return Drained::Failed(ProviderError::Malformed {
                detail: "the stream ended without a finish reason".to_owned(),
            });
        };

        let turn = Turn {
            trace: acc.trace(usage.completion_tokens),
            text: acc.text,
            reasoning_chars: acc.reasoning_chars,
            tool_calls: acc.tool_calls,
            usage,
            finish,
            ttfb_ms: acc.ttfb_ms,
            elapsed_ms: elapsed_ms(started),
            budget: posting.budget(),
        };

        // 🚨 F511. Recorded *after* the turn exists rather than beside the
        // stream, because whether the payload is about to be thrown away is a
        // question only the assembled turn can answer — and that answer is what
        // decides whether the arguments are kept.
        journal.record(Event::ModelCallEnded {
            attempt,
            usage,
            finish: turn.finish.clone(),
            ttfb_ms: turn.ttfb_ms,
            elapsed_ms: turn.elapsed_ms,
            composition: Some(turn.composition()),
        });

        Drained::Turn(turn)
    }
}

/// The deltas of one turn, folded as they arrive.
#[derive(Default)]
struct Accumulator {
    ttfb_ms: u64,
    text: String,
    reasoning_chars: usize,
    /// Argument bytes seen in flight, which is a different quantity from the
    /// assembled calls' lengths only when a turn is cut before its ending.
    tool_call_chars: u32,
    saw_reasoning: bool,
    reasoning_open: bool,
    tool_calls: Vec<ToolCall>,
    ended: Option<(Usage, Finish)>,
    /// 🚨 F625/F592. The tool call the server has announced and not yet
    /// delivered, and how long the stream has been quiet inside it. Together
    /// these are the whole content of a liveness mark taken during the one
    /// silence this stack produces on purpose — without them the mark says
    /// *0 chars of everything*, which is what a dead socket says too.
    writing: Option<String>,
    silent_ms: u64,
}

impl Accumulator {
    fn take(&mut self, delta: Delta) {
        match delta {
            Delta::Opened { ttfb_ms } => self.ttfb_ms = ttfb_ms,
            Delta::Text(chunk) => {
                self.reasoning_open = false;
                self.text.push_str(&chunk);
            }
            Delta::Reasoning(chunk) => {
                self.saw_reasoning = true;
                self.reasoning_open = true;
                self.reasoning_chars += chunk.len();
            }
            Delta::ToolCall(call) => {
                self.reasoning_open = false;
                self.writing = None;
                self.silent_ms = 0;
                self.tool_calls.push(call);
            }
            // 🚨 F625. The announcement, kept so the silence after it has a name
            // in the log. It is not content and moves no counter.
            Delta::ToolCallOpened { tool } => {
                self.reasoning_open = false;
                self.writing = Some(tool);
            }
            // 🚨 F592. The read loop saying it is still waiting. The only delta
            // that is *about* the absence of deltas, so it is the one thing a
            // mark written during a silence can report.
            Delta::Waiting { silent_ms } => self.silent_ms = silent_ms,
            // 🚨 F537. Not content, and counted anyway: this is the only
            // record that a turn spending its whole budget on one argument was
            // working rather than hanging.
            Delta::ToolCallProgress { chars } => {
                self.reasoning_open = false;
                self.tool_call_chars = self.tool_call_chars.saturating_add(chars);
            }
            Delta::Closed { usage, finish } => self.ended = Some((usage, finish)),
        }
    }

    /// Whether this turn has produced anything but trace so far — the same
    /// quantity [`Accumulator::mark`] reports, asked as a question.
    ///
    /// 🚨 **F831, and it is why the reasoning ceiling asks it.** The two
    /// populations the ceiling was fitted against — turns that produce
    /// something and turns that produce nothing — were separable over 3,048
    /// logged turns and are **not separable in general**: `a2065` contains a
    /// barren turn at 26,275 reasoning chars and a productive one at 34,760 in
    /// the same attempt. A threshold alone therefore cannot be made both safe
    /// and complete, and this predicate is the half that can be made safe.
    ///
    /// ⚠ **What it does NOT buy.** LM Studio buffers a tool call's arguments
    /// and delivers them in one delta at the end (F624), so a turn whose only
    /// output is a large call looks barren for almost all of its trace. The
    /// announcement — [`Delta::ToolCallOpened`] — arrives first and is what
    /// makes this more than a text check, but nothing guarantees a server
    /// sends one. **The threshold is still doing the real work**; this only
    /// makes it impossible to throw away something already produced.
    fn produced_nothing(&self) -> bool {
        self.text.is_empty()
            && self.tool_calls.is_empty()
            && self.tool_call_chars == 0
            && self.writing.is_none()
    }

    /// What a liveness mark says, which depends on what the stream is doing.
    ///
    /// 🚨 **The two sentences are the point.** A mark taken while bytes are
    /// flowing reports the counters, as it always did. A mark taken during a
    /// silence reports *which tool call is being written and for how long* —
    /// because on this stack that silence is the normal way a large patch is
    /// produced (F622), and the old sentence described it as a stream delivering
    /// nothing, which is indistinguishable from a dead socket.
    fn mark(&self, key: &str) -> String {
        match (&self.writing, self.silent_ms) {
            (Some(tool), ms) if ms > 0 => format!(
                "{key} writing a tool call: {tool}, quiet for {}, {} chars of trace so far",
                seconds(ms),
                self.reasoning_chars
            ),
            (Some(tool), _) => format!("{key} writing a tool call: {tool}"),
            (None, ms) if ms > 0 => format!(
                "{key} waiting: quiet for {}, {} chars of answer, {} of trace",
                seconds(ms),
                self.text.len(),
                self.reasoning_chars
            ),
            (None, _) => format!(
                "{key} streaming: {} chars of answer, {} of trace, {} of tool-call arguments",
                self.text.len(),
                self.reasoning_chars,
                self.tool_call_chars
            ),
        }
    }

    /// ADR-0010 §7's one in-flight signal, asked at token 200: **had the trace
    /// closed?** If not, this turn is far likelier to end at the ceiling with
    /// nothing. 🚨 It is a stop, not a score, and it never becomes a number shown
    /// next to an answer.
    fn trace(&self, completion_tokens: u32) -> TraceSignal {
        if !self.saw_reasoning {
            TraceSignal::Absent
        } else if self.reasoning_open && completion_tokens >= 200 {
            TraceSignal::OpenAt200
        } else {
            TraceSignal::Closed
        }
    }
}

enum Drained {
    Turn(Turn),
    Interrupted,
    /// The reasoning trace ran past [`Limits::reasoning_ceiling`] and this
    /// engine ended the turn. F829.
    Runaway {
        chars: usize,
    },
    Failed(ProviderError),
}

/// The two ways a phase ends without an artifact. Split out so the loop reads as
/// the sequence it is rather than as five early returns that each rebuild a
/// report.
enum Ending {
    Stopped(Stop),
    Unmeasured(Why),
}

impl Ending {
    fn into_phase(self, mut report: PhaseReport, started: Instant) -> PhaseEnded {
        report.elapsed_ms = elapsed_ms(started);
        match self {
            Ending::Stopped(stop) => PhaseEnded::Stopped { stop, report },
            Ending::Unmeasured(why) => PhaseEnded::Unmeasured { why, report },
        }
    }
}

fn elapsed_ms(from: Instant) -> u64 {
    u64::try_from(from.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Milliseconds as a person reads them, to one decimal.
///
/// Integer arithmetic rather than a float, because this lands in a log line an
/// operator reads during a silence and a rounding artefact there is a number
/// somebody will try to explain.
fn seconds(ms: u64) -> String {
    format!("{}.{} s", ms / 1000, (ms % 1000) / 100)
}

/// What one phase asks the model for, fixed for the whole phase.
///
/// The pair travels together because it is one decision: [`Posting`] composes
/// the head and the ceiling, and [`Schema`] is the shape the artifact of *that*
/// head has to take. What varies inside a phase is the round, which is why the
/// round is not in here.
#[derive(Clone, Copy)]
struct Asking {
    posting: Posting,
    schema: Option<Schema>,
}

/// llama.cpp's *pick one for me* sentinel. A derived seed that landed here would
/// be a call asking for randomness while the log recorded a number, so it is the
/// one value this function will not return.
const LLAMA_RANDOM_SEED: u32 = u32::MAX;

/// The sampler's seed for one model call.
///
/// 🚨 **Derived rather than fixed, and that distinction is the whole ruling**
/// (the operator, 2026-09-12). What a sortie needs is to be *replayable*, which
/// means the seed is **written down**; what it must not become is *repetitive*,
/// which is what one constant would make it. A retry carries a different
/// [`AttemptId`] — the id is the seq that opened it — so it samples differently
/// from its parent, which matters now that F701 has it opening on its parent's
/// tree. A constant would have made the two together a no-op.
///
/// The three inputs are the three things that distinguish one call from another
/// within a run: which attempt, which head, and which round of that head's
/// phase. ⚠ The **digest** rather than the head's key, so that editing a
/// charter re-seeds the calls made under it — two builds that disagree about the
/// prompt must not agree about the sampler and call the pair a replication.
///
/// ⚠ `u32` because the request is JSON and LM Studio parses it in TypeScript,
/// where anything past 2^53 is rounded on the way in.
fn seed_for(attempt: AttemptId, posting: Posting, round: u32) -> u32 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(attempt.to_string().as_bytes());
    hasher.update([0]);
    hasher.update(posting.digest().as_bytes());
    hasher.update([0]);
    hasher.update(round.to_le_bytes());
    let out = hasher.finalize();
    let seed = u32::from_le_bytes([out[0], out[1], out[2], out[3]]);
    // One value means *choose your own*, so it is the one this may not hand back.
    if seed == LLAMA_RANDOM_SEED { 0 } else { seed }
}
