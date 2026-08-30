//! The breaker: **a report, and never a gate.**
//!
//! ADR-0010 §4. It reads the event log rather than a field on a task, its
//! threshold is learned from the population rather than written down, and it
//! ends in a sentence an operator can act on. Nothing here returns a decision,
//! and there is deliberately no function on [`Report`] that answers *should the
//! fleet stop* — that is ADR-0008's ruling applied to a statistical verdict
//! instead of a model's, for the same reason: **a report costs attention and a
//! false block costs trust.**
//!
//! # The update rule, not the number (F374)
//!
//! The quantity worth having is not the pass rate. It is the value of the *next*
//! attempt given the ones already burnt, because those failures are evidence
//! about which task this is. With `p_t` a task's measured pass rate and the
//! population as the prior:
//!
//! ```text
//! P(pass at k+1 | the first k all failed) = Σ p_t (1−p_t)^k ⁄ Σ (1−p_t)^k
//! ```
//!
//! 🚨 **And the size of that update is a property of the population, not a
//! constant.** One observed failure takes Q56 from **88.2% to 43.6%** — 44.6
//! points — because Q56 is bimodal, 40 tasks at `p = 1.0` and one at `p = 0`, so
//! a single failure is strong evidence you are on one of the fifteen hard ones.
//! U100's tasks sit at middling probabilities and the same failure moves the
//! estimate **under 7 points** (88.8% → 81.9%). A constant *retry twice then
//! escalate* is therefore right on one corpus and wrong on the other. So this
//! module ships the rule and computes the number from whatever history the log
//! actually holds.
//!
//! # 🚨 An absence is not a failure, and this is where that matters most
//!
//! [`AttemptOutcome::Uncertain`] is excluded from the rate rather than counted
//! against it, and the exclusion is not fastidiousness — it is the difference
//! between measuring the work and measuring the instrument.
//!
//! F539's wedge left `/v1/models` answering normally while **every completion
//! returned nothing for 60 s**. Every attempt in that window ends
//! `SaidNothing` or `Timeout`. Count those as failures and the observed pass
//! rate collapses to zero, and the breaker reports *these tasks are hard* about
//! a server that is not answering at all — the one conclusion that is certainly
//! wrong, arrived at with a large sample.
//!
//! ▶ **A rate cannot tell a hard task from a dead server. Only the pulse can**,
//! which is why F539 says the breaker's input must be a real one-token
//! completion and why [`Report`] carries both halves and prints the absences
//! next to the rate rather than folded into it.

use abcc_core::attempt::AttemptOutcome;
use abcc_core::event::Event;
use abcc_core::seq::{Seq, TaskId};
use abcc_store::{Store, StoreError};

use std::fmt;
use std::fmt::Write as _;

/// How many events one page of the fold reads. The log is read whole; this is
/// the page size, not a window — a breaker that quietly looked at the last N
/// events would have a hidden constant in exactly the place ADR-0010 §4 says
/// there must not be one.
const PAGE: usize = 4096;

/// One task's measured history, as the log holds it.
///
/// Three counters and not two: `absent` is kept apart from `failed` because the
/// two are different sentences, and folding them makes a wedged server look like
/// a corpus of impossible tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub task: TaskId,
    /// Attempts the gate measured and passed.
    pub passed: u32,
    /// Attempts that failed in a way something watched: a soft or hard failure,
    /// or a deterministic rung's refusal.
    pub failed: u32,
    /// 🚨 Attempts that produced **no measurement at all** — the model said
    /// nothing, the window filled, a checker was not on the host. Not a score,
    /// and so not in the rate.
    pub absent: u32,
}

impl Record {
    /// Attempts that produced a measurement, which is the denominator of
    /// anything honest.
    #[must_use]
    pub const fn measured(&self) -> u32 {
        self.passed + self.failed
    }

    /// This task's `p_t`. `None` when nothing about it was ever measured, which
    /// is different from a rate of zero.
    #[must_use]
    pub fn rate(&self) -> Option<f64> {
        (self.measured() > 0).then(|| f64::from(self.passed) / f64::from(self.measured()))
    }
}

/// The prior, folded out of the log: one measured pass rate per task that has
/// one.
///
/// 🚨 It is a projection and is held nowhere. The board, admission and this are
/// three folds of the same events, so none of them can drift from the others.
#[derive(Debug, Clone, Default)]
pub struct Population {
    records: Vec<Record>,
}

