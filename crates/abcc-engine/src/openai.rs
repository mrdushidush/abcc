//! The provider that talks to a model actually running on this box.
//!
//! ⚠ **The module is named for the dialect, not for the vendor.** LM Studio's
//! proxy on `:1234` and a bare `llama-server` both speak
//! `POST /v1/chat/completions` with `stream: true` and SSE, and the differences
//! between the two are real (F388: the bare server refuses `logprobs` with
//! `tools` + `stream`, and names the model by GGUF path rather than by the
//! LM Studio id). Nothing here is specific to either.
//!
//! [`crate::scripted::Scripted`] is the [`Delta`] sequence this has to produce.
//! That was not decoration: writing this module found the seam one field short —
//! see [`Message::assistant_calling`] — which is the only thing a contract-shaped
//! fake is for.
//!
//! # 🚨 F493 — the idle gap is ours, because the library's is gone
//!
//! ADR-0006 rests on F198: that `RequestBuilder::timeout` on `reqwest::blocking`
//! is a **per-read** budget that renews on every chunk, so one number is both the
//! hang detector and the time-to-first-byte bound. F198 flagged the risk itself —
//! *the doc comment describes the opposite behaviour, so a design that leans on
//! the measured behaviour must own a test that pins it* — and made pinning it the
//! **first of two not-optional tests**.
//!
//! **The test was written and it failed on its first run.** Re-measured here
//! against a socket that writes when it is told to, six lines 200 ms apart under
//! a 500 ms budget, with a control that completes:
//!
//! | version | F198 test A (1.2 s of body, 500 ms budget) | verdict |
//! |---|---|---|
//! | reqwest 0.12.28 | **fails at 0.502 s** | total duration |
//! | reqwest 0.13.4 | **fails at 0.509 s** | total duration |
//!
//! F198's *measurement* was right for the version it ran; its reading of the
//! mechanism was incomplete. `blocking/response.rs` and `blocking/wait.rs` are
//! still byte-identical to what it quoted — the per-read `wait::timeout` is still
//! there — but the blocking request's timeout now also reaches the async layer as
//! `RequestConfig<TotalTimeout>`, which wraps the response body in a
//! `TotalTimeoutBody` whose deadline runs from the start of the request. **Two
//! timers, one knob, and the total one always fires first.** There is no second
//! knob to reach for: `read_timeout` exists on the async `ClientBuilder` and is
//! absent from the blocking one, exactly as F199 recorded.
//!
//! So the gap detector is not bought from the client any more. The response is
//! read on its own thread, the deltas cross an `mpsc` channel, and the consumer
//! waits with `recv_timeout(idle_gap)`. That is the same instrument F199
//! specifies — *one number bounds the wait for the first byte and each subsequent
//! read, separately, not cumulatively* — and it has the property the old one
//! turned out not to have: **nothing outside this file can change what it
//! means.** The client is built with no timeout at all, which is deliberate and
//! is not the default (the blocking builder's default is 30 s).
//!
//! ⚠ The residue, stated rather than hidden: a stream abandoned on an idle gap
//! leaves its reader thread parked in `read()` until the server writes again or
//! the socket dies. It is one thread per in-flight model call — bounded at two by
//! the measured concurrency ceiling (F83) — and it costs nothing but a stack.
//! Cancellation of a *live* stream is unaffected and is still F200's socket
//! close: the reader's next send fails, it drops the response, the socket closes.
//!
//! # The rest, each forced by a measurement
//!
//! * **`stream_options.include_usage` is asked for, and a stream that answers
//!   without usage is [`ProviderError::Malformed`] rather than a turn costing
//!   zero tokens.** The donor asks for the same field for the same reason (F84).
//!   A zero from an instrument nobody validated is not a measurement, and an
//!   accounting that silently under-reports is worse than one that stops.
//! * **`response_format` is `json_schema` with `strict: true`, never
//!   `json_object`** — the server answers HTTP 400 to the older dialect, which is
//!   not a style preference (F82).
//! * **`num_ctx` is deliberately not sent.** It has no analogue in the compat
//!   dialect; the window is fixed at load time and the client cannot request one
//!   (F84). Pretending otherwise is the F50 trap.
//! * **A transient 400 during a model swap is retried once after 750 ms.** F79
//!   measures a swap at 23.77 s round trip, which is exactly when that window
//!   opens; the six surface forms are the donor's, and inheriting them is
//!   inheriting a list of strings rather than a design.
//! * **The non-SSE fallback keeps the same number and means something else by
//!   it**, and says so at the call site — see [`whole`].
//!
//! ⚠ **Not checked here, on purpose: that the server loaded the model that was
//! asked for.** LM Studio serves a request naming a model it does not have using
//! whichever model is loaded, and the bare server names models by GGUF path — so
//! the confirmation needs a fingerprint the operator configured, which is the
//! driver's business and not a socket's.

