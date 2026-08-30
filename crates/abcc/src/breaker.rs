//! `abcc breaker` — **the report, and there is no gate underneath it.**
//!
//! ADR-0010 §4 and F539. Two inputs that answer two different questions, printed
//! together because neither is worth much alone:
//!
//! * **the pulse** — one real token out of the server, which is a *measurement*
//!   and can tell a dead server from a hard task;
//! * **the rate** — F374's update rule over this log's own population, which is
//!   a *statistical verdict* and cannot.
//!
//! 🚨 **Nothing here refuses anything**, and that is ADR-0008's ruling applied
//! one level out: only deterministic rungs may refuse, and a learned threshold
//! is not one. The operator reads this and decides. What the command buys is
//! that the decision is made against numbers rather than against a feeling —
//! and against *this* population's numbers, because one failure moves Q56 by
//! 44.6 points and U100 by under 7 (F374).

use std::fmt::Write as _;
use std::io::Write;

use abcc_fleet::breaker::{Population, Pulse, Record, Report};

use crate::{AppError, Invocation, confirm, ops, pulse};

/// Take a pulse and fold the log, then print both.
///
/// # Errors
///
/// [`AppError`] if there is no repository here, the log will not open, or the
/// report will not write.
pub fn report(
    invocation: &Invocation,
    model: Option<&str>,
    url: Option<&str>,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let ground = ops::ground(invocation)?;
    let store = ops::open_log(&ground.home)?;

    // ⚠ The pulse is taken **before** anything is read off the rate, because an
    // unhealthy server is what corrupts the rate. Printed the other way round,
    // the table gets believed and the pulse reads as a footnote.
    let pulse = match ops::model_name(model) {
        Ok(model) => {
            let base = ops::base_url(url);
            pulse::take(&base, &model, ops::api_key().as_deref())
        }
        // 🚨 An unnamed model is a refusal everywhere a run is about to spend
        // real time. Here it is `NotTaken` instead: the population half is still
        // worth printing, and *not asked* is a variant precisely so it cannot be
        // rendered as *answered*.
        Err(_) => Pulse::NotTaken,
    };

    write_report(&Report::new(pulse, Population::of(&store)?), out)
}

/// The whole rendering, so a test can drive it without a server or a log.
///
/// # Errors
///
/// [`AppError::Io`] if the writer will not take it.
pub fn write_report(report: &Report, out: &mut impl Write) -> Result<(), AppError> {
    writeln!(out, "\nbreaker  a report, never a gate\n")?;

    writeln!(out, "pulse    {}", report.pulse)?;
    match &report.pulse {
        Pulse::Answered { .. } => {}
        Pulse::NotTaken => writeln!(
            out,
            "         \u{25b6} name a model (--model, or {}) to ask the server for one token",
            confirm::MODEL_ENV
        )?,
        // F539's sentence, at the only place it can be said usefully.
        _ => writeln!(
            out,
            "         \u{26a0} a model listing answering is NOT this check. It answered normally \
             for 60 s\n           while every completion returned nothing, and took both slots \
             down (F539)."
        )?,
    }

    writeln!(out, "\nrate     {}", report.standing())?;
    if report.population.measured_tasks().is_empty() {
        writeln!(
            out,
            "         \u{25b6} the rule is what ships, not the number:\n           \
             P(pass at k+1 | first k failed) = \u{3a3} p(1-p)^k / \u{3a3} (1-p)^k"
        )?;
        out.flush()?;
        return Ok(());
    }

    writeln!(out, "\n  failures so far   the next attempt passes")?;
    for (k, p) in report.rows() {
        let shown = match p {
            Some(p) => format!("{:.1}%", p * 100.0),
            // ⚠ Not 0%. This population has never seen k failures in a row, so
            // it has nothing to say about them.
            None => "\u{2014} (never seen on this log)".to_owned(),
        };
        writeln!(out, "  {k:>15}   {shown}")?;
    }

    if let Some(share) = report.population.bimodal_share() {
        writeln!(
            out,
            "\n  {:.0}% of measured tasks sit at 0 or 1 \u{2014} {}",
            share * 100.0,
            if share >= 0.7 {
                "bimodal, so a third attempt buys little (ADR-0010 \u{a7}3)"
            } else {
                "not bimodal, so a third attempt is worth pricing"
            }
        )?;
    }

    writeln!(out, "\n  per task")?;
    let mut records = report.population.measured_tasks();
    records.sort_by_key(|r| r.task);
    for record in &records {
        writeln!(out, "  {}", row(record))?;
    }
    out.flush()?;
    Ok(())
}

/// One task's line. The absences are printed even though they are not in the
/// rate — especially because they are not in it.
fn row(record: &Record) -> String {
    let rate = match record.rate() {
        Some(p) => format!("{:.0}%", p * 100.0),
        None => "\u{2014}".to_owned(),
    };
    let mut line = format!(
        "{:<8} {rate:>5}  {} passed, {} failed",
        record.task.to_string(),
        record.passed,
        record.failed
    );
    if record.absent > 0 {
        let _ = write!(line, ", {} unmeasured (not in the rate)", record.absent);
    }
    line
}

/// The report as one sentence, for the log.
#[must_use]
pub fn note(report: &Report) -> String {
    format!("breaker: pulse {} | {}", report.pulse, report.standing())
}
