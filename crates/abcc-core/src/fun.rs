//! ADR-0012 §5 — **fun is six queries over the event log**, and three of them
//! have nothing to fold.
//!
//! §5 writes six product constraints down as observables with bars, because a
//! feeling nobody can query is a feeling nobody can hold a milestone to. This
//! module is those six. Writing them answered a question the ADR did not ask —
//! *which of the six the log can actually produce* — and the answer is not six:
//!
//! | §5 constraint | here | state |
//! |---|---|---|
//! | No dead air | [`DeadAir`] | measured, with the two limits below |
//! | Speed where it is felt | [`Speed`] | measured, to the log's edge |
//! | Legibility | [`Fun::legibility`] | 🚨 **no instrument** |
//! | Agency | [`Agency`] | measured |
//! | Personality with an off switch | [`Fun::personality`] | 🚨 **no instrument** |
//! | Honest failure | [`HonestFailure`] | denominator only |
//!
//! # The three that have no instrument
//!
//! 🚨 **They return [`Answer::NoInstrument`] and never `0`.** All three ask what
//! the operator did *at the console* — which screen, which theme, which scrub
//! position — and `abcc-tui` does not depend on `abcc-store`. That is not an
//! omission: the console is a reader by construction (ADR-0012, and the rule in
//! `CLAUDE.md` that the reader reads the log and never the projection), so
//! installing these three means **making the reader a writer**. That is an
//! ADR-level decision and not a missing `match` arm, which is exactly why the
//! answer here is a named absence rather than a plausible zero.
//!
//! A zero would be worse than no answer. *Screen switches before the first
//! operator command: 0* reads as **perfect legibility** and means **nothing is
//! counting**, and this project has paid for that shape often enough to give it
//! a type.
//!
//! # The two limits on dead air, which the bar's own author would want said
//!
//! 1. 🚨 **A fold sees only silences that ended.** A gap needs an event on both
//!    sides. The forty-minute hang W5 measured — 240× the attention limit, the
//!    line ADR-0012 §5 calls the most actionable in W5 — writes *no* second
//!    event, so a pure fold over the log cannot see the very silence the bar
//!    exists for. Seeing it needs the wall clock, which is not on the log:
//!    [`Fun::open_silence`] takes `now` for exactly that reason, and it is the
//!    half of the query that cannot be a fold.
//! 2. ⚠ **A run's end is not on the log.** [`Event::RunStarted`] has no
//!    counterpart, so the events after a run exits and before the next one
//!    starts are, on the log, indistinguishable from events inside it — and an
//!    `abcc accept` typed an hour later would read as an hour of dead air. So a
//!    run's span is bounded here at its last **in-flight** event ([`in_flight`]),
//!    the class only a running process writes, rather than at the next
//!    `RunStarted`.

use crate::attempt::AttemptOutcome;
use crate::event::{Event, Logged};
use crate::seq::Seq;

/// W5's bar, in milliseconds: **no gap over ten seconds goes unmarked**.
///
/// ADR-0012 §5 calls this the single most actionable line in W5. The donor
/// harness once sat silent for forty minutes — 240× the attention limit — and
/// could not tell that from a hang.
pub const SILENCE_BAR_MS: i64 = 10_000;

/// ADR-0012 §5's bar between an operator's command and the first thing they see
/// back: **under a second, independent of TTFT**.
pub const RESPONSE_BAR_MS: i64 = 1_000;

/// A measurement the log cannot produce, named by the event that does not exist.
///
/// 🚨 This type exists so that *nothing is counting* cannot be reported as a
/// count. Each variant names the event that would have to be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// Which screen the console is showing, and when it changed.
    ScreenSwitch,
    /// The theme or the audio being turned off — and, the bar's actual clause,
    /// turned back on.
    SettingChange,
    /// The replay cursor being moved over a finished run.
    ReplayCursor,
}

impl Missing {
    /// The event variant that would have to exist for the query to have a
    /// number.
    #[must_use]
    pub const fn needs(self) -> &'static str {
        match self {
            Missing::ScreenSwitch => "Event::ScreenSwitched",
            Missing::SettingChange => "Event::SettingChanged",
            Missing::ReplayCursor => "Event::ReplayCursorMoved",
        }
    }

    /// Why it is not a small addition — the sentence an operator asking *can we
    /// just count it* should get back.
    #[must_use]
    pub const fn why(self) -> &'static str {
        match self {
            Missing::ScreenSwitch | Missing::SettingChange | Missing::ReplayCursor => {
                "the console is a reader: abcc-tui does not depend on abcc-store, \
                 so recording this makes the reader a writer (ADR-0012)"
            }
        }
    }
}