use std::io::{BufRead, BufReader, ErrorKind};
use std::sync::LazyLock;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use abcc_core::event::{Finish, Usage};
use reqwest::blocking::{Client, Response};
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::head::Posting;
use crate::provider::{
    ApiRequest, Delta, Message, Provider, ProviderClass, ProviderError, ProviderId, Role, ToolCall,
    TurnStream,
};

/// Where LM Studio listens until somebody moves it. ⚠ The port and the API key
/// change on every `lms load`, so both are configuration and neither is a
/// constant anything depends on.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:1234";

/// Read instead of [`DEFAULT_BASE_URL`] when set.
pub const BASE_URL_ENV: &str = "ABCC_MODEL_BASE_URL";
/// The bearer token, when the server was started with one.
pub const API_KEY_ENV: &str = "ABCC_MODEL_API_KEY";

/// How long to wait before the one retry. F84, from the donor's `api.rs:655-733`.
///
/// ⚠ It is spent **inside** the consumer's idle-gap budget, because from the
/// other side of the channel a retry is silence like any other. Against the
/// rung's 90 s (F199) that is a rounding error; it is written down because it is
/// the kind of thing that stops being one if either number moves.
const RELOAD_RETRY_AFTER: Duration = Duration::from_millis(750);

/// How many deltas the reader may run ahead of the consumer.
///
/// The gap is measured only when this is empty, which is the right question —
/// *is there anything to read* rather than *did the last chunk arrive on time*.
/// Bounded so a fast server cannot buffer a whole turn into memory ahead of a
/// consumer that is doing something with it.
const AHEAD: usize = 64;

/// 🚨 The six surface forms of LM Studio's transient 400, verbatim from the
/// donor. F79 measures a model swap at 23.77 s round trip and this is the window
/// it opens; the list is inherited as **strings**, which is all it ever was.
const RELOADING: [&str; 6] = [
    "model reloaded",
    "model is loading",
    "model not loaded",
    "model unloaded",
    "failed to load",
    "operation canceled",
];

// ---------------------------------------------------------------------------
// The provider
// ---------------------------------------------------------------------------

/// A [`Provider`] over one OpenAI-compatible chat-completions endpoint.
pub struct OpenAiCompat {
    id: ProviderId,
    endpoint: String,
    api_key: Option<String>,
    client: Client,
}

impl OpenAiCompat {
    /// Point at a server.
    ///
    /// `base_url` may be the root (`http://127.0.0.1:1234`), the versioned root,
    /// or the full completions path; all three land on the same endpoint.
    ///
    /// 🚨 **The client is built with no timeout, and that is not the default** —
    /// the blocking builder's default is 30 s, which would end every long turn as
    /// a transport failure. Since F493 the library's timer is a total-duration
    /// one and this design needs an idle gap, so the whole budget lives in
    /// [`TurnStream::next_delta`] instead.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Transport`] if the HTTP client cannot be built.
    pub fn new(base_url: &str) -> Result<OpenAiCompat, ProviderError> {
        let client = Client::builder()
            .timeout(None::<Duration>)
            .build()
            .map_err(|e| ProviderError::Transport {
                provider: "local".to_owned(),
                detail: e.to_string(),
            })?;
        Ok(OpenAiCompat {
            id: ProviderId::new("local"),
            endpoint: completions_url(base_url),
            api_key: None,
            client,
        })
    }

