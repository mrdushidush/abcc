//! `abcc replay` — ADR-0012's after-action view, over this repository's log.
//!
//! The fold is [`abcc_core::replay`] and none of it is here; this file is the
//! two things that are only true of a process — reading the whole log, and
//! putting a [`TaskState`] through the console's label map so the report names
//! states with the words the operator saw on the board.
//!
//! 🚨 **It opens the log without booting it**, for the reason [`crate::fun`]
//! does: `Store::boot` sweeps orphans and requeues their tasks, which is right
//! for a process that has just started and catastrophic for one standing beside
//! a live attempt. An after-action view that changed what it reported on would
//! be the defect `abcc board` avoids by the same means — and this one is meant to
//! be usable *while* the next sortie is flying.
//!
//! # The two shapes it prints
//!
//! * **No task** — [`Replay::endings`]: every final state against the endings of
//!   the attempts underneath it. This is the one that answers *what is that word
//!   standing over*, and on this project's log the answer was five endings under
//!   `MISSION FAILED`, sixteen of twenty of them `Uncertain`.
//! * **One task** — its transitions, then each attempt's phases, spend, tools,
//!   rungs, longest silence and ending.

use std::io::Write;

use abcc_core::attempt::{AttemptOutcome, Cause};
use abcc_core::event::TraceSignal;
use abcc_core::outcome::Outcome;
use abcc_core::replay::{AttemptTrace, Quiet, Replay, TaskTrace};
use abcc_core::seq::TaskId;
use abcc_tui::Theme;

use crate::cli::TaskRef;
use crate::fun::read_all;
use crate::ops::{self, open_log};
use crate::{AppError, Invocation};

/// Fold the log and write the after-action view to `out`.
///
/// # Errors
///
/// [`AppError`] if there is no repository here, the log will not open, `out`
/// will not take the text, or `task` names something the log never created.
pub fn report(
    invocation: &Invocation,
    task: Option<TaskRef>,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let ground = ops::ground(invocation)?;
    let store = open_log(&ground.home)?;
    let log = read_all(&store)?;
    if log.is_empty() {
        writeln!(
            out,
            "the log is empty. `abcc task \"<what to do>\"` then `abcc run`."
        )?;
        return Ok(());
    }
    let replay = Replay::over(&log);
    match task {
        None => board(&replay, out),
        Some(TaskRef(raw)) => {
            let id = TaskId::at(abcc_core::seq::Seq::new(raw));
            let trace = replay.task(id).ok_or_else(|| {
                AppError::Refused(format!(
                    "the log never created {id} — try `abcc replay` for what it did create"
                ))
            })?;
            one(trace, out)
        }
    }
}

// ---------------------------------------------------------------------------
// the whole board
// ---------------------------------------------------------------------------

