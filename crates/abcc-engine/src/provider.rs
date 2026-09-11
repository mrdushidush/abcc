//! The provider seam: `&self`, two classes, and a stream.
//!
//! ADR-0013. The inherited abstraction had **one production implementation out of
//! eighteen**, was pinned to a concrete type at every production call site, and
//! carried a signature — `&mut self`, returning a `Vec`, blocking — that makes
//! co-op's headline requirement *an async escalation that does not stall the
//! local pipeline* **unrepresentable** (F466–F468). Three things change here and
//! each of them is forced by a measurement rather than by taste.
//!
//! 1. **`&self` rather than `&mut self`.** This is the whole point: it permits a
//!    pending cloud turn and a running local turn to coexist, which the inherited
//!    trait forbids.
//! 2. **A stream rather than a `Vec`.** [`TurnStream`] yields one [`Delta`] at a
//!    time, and cancellation is *not* a runtime capability — **it is a socket
//!    close, and a socket closes when its owner drops it**. A watchdog flipping a
//!    flag and the reader dropping the response stopped a live generation at
//!    **14 ms and 4 ms** (F200), so nothing in this module needs to know about
//!    cancellation: the turn loop samples its control channel between deltas and
//!    drops the stream.
//! 3. **[`ProviderClass`] is two-valued on purpose.** It is the predicate the
//!    egress guard binds to, and **a value only ever read by a `match` asking
//!    "does this leave the machine" must not have a third arm that answers "sort
//!    of"**. A remote rig is a *device on the local provider*, never a peer:
//!    llama.cpp spreads one model across local and remote devices in proportion
//!    to memory, so a rig buys parameters or context and **buys no parallelism at
//!    all**. Nothing above this trait ever names one.
//!
//! ⚠ The timeout carried on a request is a **per-read** budget, not a total one.
//! Six body lines 1000 ms apart under a 2 s client timeout complete in 5.003 s; a
//! 3000 ms gap fails at 2.004 s (F198). The deadline is recomputed inside each
//! `read()`, which is the definition of an idle-gap timeout — so the same number
//! is the hang detector, and a stream silent for that long is
//! [`ProviderError::IdleGap`] rather than a generic failure.

use std::fmt;
use std::time::Duration;

/// Re-exported so this module still *names* the signal it computes: `trace()`
/// below is the only thing that can produce one, and it does so from deltas that
/// are gone by the end of the turn. It is defined in `abcc-core` because it has
/// to be writable to the log — see [`abcc_core::Event::PhaseEnded`] (F513).
pub use abcc_core::event::TraceSignal;
use abcc_core::event::{CallShape, Composition, Finish, Usage};
use abcc_core::outcome::Why;
use abcc_core::redact::Scrubbed;
use serde::{Deserialize, Serialize};

use crate::head::Posting;

