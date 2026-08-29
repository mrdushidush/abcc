//! A provider that replays a written script.
//!
//! ⚠ **Not a mock.** It is two things the project needs on its own terms.
//!
//! 1. It is how the turn loop is exercised without a GPU. The box has one card,
//!    a model swap costs 23.77 s and the quality champion runs at 4.9× — so a
//!    test suite that needs a resident model is a test suite nobody runs before
//!    committing.
//! 2. **It is the contract the HTTP provider has to satisfy.** The [`Delta`]
//!    sequence here is the sequence a real stream produces; if writing the real
//!    one requires a shape this cannot express, the seam is wrong and that is
//!    worth finding out in a test rather than at a socket.
//!
//! It also earns its keep as a check on ADR-0013's `&self`: this type is shared,
//! not owned, and it holds its script behind a lock rather than behind `&mut`.
//! A provider that needed `&mut self` could not be written this way, which is the
//! property the inherited trait lacked.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use abcc_core::event::{Finish, Usage};

use crate::provider::{
    ApiRequest, Delta, Message, Provider, ProviderClass, ProviderError, ProviderId, Schema,
    ToolCall, TurnStream,
};

/// One turn's worth of deltas, in the order a stream produces them.
#[derive(Debug, Clone, Default)]
pub struct Script(Vec<Result<Delta, ProviderError>>);

impl Script {
    /// A turn that answers and asks for nothing.
    #[must_use]
    pub fn says(text: &str) -> Script {
        Script(vec![
            Ok(Delta::Opened { ttfb_ms: 12 }),
            Ok(Delta::Text(text.to_owned())),
            Ok(Delta::Closed {
                usage: usage(64, u32::try_from(text.len() / 4).unwrap_or(1), None),
                finish: Finish::Stop,
            }),
        ])
    }

    /// A turn that asks for one tool.
    #[must_use]
    pub fn calls(id: &str, tool: &str, arguments: &str) -> Script {
        Script(vec![
            Ok(Delta::Opened { ttfb_ms: 12 }),
            Ok(Delta::ToolCall(ToolCall {
                id: id.to_owned(),
                tool: tool.to_owned(),
                arguments: arguments.to_owned(),
            })),
            Ok(Delta::Closed {
                usage: usage(64, 20, None),
                finish: Finish::ToolCalls,
            }),
        ])
    }

    /// 🚨 The ending that is an absence: the cap, with nothing in the payload.
    /// 17 of 57 judge calls were lost this way, and none of them is a zero.
    #[must_use]
    pub fn truncated_at_cap(budget: u32) -> Script {
        Script(vec![
            Ok(Delta::Opened { ttfb_ms: 12 }),
            Ok(Delta::Reasoning("thinking, at length, ".repeat(64))),
            Ok(Delta::Closed {
                usage: usage(64, budget, Some(budget)),
                finish: Finish::Length {
                    content_empty: true,
                },
            }),
        ])
    }

    /// 🚨 **F511's shape, and it is the one the live runs actually produced.**
    ///
    /// A turn that spends its whole budget assembling **one enormous tool call**
    /// and is cut mid-argument. Five consecutive Change phases ended here, each
    /// with 89–204 characters of answer, 359–1,811 of trace, and 94–98% of the
    /// completion in arguments — which the usage block reports as a single
    /// number and therefore cannot tell apart from a model writing an essay.
    ///
    /// ⚠ `content_empty: false`, because there *was* a little text: this is the
    /// case F506 widened `TruncatedAtCap` to cover, and the case that fell
    /// through both guards before it did.
    #[must_use]
    pub fn cut_assembling_a_call(budget: u32, tool: &str, arguments: &str) -> Script {
        Script(vec![
            Ok(Delta::Opened { ttfb_ms: 12 }),
            Ok(Delta::Reasoning("weighing it up ".repeat(8))),
            Ok(Delta::Text("Writing the file now.".to_owned())),
            Ok(Delta::ToolCall(ToolCall {
                id: "call-cut".to_owned(),
                tool: tool.to_owned(),
                arguments: arguments.to_owned(),
            })),
            Ok(Delta::Closed {
                usage: usage(64, budget, Some(96)),
                finish: Finish::Length {
                    content_empty: false,
                },
            }),
        ])
    }