/// A query's answer: a number, or the named reason there is none.
///
/// 🚨 The whole point of the type. `Measured(0)` and `NoInstrument(..)` are
/// different facts, and this project has repeatedly paid for conflating them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer<T> {
    Measured(T),
    NoInstrument(Missing),
}

/// Whether a bar held.
///
/// Three variants and no `bool`, because *nothing to judge* is a third thing —
/// `abcc-core`'s rule 3, and the same reason [`AttemptOutcome`] keeps failures
/// and absences apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every sample was inside the bar, and there was at least one sample.
    Held,
    /// At least one sample was outside it.
    Broken,
    /// No sample, or no instrument.
    Unknown,
}

/// One silence, and the two events that bound it — so a verdict can be looked
/// at rather than believed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub ms: i64,
    /// The event the silence started after.
    pub after: Seq,
    /// The event that ended it.
    pub before: Seq,
}

/// A sample's shape. §5 asks for p50, p95 and max by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spread {
    pub n: usize,
    pub p50_ms: i64,
    pub p95_ms: i64,
    pub max_ms: i64,
}

impl Spread {
    /// The spread of a sample, or `None` if there is no sample.
    ///
    /// Percentiles are **nearest-rank**: the smallest observation at or below
    /// which at least `p` percent of the sample sits. Chosen over interpolation
    /// because every number reported is then a gap that actually happened.
    #[must_use]
    pub fn of(mut sample: Vec<i64>) -> Option<Spread> {
        if sample.is_empty() {
            return None;
        }
        sample.sort_unstable();
        Some(Spread {
            n: sample.len(),
            p50_ms: percentile(&sample, 50),
            p95_ms: percentile(&sample, 95),
            max_ms: sample.last().copied().unwrap_or(0),
        })
    }
}

/// Nearest-rank percentile over an ascending sample.
fn percentile(sample: &[i64], p: u64) -> i64 {
    let n = sample.len();
    let width = u64::try_from(n).unwrap_or(u64::MAX);
    let rank = usize::try_from((p * width).div_ceil(100)).unwrap_or(n);
    sample.get(rank.clamp(1, n) - 1).copied().unwrap_or(0)
}

/// §5's *no dead air*: the silences inside runs, and how many broke the bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadAir {
    /// `None` when no run contained two events to be silent between.
    pub spread: Option<Spread>,
    /// How many gaps exceeded [`SILENCE_BAR_MS`].
    pub over_bar: usize,
    /// The worst one, with its two seqs.
    pub worst: Option<Gap>,
}

impl DeadAir {
    /// Held when every closed in-run gap was inside the bar.
    ///
    /// ⚠ This says nothing about an *open* silence — see
    /// [`Fun::open_silence`], which is the one the bar was written for.
    #[must_use]
    pub const fn verdict(&self) -> Verdict {
        match self.spread {
            None => Verdict::Unknown,
            Some(_) if self.over_bar == 0 => Verdict::Held,
            Some(_) => Verdict::Broken,
        }
    }
}

/// §5's *speed where it is felt*: an operator command, and the next thing the
/// log has to show for it.
///
/// ⚠ **Measured to the log's edge and no further.** Every event has a line
/// (`abcc_tui::line::describe` is exhaustive over [`Event`] with no wildcard
/// arm), so the next event after a command *is* the next thing the console
/// draws — but the transport and the paint are a second hop that nothing
/// records. This is the log half of the bar, and it is the half a slow answer
/// shows up in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Speed {
    pub spread: Option<Spread>,
    /// How many answers took longer than [`RESPONSE_BAR_MS`].
    pub over_bar: usize,
    /// 🚨 Commands the log never answered at all — the run ended first. Not a
    /// slow answer, and not folded in with one: the live sortie of 2026-09-04
    /// ended with exactly this, a `ControlRequested` for a task that was then
    /// never admitted.
    pub unanswered: usize,
    pub slowest: Option<Gap>,
}

impl Speed {
    /// Held when every answered command was inside the bar.
    ///
    /// ⚠ [`Speed::unanswered`] does not break it — a command with no answer is
    /// an absence, not a slow response, and mixing them would put a missing
    /// measurement into a number about latency.
    #[must_use]
    pub const fn verdict(&self) -> Verdict {
        match self.spread {
            None => Verdict::Unknown,
            Some(_) if self.over_bar == 0 => Verdict::Held,
            Some(_) => Verdict::Broken,
        }
    }
}

/// §5's *agency*: control events per run. A control bar nobody touches is
/// decoration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Agency {
    pub runs: usize,
    /// Runs carrying at least one [`Event::ControlRequested`].
    pub runs_with_control: usize,
    pub events: usize,
}