/// Which provider answered. It goes on every model event, so the log says where
/// a token came from rather than the caller's memory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    #[must_use]
    pub fn new(raw: impl Into<String>) -> ProviderId {
        ProviderId(raw.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The egress-relevant fact, and the only one. **Two arms, deliberately.**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderClass {
    /// On this machine, or on a device this machine spreads a model across.
    Local,
    /// The payload leaves. Every call in this class passes the redaction and
    /// audit boundary at [`Provider::start`] (ADR-0013 §4).
    Cloud,
}

impl ProviderClass {
    /// The one predicate. `SinglePlayer` is *zero providers for which this is
    /// true* — a policy rather than a degraded code path, which is the whole
    /// reason it can be the primary mode.
    #[must_use]
    pub fn leaves_the_machine(self) -> bool {
        matches!(self, ProviderClass::Cloud)
    }
}

// ---------------------------------------------------------------------------
// The request
// ---------------------------------------------------------------------------

/// Who is speaking. 🚨 **There is no `System` variant, and that is the
/// enforcement of ADR-0011 §2**: the system prefix is the [`Head`], it is
/// immutable within an attempt, and a caller cannot prepend one because the
/// vocabulary does not contain the word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Tool,
}

/// One message in the varying half of a request.
///
/// 🚨 **`tool_calls` exists because writing the real provider found the seam
/// short one field.** [`crate::scripted::Scripted`] is documented as *the
/// contract the HTTP provider has to satisfy — if writing the real one requires a
/// shape this cannot express, the seam is wrong and that is worth finding out in
/// a test rather than at a socket*, and this is that. A transcript that carries a
/// tool *result* with no record of the assistant turn that **asked** for it is
/// not the conversation that happened: the `OpenAI` dialect rejects it outright,
/// and a lenient chat template renders results arriving from nowhere — so the
/// model is shown answers to questions it cannot see itself having asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Set on [`Role::Tool`], naming the call this is the result of.
    pub tool_call_id: Option<String>,
    /// Set on [`Role::Assistant`], naming the calls this turn asked for. Empty on
    /// every other role, and empty on an assistant turn that only spoke.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

impl Message {
    #[must_use]
    pub fn user(content: impl Into<String>) -> Message {
        Message {
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }

    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Message {
        Message {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }

    /// The assistant turn that asked for tools, with what it asked for.
    ///
    /// The text may be empty — a turn that only called tools is the common case —
    /// and the calls are carried verbatim, arguments still a string, because the
    /// transcript records what was asked rather than what the host made of it.
    #[must_use]
    pub fn assistant_calling(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Message {
        Message {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls,
        }
    }

    /// A tool's output, on its way into the model's context.
    ///
    /// 🚨 **It takes a [`Scrubbed`] and not a `String`** (ADR-0014 §5). The
    /// model's context is one of the three sinks a secret in a tool result
    /// reaches, and it is the sink the donor left open — redaction there covered
    /// both disk sinks and neither the console nor the context (F418). This
    /// signature is what makes a second path to it a compile error rather than
    /// an omission.
    #[must_use]
    pub fn tool_result(call_id: impl Into<String>, content: Scrubbed) -> Message {
        Message {
            role: Role::Tool,
            content: content.into_string(),
            tool_call_id: Some(call_id.into()),
            tool_calls: Vec::new(),
        }
    }
}

/// The varying half of a request — everything after the frozen head.
///
/// 🚨 **The only mutation is [`Body::append`].** ADR-0010's rule is *append the
/// failure context, never prepend it*, and this type is the rule: there is no
/// `insert`, no `prepend`, no indexed write. A retry that rewrites the prompt
/// head costs a full cold prefill at **4.60×**, against the **79.7%** TTFT saving
/// that one changed token at the front annihilates (F81).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Body {
    messages: Vec<Message>,
}

impl Body {
    #[must_use]
    pub fn new() -> Body {
        Body::default()
    }

    /// The task, as the operator wrote it.
    #[must_use]
    pub fn opening(prompt: impl Into<String>) -> Body {
        Body {
            messages: vec![Message::user(prompt)],
        }
    }

    /// Add to the end. The only way to change a body.
    pub fn append(&mut self, message: Message) -> &mut Body {
        self.messages.push(message);
        self
    }

    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// A constrained-output contract.
///
/// ADR-0011 §3: `response_format: json_schema` with `strict: true` for every
/// structured artifact — **because it makes malformation unrepresentable**, not
/// because the unconstrained arm was measured failing (it was not, F248). ⚠ Never
/// `json_object`: the server answers HTTP 400, which is not a style preference.
///
/// Both fields are `&'static str` because a schema is part of the contract, not
/// per-call data — and because anything that varies inside a request is a thing
/// that can accidentally reach the head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schema {
    pub name: &'static str,
    pub json: &'static str,
}

/// One turn's worth of request.
///
/// The head is a [`Posting`] rather than a string, so the prefix, the budget and
/// the admitted tool set all arrive from one immutable place and cannot drift
/// apart. 🚨 It is a *posting* rather than a `Head` because a slot may cap the
/// role running in it: the prefix a request sends and the policy the turn loop
/// enforces have to be composed from the same ceiling, and a `Head` cannot carry
/// one.
#[derive(Debug, Clone, Copy)]
pub struct ApiRequest<'a> {
    /// The model name, which is **configuration and never a constant** — the
    /// roster is three champions by axis and the price of the quality one is
    /// 4.9×, which is David's to spend per session (ADR-0011 §1).
    pub model: &'a str,
    pub posting: Posting,
    pub body: &'a Body,
    pub schema: Option<Schema>,
    /// 🚨 The **per-read** budget, which is therefore also the idle-gap timeout.
    /// Set from the rung: champion 90 s, R3 180 s (F199), roughly 2.4× each one's
    /// measured worst-case TTFB and 1.7–3.3× tighter than the 300 s inherited.
    pub idle_gap: Duration,
    /// 🚨 **F625: the per-read budget while a tool call is open**, between
    /// [`Delta::ToolCallOpened`] and the call itself.
    ///
    /// A second number rather than a larger first one, because the two bound
    /// different things. `idle_gap` bounds *the server has stopped talking*, and
    /// 90 s is right for that. This bounds *the server is writing one tool call
    /// that it will deliver whole* — a silence proportional to the call's length
    /// and to nothing else — so spending `idle_gap` against it does not detect a
    /// hang, it caps the patch size.
    pub tool_call_gap: Duration,
    /// 🚨 **F592: how long one wait slice is**, so that a stream which is quiet
    /// still wakes the caller often enough for it to mark the log.
    ///
    /// It changes neither budget above and it is not a timeout — it is the
    /// resolution at which silence becomes visible. Set from the turn loop's
    /// liveness period, so the thing that writes the log chooses how often it
    /// can.
    pub liveness_slice: Duration,
}

impl ApiRequest<'_> {
    /// Tokens this request will accept back.
    ///
    /// **16384, from the head, for every model phase that emits a structured
    /// artifact.** It was 8192; see [`Head::budget`](crate::Head::budget) for the
    /// twenty runs that
    /// falsified that number's justification. In short: the quantity that
    /// actually overruns is not the reasoning trace but **one tool call's
    /// arguments** (F511), and an overrun loses them entirely rather than
    /// recording them, because a call cut mid-argument never becomes a call
    /// (F515).
    #[must_use]
    pub fn budget(&self) -> u32 {
        self.posting.budget()
    }
}

// ---------------------------------------------------------------------------
// The stream
// ---------------------------------------------------------------------------

/// A tool call the model asked for. Arguments stay a string until the tool layer
/// admits the call: parsing the arguments of a tool a role may not have is work
/// done on behalf of a request that is about to be refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub tool: String,
    pub arguments: String,
}

