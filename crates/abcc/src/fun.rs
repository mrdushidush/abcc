//! `abcc fun` — ADR-0012 §5's six queries, run over this repository's log.
//!
//! The fold is [`abcc_core::fun`] and none of it is here; this is the two things
//! that are only true of a process — reading the whole log, and the wall clock
//! the open-silence half of the dead-air bar needs.
//!
//! 🚨 **It opens the log without booting it.** `Store::boot` sweeps orphans and
//! requeues their tasks, which is right for a process that has just started and
//! catastrophic for one standing beside a live attempt — and this command exists
//! to be run *while* a sortie is flying, to answer *how long has it been quiet*.
//! A reporting command that changed what it reports on would be the same defect
//! `abcc board` avoids by the same means.

use std::io::Write;

use abcc_core::event::Logged;
use abcc_core::fun::{Answer, DeadAir, Fun, Missing, SILENCE_BAR_MS, Speed, Spread, Verdict};
use abcc_core::seq::Seq;
use abcc_store::Store;

use crate::ops::{self, open_log};
use crate::{AppError, Invocation};

/// One page of the log. Matches the page `abcc-tui`'s reader takes (F112).
const PAGE: usize = 512;

/// Fold the six queries over the whole log and write them to `out`.
///
/// # Errors
///
/// [`AppError`] if there is no repository here, the log will not open, or `out`
/// will not take the text.
pub fn report(invocation: &Invocation, out: &mut impl Write) -> Result<(), AppError> {
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

    let fun = Fun::over(&log);
    let now = abcc_tui::reader::now_ms();
    writeln!(
        out,
        "{} event(s) over {} run(s); {} belong to no run.\n",
        fun.events, fun.runs, fun.loose
    )?;

    dead_air(out, &fun.dead_air, Fun::open_silence(&log, now))?;
    speed(out, &fun.speed)?;
    legibility(out, fun.legibility)?;
    agency(out, &fun)?;
    personality(out, fun.personality)?;
    honest_failure(out, &fun)?;

    writeln!(
        out,
        "\nthree of the six have no instrument, and say so rather than \
         reporting a zero.\nADR-0012 §5 has the bars."
    )?;
    Ok(())
}

/// 1. No dead air — and the half of it a fold cannot see.
fn dead_air(out: &mut impl Write, air: &DeadAir, open: Option<i64>) -> Result<(), AppError> {
    writeln!(out, "1. no dead air              {}", mark(air.verdict()))?;
    match &air.spread {
        None => writeln!(out, "     no run held two events to be silent between")?,
        Some(s) => {
            writeln!(out, "     {}", spread(s))?;
            writeln!(
                out,
                "     {} of {} gap(s) over the 10 s bar",
                air.over_bar, s.n
            )?;
            if let Some(w) = air.worst {
                writeln!(
                    out,
                    "     worst {} between seq {} and seq {}",
                    ms(w.ms),
                    w.after,
                    w.before
                )?;
            }
        }
    }
    // 🚨 Printed even when every closed gap is inside the bar, because the
    // forty-minute hang is exactly the run whose closed gaps are.
    if let Some(open) = open {
        let verdict = if open > SILENCE_BAR_MS {
            "over the bar — either a run is hung, or nothing is running"
        } else {
            "inside the bar"
        };
        writeln!(out, "     silent for {} right now: {verdict}", ms(open))?;
    }
    Ok(())
}

/// 2. Speed where it is felt.
fn speed(out: &mut impl Write, sp: &Speed) -> Result<(), AppError> {
    writeln!(out, "\n2. speed where it is felt   {}", mark(sp.verdict()))?;
    match &sp.spread {
        None => writeln!(out, "     no operator command has been answered here")?,
        Some(s) => {
            writeln!(out, "     {}", spread(s))?;
            writeln!(
                out,
                "     {} of {} answer(s) over the 1 s bar",
                sp.over_bar, s.n
            )?;
            if let Some(w) = sp.slowest {
                writeln!(out, "     slowest {} after seq {}", ms(w.ms), w.after)?;
            }
        }
    }
    if sp.unanswered > 0 {
        writeln!(
            out,
            "     {} command(s) the log never answered — the run ended first",
            sp.unanswered
        )?;
    }
    Ok(())
}