    /// The server named by [`BASE_URL_ENV`] and [`API_KEY_ENV`], or the default
    /// endpoint with no key.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Transport`] if the HTTP client cannot be built.
    pub fn from_env() -> Result<OpenAiCompat, ProviderError> {
        let base = std::env::var(BASE_URL_ENV).unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned());
        let mut provider = OpenAiCompat::new(&base)?;
        provider.api_key = std::env::var(API_KEY_ENV).ok().filter(|k| !k.is_empty());
        Ok(provider)
    }

    /// Send a bearer token with every call.
    #[must_use]
    pub fn with_api_key(mut self, key: impl Into<String>) -> OpenAiCompat {
        self.api_key = Some(key.into());
        self
    }

    /// Name this server, so the log says which one answered when there are two.
    #[must_use]
    pub fn named(mut self, id: impl Into<String>) -> OpenAiCompat {
        self.id = ProviderId::new(id);
        self
    }

    /// The URL this provider posts to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Provider for OpenAiCompat {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn class(&self) -> ProviderClass {
        ProviderClass::Local
    }

    /// Open a turn.
    ///
    /// ⚠ **Nothing about the reply is known when this returns**, including
    /// whether the server accepted the request. The status, the retry and the
    /// parse all happen on the reader thread and a refusal arrives as the first
    /// item on the stream. The loop treats a failed `start` and a failing stream
    /// identically — both are `Unmeasured` carrying the same [`Why`] — so this
    /// costs nothing and buys the property that matters: **there is no wait
    /// anywhere in this module that the idle gap does not bound.**
    ///
    /// [`Why`]: abcc_core::outcome::Why
    fn start(&self, req: &ApiRequest<'_>) -> Result<Box<dyn TurnStream>, ProviderError> {
        let (tx, rx) = sync_channel(AHEAD);
        let sending = Sending {
            client: self.client.clone(),
            endpoint: self.endpoint.clone(),
            api_key: self.api_key.clone(),
            provider: self.id.to_string(),
            payload: payload(req),
        };
        thread::Builder::new()
            .name("abcc-model-stream".to_owned())
            .spawn(move || sending.pump(&tx))
            .map_err(|e| ProviderError::Transport {
                provider: self.id.to_string(),
                detail: format!("no thread for the model stream: {e}"),
            })?;
        Ok(Box::new(Streamed {
            deltas: rx,
            idle_gap: req.idle_gap,
            tool_call_gap: req.tool_call_gap,
            slice: req.liveness_slice,
            awaiting_call: false,
            silent_since: None,
            done: false,
        }))
    }
}

// ---------------------------------------------------------------------------
// 🚨 The idle gap
// ---------------------------------------------------------------------------

/// A turn in flight, read one delta at a time.
///
/// 🚨 **This is the hang detector** (F493). It bounds the wait for the first byte
/// and each subsequent read, separately rather than cumulatively, which is F199's
/// semantic and is enforced here rather than bought from the HTTP client. A long
/// turn that keeps producing tokens runs as long as it likes; a silent one is
/// [`ProviderError::IdleGap`] after the rung's budget.
///
/// # 🚨 Two budgets, because there are two silences (F625)
///
/// **`idle_gap` bounds a server that has stopped talking. `tool_call_gap` bounds
/// a server that is writing one tool call.** LM Studio buffers a call's arguments
/// whole and delivers them in a single delta at the end (F622), so between
/// [`Delta::ToolCallOpened`] and [`Delta::ToolCall`] a *perfectly healthy* stream
/// carries nothing for as long as the call takes to write — measured at 211.2 s
/// for one that succeeded. Spending a 90 s per-read budget against that silence
/// does not detect a hang; it caps how large a patch this system can write, which
/// is why the largest patch ever seen intact sits inside the band 90 s buys.
///
/// The two are held apart by counting opens against deliveries, so the wider
/// budget applies **only** in the state the wire says it applies in, and one
/// unmatched open cannot widen the gap for the rest of the turn: `flush_calls`
/// emits every opened call, including one whose arguments never came.
///
/// # 🚨 The wait is taken in slices (F592)
///
/// A silence detector that rides the data path cannot observe silence. The turn
/// loop checks its liveness clock at the top of a loop whose next statement used
/// to block here for the whole budget, so nothing was written to the log while
/// the stream was quiet. The budget is now spent `slice` at a time and each
/// expired slice returns [`Delta::Waiting`], which wakes that check without
/// changing what the budget is or when it runs out.
struct Streamed {
    deltas: Receiver<Result<Delta, ProviderError>>,
    idle_gap: Duration,
    tool_call_gap: Duration,
    /// How long one wait slice is. The turn loop's liveness period, so the log's
    /// resolution is set by the thing that writes the log.
    slice: Duration,
    /// 🚨 Whether the last announced call has produced **any** bytes yet.
    ///
    /// ⚠ A bool, and the first version of this was a count of opens against
    /// [`Delta::ToolCall`]s — which was wrong in a way no test I wrote at first
    /// could see. `Delta::ToolCall` is emitted by `flush_calls` at
    /// `finish_reason`, not when the arguments land, so counting against it kept
    /// the wide budget open **from the first announcement to the end of the
    /// turn**. On this stack the two are 4 ms apart and nothing would ever have
    /// shown it; on a stream that did anything after a call, a dead socket would
    /// have waited the tool-call budget instead of the ordinary one.
    ///
    /// The state that actually matters is *announced, and not one byte since* —
    /// so it is set by [`Delta::ToolCallOpened`] and cleared by the first
    /// [`Delta::ToolCallProgress`], which is the server delivering. A bool rather
    /// than a count because a server announces and writes one call at a time.
    awaiting_call: bool,
    /// 🚨 When the current silence began, and **the reason slicing is correct
    /// rather than a way to never time out.** A [`Delta::Waiting`] does not end
    /// the read it interrupted, so the budget has to be spent against a clock
    /// that survives it; a per-call clock would restart every slice and the gap
    /// would become the slice. Cleared by any real delta, which is exactly
    /// F199's per-read semantic.
    silent_since: Option<Instant>,
    done: bool,
}

