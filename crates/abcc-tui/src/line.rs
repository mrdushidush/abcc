//! One event, one line — and the match that makes the event shape a contract.
//!
//! 🚨 **This is why the reader ships in Skeleton and not at Console.** `PLAN.md`
//! §4 wants *"every milestone emits events in the shape Console reads"* to be
//! something that breaks when it is violated rather than a written rule nothing
//! checks. [`describe`] matches [`Event`] **exhaustively, with no wildcard arm**,
//! so a new variant does not compile until somebody has decided what an operator
//! sees when it happens. That decision is cheap on the day the variant is added
//! and expensive three milestones later.
//!
//! Two rules hold for every arm:
//!
//! * **One event is one line.** Free text an operator did not write — a prompt, a
//!   note, a checker's detail — is clipped by [`clip`], because a feed where one
//!   event can occupy the screen is a feed that hides the next one.
//! * **No `Debug` reaches the screen.** Every enum that appears in a line has a
//!   sentence written for it here, or a `Display` written for it in `abcc-core`.

use std::fmt::Write as _;

use abcc_core::attempt::{AttemptOutcome, Cause};
use abcc_core::event::{Control, Event, Finish, Logged, TraceSignal, WeightsOutcome};
use abcc_core::outcome::{Outcome, Why};
use abcc_core::run::DowngradeReason;
use abcc_core::seq::{Seq, TaskId};
use abcc_core::task::{AbortReason, Command, RequeueReason};

use crate::theme::Theme;

/// How much free text one line will carry before it is cut.
pub const CLIP: usize = 56;

/// An event as the feed shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub seq: Seq,
    pub at_ms: i64,
    /// The discriminant, as the log stores it. Carried so that a reader and a
    /// `SELECT` over the `kind` column are talking about the same thing.
    pub kind: &'static str,
    pub task: Option<TaskId>,
    pub text: String,
}