impl Population {
    /// Fold every `AttemptEnded` on the log into one record per task.
    ///
    /// # Errors
    ///
    /// [`StoreError`] if the log will not read.
    pub fn of(store: &Store) -> Result<Population, StoreError> {
        let mut records: Vec<Record> = Vec::new();
        let mut cursor = Seq::new(0);
        loop {
            let page = store.read_from(cursor, PAGE)?;
            if page.is_empty() {
                break;
            }
            for logged in &page {
                cursor = logged.seq;
                let Event::AttemptEnded { task, outcome, .. } = &logged.event else {
                    continue;
                };
                if !records.iter().any(|r| r.task == *task) {
                    records.push(Record {
                        task: *task,
                        passed: 0,
                        failed: 0,
                        absent: 0,
                    });
                }
                let Some(record) = records.iter_mut().find(|r| r.task == *task) else {
                    // Unreachable: the push above is unconditional on the miss.
                    // It answers rather than panicking for the same reason
                    // `NoTools::run` does.
                    continue;
                };
                match outcome {
                    AttemptOutcome::Success => record.passed += 1,
                    // A refusal is a measurement: the host watched a checker run
                    // and watched it say no. It belongs in the rate.
                    AttemptOutcome::SoftFailure { .. }
                    | AttemptOutcome::HardFailure { .. }
                    | AttemptOutcome::Refused { .. } => record.failed += 1,
                    AttemptOutcome::Uncertain { .. } => record.absent += 1,
                }
            }
        }
        Ok(Population { records })
    }

    /// Every task with at least one measured attempt.
    #[must_use]
    pub fn measured_tasks(&self) -> Vec<Record> {
        self.records
            .iter()
            .copied()
            .filter(|r| r.measured() > 0)
            .collect()
    }

    /// Tasks the log knows an ending for, measured or not.
    #[must_use]
    pub fn tasks(&self) -> usize {
        self.records.len()
    }

    /// Attempts that produced a measurement.
    #[must_use]
    pub fn measured_attempts(&self) -> u32 {
        self.records.iter().map(Record::measured).sum()
    }

    /// 🚨 Attempts that produced none. Reported beside every rate below it, and
    /// never inside one.
    #[must_use]
    pub fn absences(&self) -> u32 {
        self.records.iter().map(|r| r.absent).sum()
    }

    /// **F374's rule.** `P(pass at k+1 | the first k all failed)`, over this
    /// population as the prior.
    ///
    /// `None` when no task here has a measured outcome — a population of nothing
    /// has no rate, and returning `0.0` would be a number where there is no
    /// measurement.
    ///
    /// ⚠ The denominator `Σ (1−p_t)^k` goes to zero as `k` grows if every task
    /// passed every time, because such a population assigns `k` straight
    /// failures probability zero. That is the honest answer — *this population
    /// has never seen what you are asking about* — so it is `None` rather than a
    /// ratio of two zeros.
    #[must_use]
    pub fn pass_after(&self, failures: u32) -> Option<f64> {
        let mut numerator = 0.0;
        let mut denominator = 0.0;
        for record in &self.records {
            let Some(p) = record.rate() else { continue };
            let weight = (1.0 - p).powi(i32::try_from(failures).unwrap_or(i32::MAX));
            numerator += p * weight;
            denominator += weight;
        }
        (denominator > 0.0).then(|| numerator / denominator)
    }

    /// The unconditional rate — the same rule at `k = 0`, which is the mean of
    /// the per-task rates rather than the pooled one. Spelled out because it is
    /// the row every other row is read against.
    #[must_use]
    pub fn unconditional(&self) -> Option<f64> {
        self.pass_after(0)
    }

    /// 🚨 **Whether a third attempt is a per-population decision or not**
    /// (ADR-0010 §3): the share of measured tasks sitting at 0 or 1 rather than
    /// in between.
    ///
    /// Q56 is bimodal — 40 of 56 pass all five times and one passes none — and
    /// on a bimodal population the retry budget can only change the outcome on
    /// the middle. `None` when nothing is measured.
    #[must_use]
    pub fn bimodal_share(&self) -> Option<f64> {
        let measured = self.measured_tasks();
        if measured.is_empty() {
            return None;
        }
        let extreme = measured
            .iter()
            .filter_map(Record::rate)
            .filter(|p| *p <= f64::EPSILON || *p >= 1.0 - f64::EPSILON)
            .count();
        Some(ratio(extreme, measured.len()))
    }
}

/// A share, as a fraction. ⚠ The cast is lossless for any log this project will
/// ever hold — `f64` carries every integer to 2^53 — and it is written out here
/// rather than sprinkled with an allow, so the assumption is in one place.
#[allow(clippy::cast_precision_loss)]
fn ratio(part: usize, whole: usize) -> f64 {
    part as f64 / whole as f64
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// What one real one-token completion did.
///
/// 🚨 **This is the breaker's other input and it is not optional** (F539). A
/// health check that polls `/v1/models` is the check that missed the only real
/// outage this project has had: the listing answered normally for 60 s while
/// every completion returned nothing, and it took both slots down until
/// `lms load`.
///
/// ⚠ It is deliberately a *measurement* and not a verdict. The host asked for one
/// token and watched what happened, which is the same shape as a rung — and
/// unlike the rate above, it can tell a dead server from a hard task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pulse {
    /// The server generated at least one token, and this is how long it took.
    ///
    /// 🚨 **The health question is `tokens >= 1`, not whether prose came back**
    /// (F550). The champion is a reasoning model: at `max_tokens` of 1, 8 and 64
    /// it returns `content: ""` and puts every token in `reasoning_content`, so
    /// a pulse that read the answer text would call a healthy, idle, correctly
    /// loaded server **silent** — measured three times on this box before this
    /// arm was written. Waiting for prose is not an option either: the trace
    /// runs 9,942–16,564 characters on identical input (F246), so the cheap
    /// probe would become an expensive one.
    Answered {
        elapsed_ms: u64,
        /// What the server said it generated. This is the measurement.
        tokens: u32,
        /// Which channel carried it, and a little of what it said.
        sample: Sample,
    },
    /// The server was reachable and produced no token.
    ///
    /// This is F539's shape exactly, and it is its own variant rather than an
    /// error because *the server answered and said nothing* and *the server was
    /// not there* are two different sentences with two different fixes.
    Silent { after_ms: u64, detail: String },
    /// The request never completed: refused, timed out at the socket, HTTP
    /// non-200.
    Unreachable { after_ms: u64, detail: String },
    /// No pulse was taken. `abcc breaker --no-pulse`, or a report built from a
    /// log with no server to ask.
    ///
    /// ⚠ Present so that *not asked* cannot be rendered as *answered*. A missing
    /// measurement that prints like a passing one is the failure this whole
    /// codebase is organised against.
    NotTaken,
}