impl Streamed {
    /// The budget this read is spent against.
    fn budget(&self) -> Duration {
        if self.awaiting_call {
            self.tool_call_gap
        } else {
            self.idle_gap
        }
    }
}

impl TurnStream for Streamed {
    fn next_delta(&mut self) -> Option<Result<Delta, ProviderError>> {
        if self.done {
            return None;
        }
        let since = *self.silent_since.get_or_insert_with(Instant::now);
        // ⚠ Re-read every slice rather than captured once, because a call can
        // open in the middle of a wait and the read already running is the one
        // that then has to be allowed to take longer.
        let budget = self.budget();
        let left = budget.saturating_sub(since.elapsed());
        if left.is_zero() {
            self.done = true;
            return Some(Err(ProviderError::IdleGap {
                after_ms: millis(budget),
            }));
        }
        match self.deltas.recv_timeout(self.slice.min(left)) {
            Ok(delta) => {
                self.silent_since = None;
                if let Ok(d) = &delta {
                    match d {
                        Delta::ToolCallOpened { .. } => self.awaiting_call = true,
                        // Any byte of the call is the server delivering, and
                        // `ToolCall` closes it too — for a server that sends one
                        // whole call with no separate announcement.
                        Delta::ToolCallProgress { .. } | Delta::ToolCall(_) => {
                            self.awaiting_call = false;
                        }
                        _ => {}
                    }
                }
                // One failure ends a turn. What follows it would be describing a
                // stream that already stopped.
                self.done = delta.is_err();
                Some(delta)
            }
            // The slice expired and the budget has not. Say so, so the turn loop
            // can mark the log, and come back for the rest of the budget.
            Err(RecvTimeoutError::Timeout) => Some(Ok(Delta::Waiting {
                silent_ms: millis(since.elapsed()),
            })),
            // The reader finished and dropped its end.
            Err(RecvTimeoutError::Disconnected) => {
                self.done = true;
                None
            }
        }
    }
}

// Dropping `Streamed` drops the receiver, so the reader's next send fails and it
// drops the response — which closes the socket. That is F200's cancellation
// (3-14 ms), one hop further away than it used to be.

// ---------------------------------------------------------------------------
// The reader
// ---------------------------------------------------------------------------

/// Everything the reader thread needs, moved into it.
struct Sending {
    client: Client,
    endpoint: String,
    api_key: Option<String>,
    provider: String,
    payload: Value,
}

/// Where the reader puts what it read. `Err` means the consumer went away.
type Out<'a> = &'a SyncSender<Result<Delta, ProviderError>>;