/// Render one logged event as one line.
///
/// ⚠ The `allow` is deliberate and narrow: the whole value of this function is
/// that it is **one** exhaustive match over [`Event`]. Splitting it to satisfy a
/// line count would give a new variant somewhere to hide, which is exactly the
/// failure the function exists to prevent.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn describe(logged: &Logged, theme: Theme) -> Line {
    let text = match &logged.event {
        Event::RunStarted { mode, version, pid } => {
            format!(
                "run started · {} · v{version} · pid {pid}",
                theme.mode(*mode)
            )
        }
        Event::ModeDowngraded { from, to, why } => format!(
            "mode downgraded · {} -> {} · {}",
            theme.mode(*from),
            theme.mode(*to),
            downgrade_text(why)
        ),
        Event::MissionCreated { title } => format!("mission created · {}", clip(title)),
        Event::TaskCreated { mission, title, .. } => {
            format!("task created in {mission} · {}", clip(title))
        }
        Event::TaskDependsOn { on, .. } => format!("waits on {on}"),
        Event::MissionPhaseEntered { mission, phase } => {
            format!("{mission} · {}", theme.mission_phase(*phase))
        }
        Event::TaskTransitioned {
            command, from, to, ..
        } => format!(
            "{} · {} -> {}",
            command_text(command),
            theme.state(from),
            theme.state(to)
        ),
        Event::CommandRefused {
            command,
            state,
            refusal,
            ..
        } => format!(
            "refused {} in {} · {}",
            command_text(command),
            theme.state(state),
            clip(refusal)
        ),
        Event::AttemptStarted { unit, cause, .. } => format!(
            "attempt {} started on {unit} · {}",
            logged.seq,
            cause_text(cause)
        ),
        Event::AttemptEnded {
            attempt, outcome, ..
        } => format!("{attempt} ended · {}", attempt_outcome_text(outcome)),
        Event::AttemptPhaseEntered { attempt, phase } => {
            format!("{attempt} · {}", theme.attempt_phase(*phase))
        }
        // ⚠ The size and not the body. F501's rule is that one event is one
        // line, and this one is a document — the phase above it names what was
        // being asked, and the text itself is on the log for a reader who wants
        // it.
        Event::BriefRecorded { attempt, text } => {
            format!("{attempt} · brief · {} chars", text.len())
        }
        // 🚨 **F718: the seed is here because without it two calls under one head
        // render identically.** F715 put the sampler's seed on the record and no
        // reader showed it — the same shape of defect F708 was, a field written
        // and nobody reading it. It is the only value on this line that changes
        // from one call to the next within a phase, so a feed that omits it
        // cannot tell a retry from its parent or a seeded run from the eleven
        // thousand unseeded events underneath it.
        Event::ModelCallStarted {
            model,
            head,
            budget,
            seed,
            ..
        } => format!("calling {model} · head {head} · {budget} tokens back · seed {seed}"),
        Event::ModelCallEnded {
            usage,
            finish,
            ttfb_ms,
            elapsed_ms,
            ..
        } => {
            let mut s = format!(
                "answered in {elapsed_ms} ms (ttfb {ttfb_ms}) · {} in, {} out",
                usage.prompt_tokens, usage.completion_tokens
            );
            if let Some(r) = usage.reasoning_tokens {
                let _ = write!(s, ", {r} reasoning");
            }
            // ⚠ Absent, never zero. F494: this server does not report the cache
            // counter at all, so the frozen head's payoff is visible as TTFB and
            // nowhere else — and a reader that printed 0 would say the head is
            // failing when the truth is that nobody counted.
            match usage.cached_tokens {
                Some(c) => {
                    let _ = write!(s, " · {c} cached");
                }
                None => s.push_str(" · cache not reported"),
            }
            let _ = write!(s, " · {}", finish_text(finish));
            s
        }
        // 🚨 **F748. The one line on this screen that says the model was not
        // shown what it was sent** — and it has to be its own line, because the
        // call above it looks completely ordinary: `200 OK`, a finish reason of
        // `tool_calls`, and a token count that is simply smaller than one an
        // operator saw several screens ago.
        Event::PromptCut {
            reported,
            high_water,
            ..
        } => format!(
            "🚨 prompt CUT by the server · {reported} in, {high_water} earlier this phase · at least {} tokens gone, at 200 OK",
            high_water.saturating_sub(*reported)
        ),
        Event::ToolCallStarted { tool, tier, .. } => format!("tool {tool} · admitted at {tier}"),
        Event::ToolCallEnded {
            tool,
            exit,
            elapsed_ms,
            unmeasured,
            output,
            ..
        } => {
            // 🚨 The unmeasured class wins over the exit code, always. A stopped
            // child hands back 1 on Windows, and printing that as a failure is
            // the record saying *the tests failed* about work nobody ran.
            let ending = match (unmeasured, exit) {
                (Some(why), _) => format!("no measurable ending: {}", why_text(why)),
                (None, Some(code)) => format!("exit {code}"),
                (None, None) => "no exit status".to_string(),
            };
            // ⚠ The size and not the body, for `BriefRecorded`'s reason above:
            // this output is bounded at 64 KiB by `MAX_READ_BYTES` and at 4 MiB
            // by `MAX_CAPTURE_BYTES`, neither of which is a line.
            //
            // 🚨 **Absent, never zero — F494's rule, and here it is load-bearing
            // rather than pedantic.** `output` is `serde(default)`, so every one
            // of the 2,147 tool calls logged before F713 reads back as `None`;
            // rendering those as `0 chars` would put *the tool returned nothing*
            // on two thousand rows where the truth is that nobody wrote it down.
            //
            // 🚨 **And it is gated on the ending, because the budget is real.**
            // An unmeasured call spends the whole line saying *why nothing was
            // measured* — that arm was already at 103 characters of F501's
            // 112-character bar, and appending to it costs the tail of the error
            // sentence, which is the one thing an operator opens that row for.
            // So the size goes on the 92.8% of calls that end normally (156 of
            // 2,156 are unmeasured), and the other 7.2% keep their sentence. The
            // number is not lost: it is on the log, and `abcc replay` tallies it
            // per tool, where there is room for both.
            let back = match (unmeasured, output) {
                (Some(_), _) => String::new(),
                (None, Some(o)) => format!(" · {} bytes back", o.len()),
                (None, None) => " · output not recorded".to_owned(),
            };
            format!("tool {tool} · {ending} · {elapsed_ms} ms{back}")
        }
        Event::RungRecorded { attempt, outcome } => {
            format!("{attempt} · rung {}", outcome_text(outcome))
        }
        Event::ClaimRecorded { attempt, claim } => format!(
            "{attempt} · {} claims: {}",
            claim.by,
            // A claim is what the model said about its own work. It is shown, and
            // there is deliberately no path from here to an `Outcome`.
            clip(&claim.text)
        ),
        Event::CheckpointTaken { sha, git_ref, .. } => {
            format!("checkpoint {} held by {git_ref}", short(sha))
        }
        Event::WorktreeOpened { path, sha, .. } => {
            format!("worktree at {} · {}", short(sha), clip(path))
        }
        Event::WorktreeClosed { path, .. } => format!("worktree removed · {}", clip(path)),
        Event::OperatorPrompted { question, .. } => {
            format!("asks the operator ({}) · {}", logged.seq, clip(question))
        }
        Event::OperatorAnswered { prompt, answer, .. } => {
            format!("operator answered {prompt} · {}", clip(answer))
        }
        Event::OperatorSaid { attempt, text } => {
            format!("{attempt} · operator said · {}", clip(text.as_str()))
        }
        Event::ControlRequested { control, .. } => format!("operator: {}", control_text(control)),
        Event::ControlApplied { control, .. } => format!("applied: {}", control_text(control)),
        Event::LivenessMark { note, .. } => format!("alive · {}", clip(note)),
        // F503. Named as a repair rather than a fault: the phase has not ended,
        // and an operator reading the feed needs to know why the same head is
        // being asked twice in a row with no tool call between.
        Event::PhaseNudged { attempt, by, left } => {
            format!("{attempt} · {by} said nothing; asked again ({left} left)")
        }
        // 🚨 F513. The trace signal is named only when it is the one worth
        // naming: ADR-0010 §7 calls it a stop rather than a score, and a line
        // that prints "trace closed" on every phase teaches an operator to stop
        // reading the word. Denials likewise — zero is the expected number.
        Event::PhaseEnded {
            attempt,
            by,
            turns,
            tool_calls,
            denials,
            prompt_tokens,
            completion_tokens,
            elapsed_ms,
            trace,
            ..
        } => {
            let mut s = format!(
                "{attempt} · {by} done · {turns} turn(s), {tool_calls} tool call(s) · \
                 {prompt_tokens} in, {completion_tokens} out · {elapsed_ms} ms"
            );
            if *denials > 0 {
                let _ = write!(s, " · {denials} denial(s)");
            }
            if matches!(trace, TraceSignal::OpenAt200) {
                s.push_str(" · trace open at 200");
            }
            s
        }
        Event::ReviewRecorded {
            change,
            seconds,
            by,
            crossed_boundary,
        } => format!(
            "review · {} · {} · {by}{}",
            clip(change),
            minutes(u64::from(*seconds)),
            if *crossed_boundary {
                " · crosses a module boundary"
            } else {
                ""
            }
        ),
        // 🚨 **The line M1 is read off.** It leads with the change rather than
        // the task because that is the string `abcc review` takes next, and it
        // names the rung count because the entitlement to be on this line at all
        // is `Headline::Green { rungs }` and nothing else.
        Event::ChangeLanded {
            task,
            change,
            rungs,
            ..
        } => format!("landed · {} · {task} · {rungs} rung(s) green", clip(change)),
        // 🚨 The one line in the feed that can say the model is not the model.
        // `Changed` leads with the word rather than burying it after the id,
        // because it is the only arm an operator has to act on — and the two
        // passing arms are worded apart on purpose: `verified` read the bytes,
        // `unchanged` read the directory entry.
        Event::WeightsChecked {
            model,
            digest,
            outcome,
        } => {
            let short = digest.as_deref().map_or("--", |d| &d[..d.len().min(12)]);
            match outcome {
                WeightsOutcome::Pinned => {
                    format!("weights pinned · {} · {short}", clip(model))
                }
                WeightsOutcome::Verified => {
                    format!("weights verified · {} · {short}", clip(model))
                }
                WeightsOutcome::Unchanged => {
                    format!(
                        "weights unchanged · {} · {short} (not re-read)",
                        clip(model)
                    )
                }
                WeightsOutcome::Changed { was } => format!(
                    "WEIGHTS CHANGED · {} · was {} · now {short}",
                    clip(model),
                    &was[..was.len().min(12)]
                ),
                WeightsOutcome::Unlocated { why } => {
                    format!("weights unchecked · {} · {}", clip(model), clip(why))
                }
            }
        }
        Event::Note { text } => clip(text),
    };
    Line {
        seq: logged.seq,
        at_ms: logged.at_ms,
        kind: logged.event.kind(),
        task: logged.event.task(),
        text,
    }
}