/// 3. Legibility. No instrument.
fn legibility(out: &mut impl Write, answer: Answer<usize>) -> Result<(), AppError> {
    writeln!(
        out,
        "\n3. legibility               {}",
        mark(Verdict::Unknown)
    )?;
    absent(out, answer, "screen switches before the first command")
}

/// 4. Agency — the one the fleet control desk made answerable.
fn agency(out: &mut impl Write, fun: &Fun) -> Result<(), AppError> {
    writeln!(
        out,
        "\n4. agency                   {}",
        mark(fun.agency.verdict())
    )?;
    writeln!(
        out,
        "     {} control event(s) across {} of {} run(s)",
        fun.agency.events, fun.agency.runs_with_control, fun.agency.runs
    )?;
    if fun.agency.runs > 0 && fun.agency.runs_with_control == 0 {
        writeln!(out, "     the control bar is decoration on this log")?;
    }
    Ok(())
}

/// 5. Personality with an off switch. No instrument.
fn personality(out: &mut impl Write, answer: Answer<usize>) -> Result<(), AppError> {
    writeln!(
        out,
        "\n5. personality              {}",
        mark(Verdict::Unknown)
    )?;
    absent(out, answer, "theme and audio setting changes")
}

/// 6. Honest failure — a measured denominator and a numerator with no
///    instrument, reported as the two halves rather than as a ratio.
fn honest_failure(out: &mut impl Write, fun: &Fun) -> Result<(), AppError> {
    writeln!(
        out,
        "\n6. honest failure           {}",
        mark(fun.honest_failure.verdict())
    )?;
    writeln!(
        out,
        "     {} run(s) with an attempt decided against it",
        fun.honest_failure.failed_runs
    )?;
    absent(out, fun.honest_failure.replayed, "replay-cursor movement")
}

/// The line a query with no instrument gets: what is absent, what would record
/// it, and why that is not a small change.
fn absent(out: &mut impl Write, answer: Answer<usize>, what: &str) -> Result<(), AppError> {
    match answer {
        Answer::Measured(n) => writeln!(out, "     {n} {what}")?,
        Answer::NoInstrument(missing) => {
            writeln!(out, "     no instrument for {what}")?;
            writeln!(out, "     needs {}", missing.needs())?;
            writeln!(out, "     {}", Missing::why(missing))?;
        }
    }
    Ok(())
}

fn spread(s: &Spread) -> String {
    format!(
        "p50 {} · p95 {} · max {} over {} sample(s)",
        ms(s.p50_ms),
        ms(s.p95_ms),
        ms(s.max_ms),
        s.n
    )
}

/// A duration an operator reads rather than converts.
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

/// ⚠ Three words and no `bool`. `unknown` is not a soft pass — it is the answer
/// when there was nothing to judge, and it reads differently on purpose.
const fn mark(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Held => "held",
        Verdict::Broken => "BROKEN",
        Verdict::Unknown => "unknown",
    }
}

/// The whole log, in `seq` order.
///
/// Shared with `abcc paint`, which folds the same page sequence into a
/// `View` — one paged read, not two that could drift apart.
///
/// Paged rather than read at once because that is the only read the store
/// offers, and because the cursor it takes is the same integer the console
/// scrubs with (ADR-0012).
pub(crate) fn read_all(store: &Store) -> Result<Vec<Logged>, AppError> {
    let mut all = Vec::new();
    let mut since = Seq::ORIGIN;
    loop {
        let page = store.read_from(since, PAGE)?;
        let Some(last) = page.last() else {
            return Ok(all);
        };
        since = last.seq;
        all.extend(page);
    }
}