impl Sending {
    /// Send the request, then read the reply into the channel.
    fn pump(&self, out: Out<'_>) {
        let started = Instant::now();
        let response = match self.post() {
            Ok(response) => response,
            // F84: the swap window, once. A second retry would be a policy about
            // how long to wait for a model, and that belongs to whoever loads it.
            Err(error) if is_reloading(&error) => {
                thread::sleep(RELOAD_RETRY_AFTER);
                match self.post() {
                    Ok(response) => response,
                    Err(error) => {
                        let _ = out.send(Err(error));
                        return;
                    }
                }
            }
            Err(error) => {
                let _ = out.send(Err(error));
                return;
            }
        };
        let streaming = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("text/event-stream"));
        if streaming {
            self.sse(response, started, out);
        } else {
            whole(response, &self.provider, started, out);
        }
    }

    /// One attempt at the request, with the status classified.
    fn post(&self) -> Result<Response, ProviderError> {
        let mut request = self.client.post(&self.endpoint).json(&self.payload);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let response = request.send().map_err(|e| ProviderError::Transport {
            provider: self.provider.clone(),
            detail: e.to_string(),
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().unwrap_or_else(|e| e.to_string());
        Err(ProviderError::Status {
            provider: self.provider.clone(),
            code: status.as_u16(),
            body,
        })
    }

    /// Read the SSE body line by line, folding it into deltas.
    fn sse(&self, response: Response, started: Instant, out: Out<'_>) {
        let mut lines = BufReader::new(response);
        let mut parse = Parse::default();
        let mut line = String::new();
        loop {
            line.clear();
            match lines.read_line(&mut line) {
                Err(e) => {
                    // ⚠ No timeout is set on this client, so a read error here is
                    // a real transport fault. The hang has exactly one detector
                    // and it is on the other side of the channel.
                    parse.out.push(Err(if e.kind() == ErrorKind::InvalidData {
                        ProviderError::Malformed {
                            detail: e.to_string(),
                        }
                    } else {
                        ProviderError::Transport {
                            provider: self.provider.clone(),
                            detail: e.to_string(),
                        }
                    }));
                    parse.stop = true;
                }
                Ok(0) => {
                    // EOF with no `[DONE]`. Not an error on its own: the ending
                    // is decided by whether a finish reason ever arrived.
                    parse.close();
                    parse.stop = true;
                }
                Ok(_) => {
                    parse.opened(started);
                    parse.take(line.trim_end_matches(['\r', '\n']));
                }
            }
            for delta in parse.out.drain(..) {
                if out.send(delta).is_err() {
                    // The consumer dropped the stream. Returning drops the
                    // response, which closes the socket.
                    return;
                }
            }
            if parse.stop {
                return;
            }
        }
    }
}

/// Whether a failure is the model-swap window rather than a real refusal.
fn is_reloading(error: &ProviderError) -> bool {
    let ProviderError::Status { body, .. } = error else {
        return false;
    };
    let body = body.to_ascii_lowercase();
    RELOADING.iter().any(|form| body.contains(form))
}

// ---------------------------------------------------------------------------
// The SSE state machine
// ---------------------------------------------------------------------------

/// A tool call being assembled out of fragments.
#[derive(Default)]
struct Partial {
    id: String,
    tool: String,
    arguments: String,
}

/// One turn's worth of stream, folded as its lines arrive.
#[derive(Default)]
struct Parse {
    out: Vec<Result<Delta, ProviderError>>,
    opened: bool,
    stop: bool,
    calls: Vec<Partial>,
    reason: Option<String>,
    usage: Option<Usage>,
    said_something: bool,
}

impl Parse {
    /// The first byte of the body, which is the TTFB the gap budget is set
    /// against (F199's table). Taken from the first read that returned anything,
    /// comment lines included — the measurement is the wait, not the content.
    fn opened(&mut self, started: Instant) {
        if !self.opened {
            self.opened = true;
            self.out.push(Ok(Delta::Opened {
                ttfb_ms: millis(started.elapsed()),
            }));
        }
    }

    /// One SSE line. Everything that is not a `data:` payload is framing.
    fn take(&mut self, line: &str) {
        let Some(data) = line.strip_prefix("data:") else {
            return;
        };
        let data = data.trim_start();
        if data == "[DONE]" {
            self.close();
            self.stop = true;
            return;
        }
        match serde_json::from_str::<Chunk>(data) {
            Ok(chunk) => self.take_chunk(chunk),
            Err(e) => {
                self.out.push(Err(ProviderError::Malformed {
                    detail: format!("a stream chunk did not parse: {e}"),
                }));
                self.stop = true;
            }
        }
    }

    fn take_chunk(&mut self, chunk: Chunk) {
        if let Some(error) = chunk.error {
            self.out.push(Err(ProviderError::Malformed {
                detail: format!("the server reported an error mid-stream: {error}"),
            }));
            self.stop = true;
            return;
        }
        if let Some(usage) = chunk.usage {
            self.usage = Some(usage.into());
        }
        for choice in chunk.choices {
            // ⚠ The trace field is `reasoning_content` — the one the donor reads
            // on both of its parsing paths (F388, `api.rs:889`/`:1125`). It is
            // folded as a length and never kept.
            if let Some(trace) = choice.delta.reasoning_content
                && !trace.is_empty()
            {
                self.out.push(Ok(Delta::Reasoning(trace)));
            }
            if let Some(text) = choice.delta.content
                && !text.is_empty()
            {
                self.said_something = true;
                self.out.push(Ok(Delta::Text(text)));
            }
            for fragment in choice.delta.tool_calls {
                self.fragment(&fragment);
            }
            if let Some(reason) = choice.finish_reason {
                // The calls are complete now, so they are emitted now — after the
                // reasoning that preceded them and before the ending, which is
                // the order that makes the trace signal read correctly.
                self.flush_calls();
                self.reason = Some(reason);
            }
        }
    }

    /// Fold one tool-call fragment into the call it belongs to.
    ///
    /// ⚠ Servers that stream fragments carry an `index`; servers that emit one
    /// complete call per chunk sometimes carry none, and defaulting those to zero
    /// would concatenate two calls into one. A fragment whose `id` disagrees with
    /// the slot it addresses therefore opens a new slot.
    fn fragment(&mut self, fragment: &ToolCallFragment) {
        let mut pushed = 0u32;
        let at = fragment
            .index
            .unwrap_or_else(|| self.calls.len().saturating_sub(1));
        let mismatch = self.calls.get(at).is_some_and(|slot| {
            fragment
                .id
                .as_deref()
                .is_some_and(|id| !slot.id.is_empty() && slot.id != id)
        });
        let at = if mismatch { self.calls.len() } else { at };
        while self.calls.len() <= at {
            self.calls.push(Partial::default());
        }
        let slot = &mut self.calls[at];
        if let Some(id) = &fragment.id {
            slot.id.clone_from(id);
        }
        let named_before = !slot.tool.is_empty();
        if let Some(function) = &fragment.function {
            if let Some(name) = &function.name {
                slot.tool.push_str(name);
            }
            if let Some(arguments) = &function.arguments {
                slot.arguments.push_str(arguments);
                pushed = u32::try_from(arguments.len()).unwrap_or(u32::MAX);
            }
        }
        // 🚨 F625. The moment a call has a name it has been *announced*, and on
        // this stack that is the last thing the wire carries until the whole call
        // parses — 211.2 s later for one that succeeded. Emitting it is what lets
        // the idle gap tell "writing a tool call" from "stopped talking".
        //
        // ⚠ Keyed on the slot going from unnamed to named rather than on
        // `arguments == ""`, because a server that sends the name and the whole
        // argument in one chunk announces and delivers in the same fragment, and
        // one that sends the name alone announces in its own. Both are one open.
        if !named_before && !slot.tool.is_empty() {
            let tool = slot.tool.clone();
            self.out.push(Ok(Delta::ToolCallOpened { tool }));
        }
        // 🚨 F537. The assembled call is emitted once, at the ending; this
        // says the stream is delivering *now*, which is the only question the
        // idle gap asks. Without it a model writing one large argument is
        // indistinguishable from a dead socket.
        if pushed > 0 {
            self.out.push(Ok(Delta::ToolCallProgress { chars: pushed }));
        }
    }

    fn flush_calls(&mut self) {
        for call in self.calls.drain(..) {
            if call.tool.is_empty() {
                continue;
            }
            self.out.push(Ok(Delta::ToolCall(ToolCall {
                id: call.id,
                tool: call.tool,
                arguments: call.arguments,
            })));
        }
    }

    /// The ending, or deliberately nothing.
    ///
    /// 🚨 A stream that stops without a finish reason emits **no**
    /// [`Delta::Closed`], so the turn loop names it — *the stream ended without a
    /// finish reason* — rather than this module guessing at a `Stop` that would
    /// read as a complete answer.
    fn close(&mut self) {
        self.flush_calls();
        let Some(reason) = self.reason.take() else {
            return;
        };
        let Some(usage) = self.usage else {
            // 🚨 `stream_options.include_usage` was asked for and ignored. A turn
            // costing zero tokens is a silent under-report of every accounting
            // built on top of it, so this stops rather than rounds down.
            self.out.push(Err(ProviderError::Malformed {
                detail: "the server ended the stream without usage, so this turn's \
                         cost is unknown; stream_options.include_usage was ignored"
                    .to_owned(),
            }));
            return;
        };
        self.out.push(Ok(Delta::Closed {
            usage,
            finish: finish(&reason, self.said_something),
        }));
    }
}

/// The finish reason, and the one field in it that matters.
///
/// 🚨 `length` with nothing in the payload is an **absence**: 17 of 57 judge
/// calls were lost that way, and none of them is a zero.
fn finish(reason: &str, said_something: bool) -> Finish {
    match reason {
        "stop" => Finish::Stop,
        "length" => Finish::Length {
            content_empty: !said_something,
        },
        "tool_calls" | "function_call" => Finish::ToolCalls,
        other => Finish::Truncated {
            detail: format!("the server finished with {other:?}"),
        },
    }
}

// ---------------------------------------------------------------------------
// The reply that was not a stream
// ---------------------------------------------------------------------------

/// The fallback the donor already has: a `stream: true` request answered without
/// an SSE content type, parsed whole.
///
/// 🚨 **The same number means something different on this path, and the two call
/// sites look identical.** A server that does not stream writes nothing until the
/// generation is finished, so there is exactly one gap to measure and it covers
/// the whole turn: the rung's 90 s is an idle gap on the streaming path and a
/// **total budget** here. That is the donor's behaviour too, and F198 is where
/// the two meanings of one constant were first written down.
fn whole(response: Response, provider: &str, started: Instant, out: Out<'_>) {
    let mut deltas: Vec<Result<Delta, ProviderError>> = Vec::new();
    let body = response.text();
    // ⚠ On this path TTFB and total elapsed are the same number, because the
    // server wrote nothing until it had finished. It is reported rather than
    // dressed up to look like a stream's.
    deltas.push(Ok(Delta::Opened {
        ttfb_ms: millis(started.elapsed()),
    }));

    match body.map(|body| serde_json::from_str::<Reply>(&body)) {
        Err(e) => deltas.push(Err(ProviderError::Transport {
            provider: provider.to_owned(),
            detail: e.to_string(),
        })),
        Ok(Err(e)) => deltas.push(Err(ProviderError::Malformed {
            detail: format!(
                "the reply carried no SSE content type and did not parse as a \
                 completion either: {e}"
            ),
        })),
        Ok(Ok(reply)) => replay(reply, &mut deltas),
    }
    for delta in deltas {
        if out.send(delta).is_err() {
            return;
        }
    }
}

/// Turn a whole completion into the delta sequence a stream would have produced.
fn replay(reply: Reply, deltas: &mut Vec<Result<Delta, ProviderError>>) {
    let mut said_something = false;
    let mut reason = None;
    for choice in reply.choices {
        if let Some(trace) = choice.message.reasoning_content
            && !trace.is_empty()
        {
            deltas.push(Ok(Delta::Reasoning(trace)));
        }
        if let Some(text) = choice.message.content
            && !text.is_empty()
        {
            said_something = true;
            deltas.push(Ok(Delta::Text(text)));
        }
        for call in choice.message.tool_calls {
            let function = call.function.unwrap_or_default();
            deltas.push(Ok(Delta::ToolCall(ToolCall {
                id: call.id.unwrap_or_default(),
                tool: function.name.unwrap_or_default(),
                arguments: function.arguments.unwrap_or_default(),
            })));
        }
        reason = reason.or(choice.finish_reason);
    }
    match (reason, reply.usage) {
        (Some(reason), Some(usage)) => deltas.push(Ok(Delta::Closed {
            usage: usage.into(),
            finish: finish(&reason, said_something),
        })),
        (Some(_), None) => deltas.push(Err(ProviderError::Malformed {
            detail: "the reply reported no usage, so this turn's cost is unknown".to_owned(),
        })),
        // No finish reason: the loop names it rather than this guessing.
        (None, _) => {}
    }
}

// ---------------------------------------------------------------------------
// The request
// ---------------------------------------------------------------------------

/// Turn a base URL into the completions endpoint, whichever depth it was given.
fn completions_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_owned()
    } else if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