/// One piece of a turn as it arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    /// The first byte of the body. Carries the TTFB the per-read budget is set
    /// against, so the number that tunes the timeout is measured by the thing the
    /// timeout guards.
    Opened {
        ttfb_ms: u64,
    },
    Text(String),
    /// The reasoning trace. Logged as a length rather than kept, because it is
    /// the quantity that overruns and **without it the failure is invisible in
    /// the record** (ADR-0011 §3).
    Reasoning(String),
    ToolCall(ToolCall),
    /// 🚨 **F537: argument bytes for a tool call arrived.** Carries the count
    /// and nothing else — the call itself is emitted whole, once, as
    /// [`Delta::ToolCall`], because a half-written argument is not a request
    /// (F506).
    ///
    /// 🚨 **F624: the premise it was built on is false, and this delta cannot do
    /// the job it was added for.** F537 reasoned that *a model writing one large
    /// tool call delivers bytes continuously while producing no delta at all*,
    /// and that assembling the call silently was what made the hang detector a
    /// content detector. The raw SSE says otherwise: **LM Studio buffers the
    /// whole tool call and delivers it in one delta at the end** — 231 chars and
    /// 33,962 chars behave identically, the large one arriving in the last 4 ms
    /// of a 226-second call — while `reasoning_content` streams token by token
    /// in the same responses. So `pushed > 0` is true exactly once per call, in
    /// its final milliseconds, and **this fires after the silence it exists to
    /// break.** The field log agreed all along: every non-zero
    /// `tool_call_chars` mark is the terminal mark of its call and none has ever
    /// grown across two marks.
    ///
    /// ⚠ F537's *diagnosis* stands — the 90 s hang was real and the GPU was at
    /// 75% throughout. What it got wrong is where the bytes were: they were not
    /// arriving unnoticed, they were not arriving. See F625 for what that costs
    /// (a **successful** 33,962-char `apply_patch` is 211.2 s of silence against
    /// a 90 s per-read gap) and `research/DEBUG-P1-the-buffered-argument.md` §8
    /// for the three repair shapes, which are the idle gap's semantics and so
    /// are David's.
    ToolCallProgress {
        chars: u32,
    },
    /// 🚨 **F625: a tool call has opened, and its arguments have not arrived.**
    /// This is the announcement the server makes the moment it starts writing a
    /// call — on the wire it is a `tool_calls` fragment carrying an `id`, a
    /// `type` and the function's **name**, with `arguments: ""` — and until F625
    /// it was received and discarded.
    ///
    /// It is what makes the silence that follows *observable*, which is the one
    /// thing the idle gap needed and did not have. Measured on the raw stream,
    /// this delta to [`Delta::ToolCall`] is **0.7 s** for a 231-character call,
    /// **211.2 s** for a 33,962-character one, and **never** for a call cut at
    /// the token cap. A per-read budget that cannot tell those apart does not
    /// detect a hang; it caps the patch size.
    ///
    /// ⚠ It carries the tool's name because the name is what the wire gives us,
    /// and because *waiting for `apply_patch`* is a different sentence to put in
    /// front of an operator than *waiting*.
    ToolCallOpened {
        tool: String,
    },
    /// 🚨 **F592: the stream has been quiet for a while and is still open.** Not
    /// content and not an error — it is the read loop saying *nothing has arrived
    /// yet*, so that a silence detector which rides the data path can observe
    /// silence at all.
    ///
    /// Before this, the turn loop checked its liveness clock at the top of a loop
    /// whose next statement then blocked for the whole idle gap, so **no liveness
    /// mark could be written while the stream was quiet** — the field log carries
    /// 25 `model_call_started` → `liveness_mark` gaps of up to 73 s, the model's
    /// entire TTFB unmarked. The wait is now taken in slices, and this is what a
    /// slice returns when it expires.
    ///
    /// `silent_ms` is the silence so far on **this** read rather than since the
    /// turn began, because the quantity a gap is spent against is the per-read
    /// one (F199).
    Waiting {
        silent_ms: u64,
    },
    /// The turn ended, and how.
    Closed {
        usage: Usage,
        finish: Finish,
    },
}