    /// A turn whose reasoning trace is still open when the answer is due — the
    /// signal ADR-0010 §7 asks for at token 200.
    ///
    /// 🚨 **F502: it cannot be given an answer without destroying the signal.**
    /// The first `Delta::Text` closes the trace, so `OpenAt200` is reachable
    /// only by a turn that reasoned and then produced nothing — which is F497's
    /// shape with a completion count above 200. The signal is therefore a
    /// *weaker* detector of the same condition, and on the first clean run it
    /// would have fired for Builders (582) and missed Recon (47) altogether.
    #[must_use]
    pub fn trace_still_open(completion_tokens: u32) -> Script {
        Script(vec![
            Ok(Delta::Opened { ttfb_ms: 12 }),
            Ok(Delta::Reasoning("still going ".repeat(200))),
            Ok(Delta::Closed {
                usage: usage(64, completion_tokens, Some(completion_tokens)),
                finish: Finish::Stop,
            }),
        ])
    }

    /// 🚨 **F497, as it actually arrived**: the budget goes into the trace, the
    /// model stops cleanly, and the payload is empty.
    ///
    /// Both `ClaimRecorded` events on the first clean run were zero characters —
    /// Recon at completion 47 / reasoning 42, Builders at 582 / 577 — so the
    /// numbers here are the shape rather than an invention.
    #[must_use]
    pub fn all_trace_no_answer(completion_tokens: u32, reasoning_tokens: u32) -> Script {
        Script(vec![
            Ok(Delta::Opened { ttfb_ms: 12 }),
            Ok(Delta::Reasoning("thinking ".repeat(64))),
            Ok(Delta::Closed {
                usage: usage(64, completion_tokens, Some(reasoning_tokens)),
                finish: Finish::Stop,
            }),
        ])
    }

    /// 🚨 **F496, as it actually arrived**: the conversation reached the
    /// server's window, so generation stopped at `length` having spent *fewer*
    /// completion tokens than the cap we sent.
    ///
    /// Run 1: 14,261 prompt + 2,123 completion = 16,384 exactly, against a
    /// budget of 8,192. That inequality is the whole discriminator (F498) — with
    /// `completion >= budget` this is our own cap and an ordinary truncation.
    #[must_use]
    pub fn filled_the_window(prompt_tokens: u32, completion_tokens: u32) -> Script {
        Script(vec![
            Ok(Delta::Opened { ttfb_ms: 12 }),
            Ok(Delta::Text("half a th".to_owned())),
            Ok(Delta::Closed {
                usage: usage(prompt_tokens, completion_tokens, Some(0)),
                finish: Finish::Length {
                    content_empty: false,
                },
            }),
        ])
    }

    /// A stream that ends without a finish reason — a proxy that answered 200 and
    /// then nothing.
    #[must_use]
    pub fn ends_without_saying_so() -> Script {
        Script(vec![
            Ok(Delta::Opened { ttfb_ms: 12 }),
            Ok(Delta::Text("half an ans".to_owned())),
        ])
    }

    /// A stream that fails partway.
    #[must_use]
    pub fn fails(error: ProviderError) -> Script {
        Script(vec![Ok(Delta::Opened { ttfb_ms: 12 }), Err(error)])
    }

    /// Anything else.
    #[must_use]
    pub fn raw(deltas: Vec<Result<Delta, ProviderError>>) -> Script {
        Script(deltas)
    }

    /// Add a delta to the end of this turn.
    #[must_use]
    pub fn and(mut self, delta: Delta) -> Script {
        self.0.push(Ok(delta));
        self
    }