/// The request body.
///
/// 🚨 **Everything in it that is not the varying body is constant per head**, so
/// asking for tools costs no prefix-cache hit: the array below comes from
/// [`Posting::tools`], which is `Policy::admitted`, which is fixed at compile
/// time.
/// One token changed at the *front* annihilates the 79.7% TTFT saving (F81), and
/// a tool array that mutated mid-session would pay a 33.9 s cold prefill at the
/// daily driver's context to buy exactly the one turn that changed it.
fn payload(req: &ApiRequest<'_>) -> Value {
    let mut messages = Vec::with_capacity(req.body.len() + 1);
    // The system half is the head and arrives only here. `Role` has no `System`
    // variant precisely so nothing else can put one in.
    messages.push(json!({"role": "system", "content": req.posting.prefix()}));
    messages.extend(req.body.messages().iter().map(wire_message));

    let mut payload = Map::new();
    payload.insert("model".to_owned(), json!(req.model));
    payload.insert("messages".to_owned(), Value::Array(messages));
    payload.insert("max_tokens".to_owned(), json!(req.budget()));
    // 🚨 The one sampling field this engine sets, and it sets it so that a
    // run can be re-flown rather than to move the distribution. Without it the
    // champion answers five identical requests five different ways; with it,
    // one. Sampling is otherwise the server's, which is where every number this
    // project has quoted was taken.
    payload.insert("seed".to_owned(), json!(req.seed));
    payload.insert("stream".to_owned(), json!(true));
    // F84: the compat dialect reports real token counts only when asked.
    payload.insert("stream_options".to_owned(), json!({"include_usage": true}));

    let tools = advertised(req.posting);
    if !tools.is_empty() {
        payload.insert("tools".to_owned(), Value::Array(tools.to_vec()));
    }

    if let Some(schema) = req.schema {
        payload.insert(
            "response_format".to_owned(),
            json!({
                "type": "json_schema",
                "json_schema": {
                    "name": schema.name,
                    "strict": true,
                    "schema": parsed(schema.json),
                }
            }),
        );
    }

    Value::Object(payload)
}