/// Cut free text to one line's worth, marking that it was cut, and flattening
/// anything that would move the cursor.
#[must_use]
pub fn clip(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.trim();
    if flat.chars().count() <= CLIP {
        return flat.to_string();
    }
    let head: String = flat.chars().take(CLIP - 1).collect();
    format!("{}…", head.trim_end())
}

/// Seconds as the ladder reports them.
///
/// W13's ladder is measured in minutes and the log stores **seconds**, because a
/// float in a record whose whole purpose is to be summed over months stops
/// summing exactly. The conversion belongs here, at the screen, and nowhere else.
///
/// ⚠ `u64` rather than the event's `u32`: one review fits in a `u32` and a
/// ladder's **sum** is the quantity being summed over months, so the widening
/// happens where the two meet rather than at whichever call site overflows
/// first.
#[must_use]
pub fn minutes(seconds: u64) -> String {
    format!("{}.{} min", seconds / 60, (seconds % 60) * 10 / 60)
}

fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

fn command_text(command: &Command) -> String {
    match command {
        Command::Deploy { unit } => format!("deploy to {unit}"),
        Command::Engage { attempt } => format!("engage as {attempt}"),
        Command::RequestOrders { .. } => "request orders".to_string(),
        Command::Hold { checkpoint } => format!("hold at {checkpoint}"),
        Command::Resume { unit } => format!("resume on {unit}"),
        Command::Commandeer => "commandeer".to_string(),
        Command::Release => "release".to_string(),
        Command::Requeue { why } => format!("requeue · {}", requeue_text(why)),
        Command::Accomplish { .. } => "accomplish".to_string(),
        Command::Fail { .. } => "fail".to_string(),
        Command::Abort { reason } => format!("abort · {}", abort_text(reason)),
    }
}