    /// Report reasoning tokens on this turn, which `PLAN.md` §5 requires logged
    /// on every call from day one.
    #[must_use]
    pub fn with_reasoning_tokens(mut self, n: u32) -> Script {
        for delta in &mut self.0 {
            if let Ok(Delta::Closed { usage, .. }) = delta {
                usage.reasoning_tokens = Some(n);
            }
        }
        self
    }
}

#[must_use]
fn usage(prompt: u32, completion: u32, reasoning: Option<u32>) -> Usage {
    Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        reasoning_tokens: reasoning,
        cached_tokens: None,
    }
}

/// What the loop actually sent. The point of keeping it is that the freeze is
/// checkable end to end: a head that moved between rounds shows up here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub head_key: &'static str,
    pub head_prefix: &'static str,
    pub model: String,
    pub budget: u32,
    pub messages: Vec<Message>,
    /// The constrained-output contract this call was made under, if any.
    ///
    /// Kept for the same reason the head prefix is: it is part of the request
    /// and it is the part a caller can get wrong invisibly. A phase whose
    /// artifact has a declared shape and that sends `None` produces prose that
    /// parses today and does not tomorrow, and the failure would look like the
    /// model's.
    pub schema: Option<Schema>,
}

/// A provider that replays [`Script`]s in order.
pub struct Scripted {
    id: ProviderId,
    class: ProviderClass,
    scripts: Mutex<VecDeque<Script>>,
    seen: Mutex<Vec<Seen>>,
    /// Time to wait before each delta, so a test can interrupt a stream that is
    /// genuinely in flight rather than one that has already finished.
    pace: Duration,
}

impl Scripted {
    #[must_use]
    pub fn new(scripts: Vec<Script>) -> Scripted {
        Scripted {
            id: ProviderId::new("scripted"),
            class: ProviderClass::Local,
            scripts: Mutex::new(scripts.into()),
            seen: Mutex::new(Vec::new()),
            pace: Duration::ZERO,
        }
    }

    /// Answer as a cloud provider, so an egress predicate has something to be
    /// true about.
    #[must_use]
    pub fn cloud(mut self) -> Scripted {
        self.class = ProviderClass::Cloud;
        self.id = ProviderId::new("scripted-cloud");
        self
    }

    /// Space the deltas out.
    #[must_use]
    pub fn paced(mut self, pace: Duration) -> Scripted {
        self.pace = pace;
        self
    }

    /// Every request this provider was given, in order.
    ///
    /// # Panics
    ///
    /// If a previous caller panicked while holding the lock.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("scripted provider lock").clone()
    }

    /// Scripts not yet played.
    ///
    /// # Panics
    ///
    /// If a previous caller panicked while holding the lock.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.scripts.lock().expect("scripted provider lock").len()
    }
}

impl Provider for Scripted {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn class(&self) -> ProviderClass {
        self.class
    }

    fn start(&self, req: &ApiRequest<'_>) -> Result<Box<dyn TurnStream>, ProviderError> {
        self.seen.lock().expect("lock").push(Seen {
            head_key: req.head.key(),
            head_prefix: req.head.prefix(),
            model: req.model.to_owned(),
            budget: req.budget(),
            messages: req.body.messages().to_vec(),
            schema: req.schema,
        });
        let script = self
            .scripts
            .lock()
            .expect("lock")
            .pop_front()
            .ok_or_else(|| ProviderError::Malformed {
                detail: "the script ran out of turns".to_owned(),
            })?;
        Ok(Box::new(ScriptedStream {
            deltas: script.0.into(),
            pace: self.pace,
        }))
    }
}

struct ScriptedStream {
    deltas: VecDeque<Result<Delta, ProviderError>>,
    pace: Duration,
}

impl TurnStream for ScriptedStream {
    fn next_delta(&mut self) -> Option<Result<Delta, ProviderError>> {
        if !self.pace.is_zero() {
            std::thread::sleep(self.pace);
        }
        self.deltas.pop_front()
    }
}