/// One message on the wire.
fn wire_message(message: &Message) -> Value {
    let mut wire = Map::new();
    wire.insert(
        "role".to_owned(),
        json!(match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }),
    );
    wire.insert("content".to_owned(), json!(message.content));
    if let Some(id) = &message.tool_call_id {
        wire.insert("tool_call_id".to_owned(), json!(id));
    }
    if !message.tool_calls.is_empty() {
        wire.insert(
            "tool_calls".to_owned(),
            Value::Array(
                message
                    .tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": {"name": call.tool, "arguments": call.arguments},
                        })
                    })
                    .collect(),
            ),
        );
    }
    Value::Object(wire)
}

/// The `tools` array for a posting, built once.
///
/// Nine postings, nine arrays, assembled on first use and never again — the same
/// argument as [`Posting::prefix`], for the same reason: the server renders this
/// into its chat template, so a value rebuilt per call is a head that varies.
///
/// 🚨 It is keyed by **posting** and not by head, so that a slot cap narrows the
/// wire-level tool array and the prompt text together. They are two renderings
/// of one list, and a cap that reached only one of them would advertise a tool
/// on the wire that the prose says the role does not have.
static ADVERTISED: LazyLock<Vec<Vec<Value>>> =
    LazyLock::new(|| Posting::ALL.iter().map(|p| advertise(*p)).collect());