/// What came back with the token, and down which channel.
///
/// ⚠ Three arms rather than a string, because *the model answered* and *the
/// model generated* are two different sentences and this project has a whole
/// `Why` variant about the gap between them (F497). A health check only needs
/// the second; an operator reading the line wants to know which one it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sample {
    /// Prose in `message.content`.
    Answer(String),
    /// 🚨 Only `reasoning_content`. **On the champion a short completion always
    /// lands here**, which is the normal case rather than a degraded one.
    Reasoning(String),
    /// The usage count says a token was generated and no text came with it.
    CountOnly,
}

impl fmt::Display for Sample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Sample::Answer(text) => write!(f, "{:?}", clip(text)),
            Sample::Reasoning(text) => write!(f, "{:?}, reasoning only", clip(text)),
            Sample::CountOnly => f.write_str("no text, the count only"),
        }
    }
}

/// Enough of a sample to recognise it, and no more.
fn clip(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= 40 {
        return text.to_owned();
    }
    let head: String = text.chars().take(40).collect();
    format!("{head}\u{2026}")
}

impl Pulse {
    /// Whether the server generated a token. ⚠ **Not a gate** — it is what the
    /// report prints, and the operator is the one who acts on it.
    #[must_use]
    pub const fn answered(&self) -> bool {
        matches!(self, Pulse::Answered { .. })
    }
}

impl fmt::Display for Pulse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pulse::Answered {
                elapsed_ms,
                tokens,
                sample,
            } => write!(f, "{tokens} token(s) in {elapsed_ms} ms \u{2014} {sample}"),
            Pulse::Silent { after_ms, detail } => write!(
                f,
                "reachable and SILENT after {after_ms} ms \u{2014} {detail}"
            ),
            Pulse::Unreachable { after_ms, detail } => {
                write!(f, "unreachable after {after_ms} ms \u{2014} {detail}")
            }
            Pulse::NotTaken => f.write_str("not taken"),
        }
    }
}

/// The breaker's whole output: two inputs, some arithmetic, and no decision.
#[derive(Debug, Clone)]
pub struct Report {
    pub pulse: Pulse,
    pub population: Population,
    /// The rows of F374's rule, `k = 0..=depth`.
    pub depth: u32,
}

impl Report {
    #[must_use]
    pub fn new(pulse: Pulse, population: Population) -> Report {
        Report {
            pulse,
            population,
            depth: 4,
        }
    }

    /// `(failures so far, P(the next one passes))`, `None` where this population
    /// has never seen that many failures in a row.
    #[must_use]
    pub fn rows(&self) -> Vec<(u32, Option<f64>)> {
        (0..=self.depth)
            .map(|k| (k, self.population.pass_after(k)))
            .collect()
    }

    /// 🚨 **The sentence that says how much the arithmetic above is worth.**
    ///
    /// ⚠ F544's lesson generalised: a zero from an unvalidated instrument is not
    /// a measurement, so the sample count is printed before anything is read off
    /// the table — and below a handful of measured tasks the honest answer is
    /// that this is arithmetic and not an estimate.
    #[must_use]
    pub fn standing(&self) -> String {
        let tasks = self.population.measured_tasks().len();
        let attempts = self.population.measured_attempts();
        let absent = self.population.absences();
        let mut said = match tasks {
            0 => "nothing on this log has a measured outcome, so there is no population and \
                  no rate"
                .to_owned(),
            1..=4 => format!(
                "{tasks} measured task(s) over {attempts} attempt(s) \u{2014} this is arithmetic, \
                 not an estimate"
            ),
            _ => format!("{tasks} measured tasks over {attempts} attempts"),
        };
        if absent > 0 {
            let _ = write!(
                said,
                "; {absent} attempt(s) produced no measurement and are NOT in the rate"
            );
        }
        if absent > 0 && !self.pulse.answered() {
            said.push_str(
                ". \u{1f6a8} Absences with no pulse is F539's shape: a rate cannot tell a hard \
                 task from a server that is not answering",
            );
        }
        said
    }
}