impl Agency {
    /// §5's bar is `> 0` in a meaningful share of runs. *Meaningful* is not a
    /// number anyone has set, so this answers the falsifiable half: whether the
    /// bar has been touched at all.
    ///
    /// ⚠ This could not return [`Verdict::Held`] before 2026-09-04, because
    /// until the fleet control desk landed (`abcc` 900efe8, F582–F586) `abcc
    /// fleet` had no way to produce a control event.
    #[must_use]
    pub const fn verdict(&self) -> Verdict {
        match (self.runs, self.runs_with_control) {
            (0, _) => Verdict::Unknown,
            (_, 0) => Verdict::Broken,
            _ => Verdict::Held,
        }
    }
}

/// §5's *honest failure*: replay-cursor movement on failed runs ÷ failed runs.
///
/// 🚨 **The denominator is measured and the numerator has no instrument.** A
/// ratio cannot be reported, and the honest report is the two halves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HonestFailure {
    /// Runs in which an attempt ended in a way that was *decided* against it —
    /// `SoftFailure`, `HardFailure` or `Refused`.
    ///
    /// ⚠ [`AttemptOutcome::Uncertain`] is deliberately not counted. It is an
    /// absence rather than a score, and `abcc-core`'s whole rule 2 is that the
    /// two do not get added together.
    pub failed_runs: usize,
    pub replayed: Answer<usize>,
}

impl HonestFailure {
    /// [`Verdict::Unknown`] while nothing records a replay.
    #[must_use]
    pub const fn verdict(&self) -> Verdict {
        match self.replayed {
            Answer::NoInstrument(_) => Verdict::Unknown,
            Answer::Measured(0) => Verdict::Broken,
            Answer::Measured(_) => Verdict::Held,
        }
    }
}

/// The six, folded in one pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fun {
    /// [`Event::RunStarted`] count. Only `abcc run` and `abcc fleet` write one.
    pub runs: usize,
    /// Events the fold saw in total.
    pub events: usize,
    /// Events belonging to no run — what an operator typed between them.
    pub loose: usize,
    pub dead_air: DeadAir,
    pub speed: Speed,
    /// 🚨 Screen switches before the first operator command. No instrument.
    pub legibility: Answer<usize>,
    pub agency: Agency,
    /// 🚨 Theme and audio setting changes. No instrument.
    pub personality: Answer<usize>,
    pub honest_failure: HonestFailure,
}

impl Fun {
    /// Fold the six over a log, in `seq` order — the order [`crate::seq`]
    /// defines and the only order the store reads in.
    #[must_use]
    pub fn over(log: &[Logged]) -> Fun {
        let spans = spans(log);
        let mut gaps: Vec<i64> = Vec::new();
        let mut worst: Option<Gap> = None;
        let mut over_silence = 0usize;

        let mut answers: Vec<i64> = Vec::new();
        let mut slowest: Option<Gap> = None;
        let mut over_response = 0usize;
        let mut unanswered = 0usize;

        let mut runs_with_control = 0usize;
        let mut control_events = 0usize;
        let mut failed_runs = 0usize;

        for span in &spans {
            let mut this_run_control = 0usize;
            let mut failed = false;
            for (i, logged) in span.iter().enumerate() {
                let next = span.get(i + 1).map(|n| Gap {
                    ms: n.at_ms.saturating_sub(logged.at_ms),
                    after: logged.seq,
                    before: n.seq,
                });
                if let Some(gap) = next {
                    gaps.push(gap.ms);
                    if gap.ms > SILENCE_BAR_MS {
                        over_silence += 1;
                    }
                    if worst.is_none_or(|w| gap.ms > w.ms) {
                        worst = Some(gap);
                    }
                }
                if matches!(logged.event, Event::ControlRequested { .. }) {
                    this_run_control += 1;
                    match next {
                        None => unanswered += 1,
                        Some(answer) => {
                            answers.push(answer.ms);
                            if answer.ms > RESPONSE_BAR_MS {
                                over_response += 1;
                            }
                            if slowest.is_none_or(|s| answer.ms > s.ms) {
                                slowest = Some(answer);
                            }
                        }
                    }
                }
                if let Event::AttemptEnded { outcome, .. } = &logged.event
                    && decided_against(outcome)
                {
                    failed = true;
                }
            }
            control_events += this_run_control;
            if this_run_control > 0 {
                runs_with_control += 1;
            }
            if failed {
                failed_runs += 1;
            }
        }

        let in_span: usize = spans.iter().map(|s| s.len()).sum();
        Fun {
            runs: spans.len(),
            events: log.len(),
            loose: log.len().saturating_sub(in_span),
            dead_air: DeadAir {
                spread: Spread::of(gaps),
                over_bar: over_silence,
                worst,
            },
            speed: Speed {
                spread: Spread::of(answers),
                over_bar: over_response,
                unanswered,
                slowest,
            },
            legibility: Answer::NoInstrument(Missing::ScreenSwitch),
            agency: Agency {
                runs: spans.len(),
                runs_with_control,
                events: control_events,
            },
            personality: Answer::NoInstrument(Missing::SettingChange),
            honest_failure: HonestFailure {
                failed_runs,
                replayed: Answer::NoInstrument(Missing::ReplayCursor),
            },
        }
    }