fn advertise(posting: Posting) -> Vec<Value> {
    posting
        .tools()
        .into_iter()
        .map(|spec| {
            json!({
                "type": "function",
                "function": {
                    "name": spec.name,
                    "description": spec.summary,
                    "parameters": parsed(spec.schema),
                }
            })
        })
        .collect()
}

fn advertised(posting: Posting) -> &'static [Value] {
    &ADVERTISED[posting.index()]
}

/// Inline a schema that is already JSON.
///
/// # Panics
///
/// If a `&'static str` schema in this workspace is not valid JSON.
/// `tests/http.rs` drives every registry entry through this, so a malformed one
/// fails the suite rather than a request.
fn parsed(schema: &'static str) -> Value {
    serde_json::from_str(schema).expect("a schema in this workspace is not valid JSON")
}

// ---------------------------------------------------------------------------
// The wire shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<ApiUsage>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: ChoiceDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct ChoiceDelta {
    #[serde(default)]
    content: Option<String>,
    /// The reasoning trace, in the field the donor reads on both of its parsing
    /// paths (F388).
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallFragment>,
}

#[derive(Deserialize)]
struct ToolCallFragment {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionFragment>,
}

#[derive(Deserialize, Default)]
struct FunctionFragment {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    choices: Vec<WholeChoice>,
    #[serde(default)]
    usage: Option<ApiUsage>,
}

#[derive(Deserialize)]
struct WholeChoice {
    #[serde(default)]
    message: WholeMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct WholeMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallFragment>,
}

#[derive(Deserialize)]
struct ApiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    completion_tokens_details: Option<TokenDetails>,
    #[serde(default)]
    prompt_tokens_details: Option<TokenDetails>,
}

#[derive(Deserialize, Default)]
struct TokenDetails {
    #[serde(default)]
    reasoning_tokens: Option<u32>,
    #[serde(default)]
    cached_tokens: Option<u32>,
}

impl From<ApiUsage> for Usage {
    fn from(api: ApiUsage) -> Usage {
        Usage {
            prompt_tokens: api.prompt_tokens,
            completion_tokens: api.completion_tokens,
            // ⚠ `None` where the server did not report one, which is not zero.
            reasoning_tokens: api
                .completion_tokens_details
                .and_then(|d| d.reasoning_tokens),
            // The number that would say whether the frozen head is doing its
            // job. ⚠ Measured against the champion on LM Studio: it is **not
            // reported** — `prompt_tokens_details` is absent, so this is always
            // `None` there. The prefix cache is still working and is visible as
            // TTFB (1,034 ms cold, 146 ms on the same 495-token prefix), which
            // means the head's payoff has to be measured rather than counted.
            cached_tokens: api.prompt_tokens_details.and_then(|d| d.cached_tokens),
        }
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}