/// A turn in flight.
///
/// Dropping it closes the socket, which is the cancellation mechanism — measured
/// at 3–14 ms to stop a live generation, with the server observing the dead
/// socket one write cadence later (F200). There is deliberately no `cancel()`
/// method: a second way to stop is a second thing that can be forgotten.
pub trait TurnStream: Send {
    /// The next piece, or `None` when the stream is finished.
    fn next_delta(&mut self) -> Option<Result<Delta, ProviderError>>;
}

/// What a model call can talk to.
///
/// `&self` throughout, so one provider serves every worker thread and a pending
/// cloud turn does not block a running local one.
pub trait Provider: Send + Sync {
    /// The id that goes on every event this provider produces.
    fn id(&self) -> ProviderId;

    /// Local or cloud. The egress guard binds here, not to a tool.
    fn class(&self) -> ProviderClass;

    /// Open a turn.
    ///
    /// 🚨 For [`ProviderClass::Cloud`] this is the redaction and audit boundary:
    /// egress is guarded on the **provider**, never on the tools, because the
    /// model call is the one path a co-op payload actually leaves by (F469–F471).
    ///
    /// # Errors
    ///
    /// Any [`ProviderError`]. A refusal by the run's frozen egress policy is
    /// [`ProviderError::EgressDenied`] and is a normal outcome, not a fault.
    fn start(&self, req: &ApiRequest<'_>) -> Result<Box<dyn TurnStream>, ProviderError>;
}

// ---------------------------------------------------------------------------
// Failure
// ---------------------------------------------------------------------------

/// Why a turn did not produce an answer.
///
/// Every arm maps to a [`Why`] through [`ProviderError::why`], because ADR-0009
/// §7 makes that enum the engine's failure class too — so a provider fault
/// reaches the report as a sentence an operator can act on rather than as a
/// string somebody has to read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("the connection to {provider} failed: {detail}")]
    Transport { provider: String, detail: String },
    #[error("{provider} answered HTTP {code}: {body}")]
    Status {
        provider: String,
        code: u16,
        body: String,
    },
    /// 🚨 The hang signal, and a **class rather than a generic failure**. The
    /// stream went silent for longer than the per-read budget, which is what
    /// F172 says the whole donor family lacks and F198 found sitting under the
    /// code already.
    #[error("the stream was silent for {after_ms} ms")]
    IdleGap { after_ms: u64 },
    /// The bytes arrived and were not a turn — a proxy that returned 200 and then
    /// nothing, a body that is not the shape the schema promised.
    #[error("the response could not be read: {detail}")]
    Malformed { detail: String },
    /// The run's frozen egress policy refused the payload. A normal outcome.
    #[error("egress denied by {rule}")]
    EgressDenied { rule: String },
}