    /// 🚨 **The half of the dead-air bar that cannot be a fold**: how long the
    /// log has been silent *right now*.
    ///
    /// [`Fun::over`] can only see silences that ended, because a gap needs an
    /// event on both sides — and the hang the bar exists for never writes the
    /// second one. This is the query that catches it, and it needs `now`, which
    /// is not on the log.
    ///
    /// `None` when the log is empty. It is about the log as a whole rather than
    /// about a run, because whether a run is still alive is itself not recorded.
    #[must_use]
    pub fn open_silence(log: &[Logged], now_ms: i64) -> Option<i64> {
        log.last().map(|l| now_ms.saturating_sub(l.at_ms))
    }
}

/// Whether an attempt's ending was **decided against it**, as opposed to not
/// decided at all.
fn decided_against(outcome: &AttemptOutcome) -> bool {
    match outcome {
        AttemptOutcome::SoftFailure { .. }
        | AttemptOutcome::HardFailure { .. }
        | AttemptOutcome::Refused { .. } => true,
        // 🚨 `Uncertain` is an absence and not a failure — the commonest member
        // is the model stopping at the token cap with an empty payload.
        AttemptOutcome::Success | AttemptOutcome::Uncertain { .. } => false,
    }
}

/// The runs, each as the slice of events it produced.
///
/// A span opens at [`Event::RunStarted`] and closes at the **last in-flight
/// event before the next one** — never at the next `RunStarted`, because there
/// is no `RunEnded` and the events between two runs are an operator's, not the
/// earlier run's. See this module's header.
fn spans(log: &[Logged]) -> Vec<&[Logged]> {
    let starts: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, l)| matches!(l.event, Event::RunStarted { .. }))
        .map(|(i, _)| i)
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(k, &start)| {
            let bound = starts.get(k + 1).copied().unwrap_or(log.len());
            let last = log[start..bound]
                .iter()
                .rposition(|l| in_flight(&l.event))
                .map_or(start, |off| start + off);
            &log[start..=last]
        })
        .collect()
}

/// Whether only a **running process** writes this event.
///
/// 🚨 Exhaustive with no wildcard arm, on purpose and for the same reason
/// `abcc_tui::line::describe` is: a new variant should not build until somebody
/// has decided whether a run in flight is the only thing that writes it. Getting
/// it wrong the permissive way attributes an operator's later command to the
/// previous run and reports the wait as dead air.
///
/// Where a variant is written from both sides — [`Event::Note`] is written by
/// `abcc::desk` inside a run *and* by `abcc check` outside one — the answer is
/// `false`. A span that is short by one event under-reports; a span that runs on
/// invents a silence.
#[must_use]
pub const fn in_flight(event: &Event) -> bool {
    match event {
        // Written only by `abcc run` and `abcc fleet`, and by the driver and the
        // engine underneath them. `abcc::desk` is the console edge *inside* a
        // run or a sortie, so nothing outside one writes a control verb either.
        Event::RunStarted { .. }
        | Event::ModeDowngraded { .. }
        | Event::AttemptStarted { .. }
        | Event::AttemptEnded { .. }
        | Event::AttemptPhaseEntered { .. }
        | Event::ModelCallStarted { .. }
        | Event::ModelCallEnded { .. }
        | Event::PhaseEnded { .. }
        | Event::ToolCallStarted { .. }
        | Event::ToolCallEnded { .. }
        | Event::RungRecorded { .. }
        | Event::ClaimRecorded { .. }
        | Event::CheckpointTaken { .. }
        | Event::WorktreeOpened { .. }
        | Event::WorktreeClosed { .. }
        | Event::OperatorPrompted { .. }
        | Event::LivenessMark { .. }
        | Event::PhaseNudged { .. }
        | Event::ControlApplied { .. }
        | Event::ControlRequested { .. } => true,
        // An operator's, or written from both sides. `TaskTransitioned` is the
        // one that matters: `abcc accept` writes one, hours after the run.
        Event::MissionCreated { .. }
        | Event::TaskCreated { .. }
        | Event::TaskDependsOn { .. }
        | Event::MissionPhaseEntered { .. }
        | Event::TaskTransitioned { .. }
        | Event::CommandRefused { .. }
        | Event::OperatorAnswered { .. }
        | Event::ReviewRecorded { .. }
        | Event::Note { .. } => false,
    }
}