fn requeue_text(why: &RequeueReason) -> String {
    match why {
        RequeueReason::SpinUpTimeout { after_ms } => format!("nothing started in {after_ms} ms"),
        RequeueReason::ProgressStalled { after_ms } => format!("no progress for {after_ms} ms"),
        RequeueReason::OrphanedByRestart => "orphaned by a restart".to_string(),
        // The one requeue that is not a watchdog's. It names the attempt because
        // the reason it happened is on that attempt's `AttemptEnded`, and a
        // reader who wants it should be sent there rather than told it twice.
        RequeueReason::AttemptRetryable { of } => format!("{of} is worth another go"),
    }
}

fn abort_text(reason: &AbortReason) -> String {
    match reason {
        AbortReason::Operator { by } => format!("by {by}"),
        AbortReason::CompletedByOperator { by } => format!("{by} finished it by hand"),
        AbortReason::BudgetExhausted { which } => format!("{which} budget exhausted"),
        AbortReason::Superseded { by } => format!("superseded by {by}"),
        AbortReason::Unrecoverable { detail } => clip(detail),
    }
}

fn cause_text(cause: &Cause) -> String {
    match cause {
        Cause::Fresh => "fresh".to_string(),
        Cause::Retry { of } => format!("retry of {of}"),
        Cause::Rescope { of } => format!("rescope of {of}"),
        Cause::Edit { of } => format!("edited {of}"),
        Cause::Replay { of } => format!("replay of {of}"),
    }
}