/// 🚨 What each of the board's words is standing over.
fn board(replay: &Replay, out: &mut impl Write) -> Result<(), AppError> {
    let rows = replay.endings();
    let attempts: usize = rows
        .iter()
        .map(abcc_core::replay::StateEndings::attempts)
        .sum();
    writeln!(
        out,
        "{} event(s), {} task(s), {attempts} ended attempt(s).\n",
        replay.events,
        replay.tasks.len()
    )?;

    for row in &rows {
        writeln!(
            out,
            "{:<22} {} task(s), {} attempt(s)",
            Theme::Command.state(&row.state),
            row.tasks.len(),
            row.attempts()
        )?;
        if row.endings.is_empty() {
            writeln!(out, "     no attempt ever ended under it")?;
        }
        for (label, n) in &row.endings {
            writeln!(out, "     {n:>3}  {label}")?;
        }
        // 🚨 The sentence the table exists to make sayable. `Uncertain` is not a
        // failure — it is the system reporting that it could not tell, and a
        // one-word state prints identically over both.
        let uncertain = row.uncertain();
        if uncertain > 0 {
            writeln!(
                out,
                "     {uncertain} of {} are Uncertain: the system could not tell, \
                 which is not the work being wrong",
                row.attempts()
            )?;
        }
        writeln!(out)?;
    }

    writeln!(
        out,
        "an ending is the attempt's, and a state is the task's — they are \
         different facts.\n`abcc replay t42` for one task, end to end."
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// one task
// ---------------------------------------------------------------------------

fn one(task: &TaskTrace, out: &mut impl Write) -> Result<(), AppError> {
    let state = task
        .state
        .as_ref()
        .map_or("STANDING BY", |s| Theme::Command.state(s));
    writeln!(out, "{}  {state}  {}", task.id, task.title)?;
    writeln!(out, "mission {}", task.mission)?;
    for line in task.prompt.lines().take(4) {
        writeln!(out, "  | {line}")?;
    }
    if task.prompt.lines().count() > 4 {
        writeln!(out, "  | ...")?;
    }

    writeln!(out, "\ntransitions")?;
    if task.transitions.is_empty() {
        writeln!(
            out,
            "  none — it has never left the state it was created in"
        )?;
    }
    for m in &task.transitions {
        writeln!(
            out,
            "  {:>6}  {:<10} {} -> {}",
            m.seq.to_string(),
            m.command.name(),
            m.from.name(),
            m.to.name()
        )?;
    }

    if task.attempts.is_empty() {
        writeln!(out, "\nno attempt has ever started on this task.")?;
    }
    for attempt in &task.attempts {
        writeln!(out)?;
        one_attempt(attempt, out)?;
    }

    if !task.asked.is_empty() {
        writeln!(out, "\nasked of you")?;
    }
    for asked in &task.asked {
        writeln!(out, "  {}  {}", asked.prompt, one_line(&asked.question, 88))?;
        match &asked.answer {
            Some(a) => writeln!(out, "        answered: {}", one_line(a, 88))?,
            // ⚠ Still owed to a person. `AwaitingOrders` is not terminal for
            // exactly this reason.
            None => writeln!(out, "        unanswered")?,
        }
    }
    Ok(())
}

fn one_attempt(attempt: &AttemptTrace, out: &mut impl Write) -> Result<(), AppError> {
    let from = attempt
        .checkpoint_from
        .map_or_else(|| "no checkpoint".to_owned(), |c| format!("from {c}"));
    writeln!(
        out,
        "{}  {}  {}  {}",
        attempt.id,
        cause(&attempt.cause),
        attempt.unit,
        from
    )?;

    phases(attempt, out)?;
    spent(attempt, out)?;
    tools(attempt, out)?;

    if attempt.nudges > 0 {
        writeln!(
            out,
            "  nudged   {} time(s) — a phase asked again for a missing answer (ADR-0016)",
            attempt.nudges
        )?;
    }
    if attempt.claims > 0 {
        writeln!(
            out,
            "  claims   {} — what a model said about the work, never evidence",
            attempt.claims
        )?;
    }

    for rung in &attempt.rungs {
        writeln!(out, "  rung     {}", one_rung(rung))?;
    }

    // 🚨 F506: the log is the only copy of a thrown-away turn's arguments.
    for call in &attempt.discarded {
        writeln!(
            out,
            "  🚨 discarded {} of {} argument char(s), never run: {}",
            call.tool,
            call.argument_chars,
            call.arguments.as_deref().map_or_else(
                || "not kept".to_owned(),
                |a| {
                    if a.is_empty() {
                        // 🚨 F506's own case: a call cut mid-argument arrives with a
                        // zero-character argument string, which is the model
                        // failing its own schema rather than an empty request.
                        "empty — cut before a single argument character arrived".to_owned()
                    } else {
                        one_line(a, 96)
                    }
                },
            )
        )?;
    }

    quiet(attempt, out)?;

    match &attempt.outcome {
        Some(o) => writeln!(
            out,
            "  ended    {} after {}",
            outcome(o),
            attempt.elapsed_ms().map_or_else(|| "—".to_owned(), ms)
        )?,
        // ⚠ Not *it failed*: an attempt with no `AttemptEnded` is either flying
        // now or was abandoned by a crash, and the log cannot tell those apart.
        None => writeln!(
            out,
            "  ended    it has no ending on the log — in flight, or a crash took it"
        )?,
    }
    Ok(())
}

/// Every phase the attempt entered, with what it spent.
fn phases(attempt: &AttemptTrace, out: &mut impl Write) -> Result<(), AppError> {
    for phase in &attempt.phases {
        let by = phase.by.as_deref().unwrap_or("—");
        let reasoning = phase
            .reasoning_tokens
            .map_or_else(|| "uncounted".to_owned(), |r| format!("{r} reasoning"));
        writeln!(
            out,
            "  {:<9} {:<11} {:>2} turn(s) {:>3} tool(s) {:>2} denial(s)  \
             {:>7} in {:>6} out ({reasoning})  {}  {}",
            format!("{:?}", phase.phase).to_lowercase(),
            by,
            phase.turns,
            phase.tool_calls,
            phase.denials,
            phase.prompt_tokens,
            phase.completion_tokens,
            trace(phase.trace),
            phase.elapsed_ms.map_or_else(
                // 🚨 A phase with no `PhaseEnded` is where the attempt stopped.
                || "STILL OPEN".to_owned(),
                |e| ms(i64::try_from(e).unwrap_or(i64::MAX))
            ),
        )?;
    }
    if let Some(open) = attempt.open_phase {
        writeln!(out, "  🚨 stopped inside {open:?} — that phase never ended")?;
    }
    Ok(())
}

/// What the attempt asked the model for, and — F511 — what came back.
fn spent(attempt: &AttemptTrace, out: &mut impl Write) -> Result<(), AppError> {
    let finishes = attempt
        .spend
        .finishes
        .iter()
        .map(|(f, n)| format!("{n} {f}"))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        out,
        "  model    {} call(s), {} in / {} out, cap {}, worst ttfb {}  [{finishes}]",
        attempt.spend.calls,
        attempt.spend.prompt_tokens,
        attempt.spend.completion_tokens,
        attempt.spend.budget,
        ms(i64::try_from(attempt.spend.ttfb_ms_max).unwrap_or(i64::MAX)),
    )?;

    // 🚨 F511: a token total says how big a completion was, never where it went.
    if attempt.made.counted > 0 {
        let share = attempt
            .made
            .reasoning_share()
            .map_or_else(|| "uncounted".to_owned(), |s| format!("{:.0}%", s * 100.0));
        writeln!(
            out,
            "  made of  {} char(s): {} text, {} trace, {} tool arguments — trace is {share}",
            attempt.made.chars(),
            attempt.made.text_chars,
            attempt.made.reasoning_chars,
            attempt.made.argument_chars
        )?;
        accounting(attempt, out)?;
    }
    Ok(())
}

/// 🚨 **What the composition accounts for, against what the server billed.**
///
/// A composition shown on its own reads as a complete description of the
/// completion, and on this log it is not one: all three full-cap truncations
/// account for under 2% of the tokens charged, every one of them a single
/// `apply_patch` whose `argument_chars` is zero. Printing the fraction is what
/// makes that visible — F511 added the field to stop a total hiding where the
/// completion went, and a partial total hides it again.
///
/// ⚠ The ~4 chars per token is this stack's documented ratio and an estimate,
/// which is why the line says `~` and prints the characters too.
fn accounting(attempt: &AttemptTrace, out: &mut impl Write) -> Result<(), AppError> {
    let billed = attempt.spend.completion_tokens;
    if billed == 0 {
        return Ok(());
    }
    let accounted = attempt.made.chars() / 4;
    // Both are token counts of one attempt; f64 holds them exactly far past
    // anything this stack can produce.
    #[allow(clippy::cast_precision_loss)]
    let share = accounted as f64 / billed as f64;
    let counted = if attempt.made.counted == attempt.spend.calls {
        String::new()
    } else {
        // ⚠ A partial denominator. Say so rather than letting the fraction
        // stand as if every call had reported.
        format!(
            ", from {} of {} call(s)",
            attempt.made.counted, attempt.spend.calls
        )
    };
    writeln!(
        out,
        "  billed   {billed} completion token(s); composition accounts for ~{accounted} ({:.0}%){counted}",
        share * 100.0
    )?;
    Ok(())
}

/// Every tool the attempt called, with the tier it was admitted at.
fn tools(attempt: &AttemptTrace, out: &mut impl Write) -> Result<(), AppError> {
    for tool in &attempt.tools {
        let mut notes = Vec::new();
        if tool.failures > 0 {
            notes.push(format!("{} non-zero", tool.failures));
        }
        if tool.unmeasured > 0 {
            notes.push(format!("{} unmeasured", tool.unmeasured));
        }
        let note = if notes.is_empty() {
            String::new()
        } else {
            format!("  [{}]", notes.join(", "))
        };
        writeln!(
            out,
            "  tool     {:<14} {:>3} call(s) at {:<6} {}{note}",
            tool.tool,
            tool.calls,
            tool.tier,
            ms(i64::try_from(tool.elapsed_ms).unwrap_or(i64::MAX))
        )?;
    }
    Ok(())
}
/// 🚨 The largest silence, and what broke it.
fn quiet(attempt: &AttemptTrace, out: &mut impl Write) -> Result<(), AppError> {
    let Some(q) = &attempt.quiet else {
        return Ok(());
    };
    writeln!(
        out,
        "  quiet    worst {} at seq {}, after {} — broken by {}{}",
        ms(q.gap_ms),
        q.at,
        q.after,
        q.broken_by,
        marked(q),
    )?;
    if attempt.over_bar > 0 {
        writeln!(
            out,
            "           {} silence(s) over the 10 s bar, {} liveness mark(s) in the attempt",
            attempt.over_bar, attempt.marks
        )?;
    }
    Ok(())
}

/// 🚨 Whether the run said *still here*, or the next piece of work simply
/// arrived. A `LivenessMark` is written from inside the stream's read loop, so
/// one closing a long gap means the stream was slow and the run knew it — and
/// anything else closing one means nothing was watching for that whole stretch.
fn marked(q: &Quiet) -> &'static str {
    if q.was_marked() {
        " (the run marked it)"
    } else if q.gap_ms > 10_000 {
        " (UNMARKED)"
    } else {
        ""
    }
}