impl ProviderError {
    /// The reason as the report and the log carry it.
    #[must_use]
    pub fn why(&self) -> Why {
        match self {
            ProviderError::IdleGap { after_ms } => Why::Timeout {
                after_ms: *after_ms,
            },
            ProviderError::EgressDenied { rule } => Why::BudgetExhausted {
                which: format!("egress: {rule}"),
            },
            other => Why::EngineError {
                detail: other.to_string(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// The turn, drained
// ---------------------------------------------------------------------------

/// What one turn produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub text: String,
    /// The trace's size, not its content. It is logged because it is the quantity
    /// that overran; it is not kept because it is not evidence.
    pub reasoning_chars: usize,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    pub finish: Finish,
    pub ttfb_ms: u64,
    pub elapsed_ms: u64,
    pub trace: TraceSignal,
    /// The cap this turn was given, so [`Turn::uncertain`] can name it.
    pub budget: u32,
}

impl Turn {
    /// 🚨 **An empty payload at the cap is an absence, not a score.**
    ///
    /// `finish_reason == "length"` with nothing in the payload — 17 of 57 judge
    /// calls were lost this way — is [`Why::TruncatedAtCap`], never a verdict and
    /// never a zero. A truncated stream is the same kind of nothing.
    ///
    /// 🚨 **F498: `length` is two different endings and the token counts tell
    /// them apart.** The cap this turn was given is our own `max_tokens`; a
    /// server that stops *below* it did not stop for our reason, and the only
    /// other thing that ends a chat completion early is the context window. So
    /// the window is not guessed at — it is `prompt + completion`, exactly, and
    /// the [`overflow`](Why::ContextOverflow) branch is asked **first** because a
    /// window cut with an empty payload is indistinguishable from a cap cut
    /// until these two numbers are compared. Run 1 died at 14,261 + 2,123 =
    /// 16,384 against a budget of 8,192, and was called an engine fault.
    ///
    /// 🚨 **F506: `length` ends the turn whether or not the payload is empty,
    /// and that is the whole point.** A cut turn's *tool calls are fragments* —
    /// twice now the last one arrived with a zero-character argument string and
    /// was recorded as the model failing its own schema. Appending that turn and
    /// asking again is what produced both contentless HTTP 500s: `a422` was
    /// 8,209 + 8,192 = **16,401 against a 32,768 window**, so the window cannot
    /// explain it and the malformed conversation can. So a `Length` finish is
    /// uncertain **always**, and `content_empty` selects the sentence rather
    /// than deciding whether there is one.
    #[must_use]
    pub fn uncertain(&self) -> Option<Why> {
        if matches!(self.finish, Finish::Length { .. })
            && self.usage.completion_tokens < self.budget
        {
            // ⚠ Saturating because these are two counts from the wire, and a
            // provider that reports nonsense should not panic the run it is
            // already failing.
            return Some(Why::ContextOverflow {
                window: self
                    .usage
                    .prompt_tokens
                    .saturating_add(self.usage.completion_tokens),
                prompt_tokens: self.usage.prompt_tokens,
            });
        }
        // F506: at or above our own cap, `length` is our doing — and it is an
        // absence either way, because a payload cut mid-token is a fragment and
        // a tool call cut mid-argument is not a request.
        if self.finish.is_uncertain() || matches!(self.finish, Finish::Length { .. }) {
            Some(Why::TruncatedAtCap {
                budget: self.budget,
            })
        } else {
            None
        }
    }

    /// 🚨 **F497: the model ended cleanly and said nothing.**
    ///
    /// `finish: stop`, no tool calls, and an empty payload. It is asked
    /// separately from [`uncertain`](Self::uncertain) because nothing about the
    /// *turn* is wrong — the absence is only visible once the phase is about to
    /// treat this text as its artifact.
    ///
    /// `by` is the head's call sign, which this type does not know; the caller
    /// supplies it because it is the caller that is about to write the claim.
    #[must_use]
    pub fn said_nothing(&self, by: &str) -> Option<Why> {
        (self.finish == Finish::Stop && self.text.trim().is_empty() && !self.wants_tools()).then(
            || Why::SaidNothing {
                by: by.to_owned(),
                completion_tokens: self.usage.completion_tokens,
                reasoning_tokens: self.usage.reasoning_tokens,
            },
        )
    }

    /// Whether this turn asked for tools rather than answering.
    #[must_use]
    pub fn wants_tools(&self) -> bool {
        !self.tool_calls.is_empty()
    }

    /// 🚨 **F511: what this completion was made of, for the log.**
    ///
    /// The arguments are carried **only when this turn is about to be
    /// discarded** — which is [`uncertain`](Self::uncertain), the same question
    /// the caller is about to ask. A discarded turn runs none of its tool calls
    /// (F506), so the log is the only place its payload can survive; a turn that
    /// is used needs no copy, because a call that fails records its own
    /// arguments (F505) and a call that succeeds is visible in the tree.
    #[must_use]
    pub fn composition(&self) -> Composition {
        let discarded = self.uncertain().is_some();
        Composition {
            text_chars: u32::try_from(self.text.chars().count()).unwrap_or(u32::MAX),
            reasoning_chars: u32::try_from(self.reasoning_chars).unwrap_or(u32::MAX),
            calls: self
                .tool_calls
                .iter()
                .map(|c| CallShape {
                    tool: c.tool.clone(),
                    argument_chars: u32::try_from(c.arguments.chars().count()).unwrap_or(u32::MAX),
                    arguments: discarded.then(|| c.arguments.clone()),
                })
                .collect(),
        }
    }
}