fn control_text(control: &Control) -> String {
    match control {
        Control::Pause => "pause at the next step".to_string(),
        Control::Halt => "halt now, keep the work".to_string(),
        Control::Kill => "kill, keep nothing".to_string(),
        Control::Redirect { prompt } => format!("redirect · {}", clip(prompt)),
        Control::Resume => "resume".to_string(),
    }
}

fn downgrade_text(why: &DowngradeReason) -> String {
    match why {
        DowngradeReason::BudgetCeiling { which } => format!("{which} ceiling"),
        DowngradeReason::EgressDenied { rule } => format!("egress rule {rule}"),
        DowngradeReason::NetworkFailure { detail } => format!("network · {}", clip(detail)),
        DowngradeReason::Operator { by } => format!("by {by}"),
    }
}

fn finish_text(finish: &Finish) -> String {
    match finish {
        Finish::Stop => "stopped".to_string(),
        // 🚨 The empty payload at the cap is the one that is an absence rather
        // than an answer, and the line says so in words.
        Finish::Length {
            content_empty: true,
        } => "hit the cap with nothing to show".to_string(),
        Finish::Length {
            content_empty: false,
        } => "hit the cap".to_string(),
        Finish::ToolCalls => "asked for tools".to_string(),
        Finish::Truncated { detail } => format!("stream ended early · {}", clip(detail)),
    }
}

fn outcome_text(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Measured(m) => {
            let counts = m.counts.map_or_else(
                || "counts not recoverable".to_string(),
                |c| format!("{} run, {} passed, {} failed", c.run, c.passed, c.failed),
            );
            format!(
                "{} at {} · exit {} · {counts}",
                m.rung,
                short(&m.sha),
                m.exit
            )
        }
        Outcome::Unmeasured { rung, why } => format!("{rung} · unmeasured: {}", why_text(why)),
    }
}

fn attempt_outcome_text(outcome: &AttemptOutcome) -> String {
    match outcome {
        AttemptOutcome::Success => "success".to_string(),
        AttemptOutcome::SoftFailure { why } => format!("soft failure · {}", why_text(why)),
        AttemptOutcome::HardFailure { why } => format!("hard failure · {}", why_text(why)),
        AttemptOutcome::Uncertain { why } => format!("uncertain · {}", why_text(why)),
        // ⚠ Clipped for F501's reason and more so: a rung's evidence is a
        // compiler diagnostic, which is many lines with its own indentation.
        // The whole of it is on the log and in the operator's question.
        AttemptOutcome::Refused { rung, detail } => {
            format!("refused by {rung} · {}", clip(detail))
        }
    }
}

/// 🚨 **F501: a `Why` is free text wearing a sentence.**
///
/// Every variant that carries a `detail`, an `os_error` or a path is carrying
/// something this program did not write — and [`Why::EngineError`] carried a
/// provider's whole HTML error page, newlines included, straight onto the feed:
/// ten unaligned rows out of the `seq · elapsed · text` columns and off the
/// right edge of the frame. The module's first rule is *one event is one line*,
/// and the three places a `Why` reaches the screen are the three that were not
/// keeping it.
///
/// ⚠ The full text is not lost — it is on the log, and the operator's prompt
/// carries the whole sentence when there is one to carry.
fn why_text(why: &Why) -> String {
    clip(&why.to_string())
}