fn one_rung(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Measured(m) => {
            let counts = m.counts.map_or_else(String::new, |c| {
                format!(" — {} run, {} passed, {} failed", c.run, c.passed, c.failed)
            });
            format!(
                "{:<11} exit {}{counts}  {}",
                m.rung,
                m.exit,
                one_line(&m.detail, 72)
            )
        }
        // ⚠ Absent, not failed. That distinction is the whole reason `Outcome`
        // has two variants rather than a bool.
        Outcome::Unmeasured { rung, why } => format!("{rung:<11} unmeasured: {why}"),
    }
}

fn outcome(outcome: &AttemptOutcome) -> String {
    match outcome {
        AttemptOutcome::Success => "Success".to_owned(),
        AttemptOutcome::Refused { rung, detail } => {
            format!("Refused by {rung}: {}", one_line(detail, 72))
        }
        AttemptOutcome::SoftFailure { why } => format!("SoftFailure — {why}"),
        AttemptOutcome::HardFailure { why } => format!("HardFailure — {why}"),
        AttemptOutcome::Uncertain { why } => format!("Uncertain — {why}"),
    }
}

const fn cause(cause: &Cause) -> &'static str {
    match cause {
        Cause::Fresh => "fresh",
        Cause::Retry { .. } => "retry",
        Cause::Rescope { .. } => "rescope",
        Cause::Edit { .. } => "edit",
        Cause::Replay { .. } => "replay",
    }
}

const fn trace(signal: Option<TraceSignal>) -> &'static str {
    match signal {
        None => "trace —",
        Some(TraceSignal::Absent) => "no trace",
        Some(TraceSignal::Closed) => "trace closed",
        // 🚨 Two of these exist in this project's whole history and both survived
        // only because a person read them off stdout.
        Some(TraceSignal::OpenAt200) => "TRACE OPEN AT 200",
    }
}

/// A duration an operator reads rather than converts. Same rule as `abcc fun`'s.
fn ms(v: i64) -> String {
    if v < 1_000 {
        return format!("{v} ms");
    }
    let secs = v / 1_000;
    if secs < 90 {
        format!("{}.{} s", secs, (v % 1_000) / 100)
    } else {
        format!("{} m {} s", secs / 60, secs % 60)
    }
}

/// One line of a possibly enormous string, for a report where one event is one
/// line (F501).
fn one_line(text: &str, cap: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim();
    if line.chars().count() <= cap {
        return line.to_owned();
    }
    let short: String = line.chars().take(cap).collect();
    format!("{short}...")
}
