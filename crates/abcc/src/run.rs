//! `abcc run` — one attempt, end to end, on a real repository task.
//!
//! This is the Skeleton milestone's exit criterion with a person in front of it.
//! The order below is not incidental:
//!
//! 1. **Boot first**, and say what it reconciled. Boot is replay and it runs
//!    every time, which is what stops the recovery path from rotting — v1's
//!    second recovery path was the one that had never run when it was needed.
//! 2. **`RunStarted` before anything else this process does.** `PLAN.md` §5 wants
//!    the run's mode as its first event, and the mode is [`Mode::SinglePlayer`]:
//!    zero cloud spend is standing, the provider is a local socket, and
//!    `SinglePlayer` is a policy rather than a degraded path (ADR-0013).
//! 3. 🚨 **The model is confirmed before the task is touched.** An unconfirmed
//!    model is a refusal, not a warning — see [`crate::confirm`].
//! 4. The desk's connection is opened **before** the driver takes the log, since
//!    opening a `Store` rebuilds the projection and that is a write.
//! 5. Then the driver runs, and whatever it lands is printed as it is.

use std::io::Write;
use std::thread;

use abcc_core::attempt::Cause;
use abcc_core::event::{Event, WeightsOutcome};
use abcc_core::outcome::Outcome;
use abcc_core::run::Mode;
use abcc_core::seq::{Seq, TaskId, UnitId};
use abcc_core::task::TaskState;
use abcc_drive::{Driver, Landed};
use abcc_engine::control::ControlPoint;
use abcc_engine::evict;
use abcc_engine::openai::OpenAiCompat;
use abcc_engine::tools::Tier;
use abcc_engine::turn::{Limits, PhaseEnded, PhaseReport};
use abcc_gate::Measured;
use abcc_store::{Reconciled, Store};
use abcc_tui::Theme;

use crate::desk::{Desk, VERBS};
use crate::{AppError, Invocation, cli, confirm, home, ops, pulse, weights};

/// Run one attempt and report where it landed.
///
/// # Errors
///
/// [`AppError`] — no repository, no queued task, an unconfirmed model, or
/// anything the driver could not do.
pub fn attempt(
    invocation: &Invocation,
    run: &cli::Run,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let ground = ops::ground(invocation)?;
    let log_path = ground.home.log();
    let mut store = Store::open(&log_path)?;
    let reconciled = store.boot()?;
    report_boot(&reconciled, out)?;

    store.append(Event::RunStarted {
        mode: Mode::SinglePlayer,
        version: crate::VERSION.to_owned(),
        pid: std::process::id(),
    })?;

    let task = choose(&store, run.task, out)?;

    // 🚨 Before the checkpoint, before the worktree, before the slot. Everything
    // after this point costs real time and produces real records, and all of it
    // would be about the wrong model.
    let Confirmed {
        model,
        evict_window,
    } = confirm_model(&mut store, run, out)?;

    let base = ops::base_url(run.base_url.as_deref());
    let mut provider = OpenAiCompat::new(&base)?;
    if let Some(key) = ops::api_key() {
        provider = provider.with_api_key(key);
    }

    let mut limits = limits_for(run);
    limits.evict_window = evict_window;
    let unit = UnitId(run.unit.unwrap_or(0));
    let (mut control, handle) = ControlPoint::new();

    // Opened here, on purpose: see the module docs, point 4.
    let desk = Desk::open(&log_path, handle, task)?;
    writeln!(
        out,
        "\nrunning {task} in unit {unit} at {ceiling} on {model}\n\
         control: type one of [{VERBS}] and press enter\n\
         watch:   abcc watch, in another terminal\n",
        // Said out loud for the same reason the fleet says it: a capped slot
        // is otherwise invisible until a role is refused something.
        ceiling = run.ceiling.unwrap_or(Tier::Exec)
    )?;
    out.flush()?;
    // Detached, and never joined: it blocks on stdin, and on a terminal stdin
    // does not end. `main` returning is what stops it.
    thread::spawn(move || desk.serve(std::io::stderr()));

    let landed = Driver::new(
        &mut store,
        &ground.repo,
        &provider,
        model,
        ground.home.worktrees(),
    )
    .limits(limits)
    .ceiling(run.ceiling.unwrap_or(Tier::Exec))
    .secrets(ops::secrets())
    .run(task, unit, Cause::Fresh, &mut control)?;

    report(&landed, out)
}

// ---------------------------------------------------------------------------
// the pieces
// ---------------------------------------------------------------------------

pub(crate) fn report_boot(reconciled: &Reconciled, out: &mut impl Write) -> Result<(), AppError> {
    if reconciled.events_replayed == 0 {
        writeln!(out, "boot  a new log")?;
        return Ok(());
    }
    writeln!(
        out,
        "boot  {} events replayed, {} task(s)",
        reconciled.events_replayed, reconciled.tasks
    )?;
    if !reconciled.requeued.is_empty() {
        let ids: Vec<String> = reconciled
            .requeued
            .iter()
            .map(ToString::to_string)
            .collect();
        writeln!(
            out,
            "      requeued {} — a slot-holding state with no worker behind it",
            ids.join(", ")
        )?;
    }
    if !reconciled.prompts_to_represent.is_empty() {
        let ids: Vec<String> = reconciled
            .prompts_to_represent
            .iter()
            .map(ToString::to_string)
            .collect();
        // Re-presented, never auto-answered: a question a machine answered on a
        // person's behalf is a question nobody asked.
        writeln!(out, "      still waiting on you: {}", ids.join(", "))?;
    }
    Ok(())
}

/// The task to run: the one named, or the first queued one.
fn choose(
    store: &Store,
    named: Option<cli::TaskRef>,
    out: &mut impl Write,
) -> Result<TaskId, AppError> {
    if let Some(reference) = named {
        let id = TaskId::at(Seq::new(reference.0));
        let row = ops::row(store, id)?;
        if row.state != TaskState::Queued {
            //   🚨 A refusal names the way out, or it is a dead end with
            // good manners. `AwaitingOrders` is the state an operator meets
            // most -- every attempt that stalls lands there -- and the route
            // back to standing-by is two verbs nobody would guess, because
            // F732 made every exit from it an operator's.
            let route = match row.state {
                TaskState::AwaitingOrders { .. } => {
                    " Read it with `abcc replay {id}`; to try again, \
                     `abcc take {id}` then `abcc release {id}`."
                }
                TaskState::Holding { .. } => {
                    " `abcc take {id}` then `abcc release {id}` puts it back on the board."
                }
                TaskState::Commandeered { .. } => " You hold it: `abcc release {id}`.",
                _ => " `abcc board` says what each task is waiting for.",
            };
            return Err(AppError::Refused(format!(
                "{id} is {}, and a run starts from standing-by.{}",
                Theme::Command.state(&row.state),
                route.replace("{id}", &id.to_string())
            )));
        }
        return Ok(id);
    }
    let queued = store
        .tasks()?
        .into_iter()
        .find(|row| row.state == TaskState::Queued)
        .ok_or_else(|| {
            AppError::Refused(
                "nothing is queued. `abcc task \"<what to do>\"` puts one on the board.".to_owned(),
            )
        })?;
    writeln!(out, "task  {} {}", queued.id, queued.title)?;
    Ok(queued.id)
}

/// Ask the server what it is holding, decide, and record the answer either way.
///
/// The `Note` is the only place in the run that says which brain answered:
/// `ModelCallStarted` records the id that was *requested*, which is exactly the
/// number that is wrong when this check would have failed.
pub(crate) fn confirm_model(
    store: &mut Store,
    run: &cli::Run,
    out: &mut impl Write,
) -> Result<Confirmed, AppError> {
    let asked = ops::model_name(run.model.as_deref())?;
    let base = ops::base_url(run.base_url.as_deref());
    let listing = confirm::served(&base, ops::api_key().as_deref())?;
    let verdict = confirm::decide(
        &asked,
        ops::fingerprint_for(run.fingerprint.as_deref()).as_deref(),
        &listing,
    );
    let note = verdict.note(&asked);
    store.append(Event::Note { text: note.clone() })?;
    writeln!(out, "model {note}")?;

    // B3: `--evict` needs the window the server actually loaded (F818). Without
    // it there is no line to evict at, so it is off and the log says why.
    let evict_window = if run.evict {
        let window = listing.window().map(|w| w as usize);
        let text = match window {
            Some(w) => format!(
                "evict: on, at {}% of the {w}-token window",
                evict::TRIGGER_PERCENT
            ),
            None => "evict: asked for, but the server did not report its window, so off".to_owned(),
        };
        store.append(Event::Note { text: text.clone() })?;
        writeln!(out, "{text}")?;
        window
    } else {
        None
    };

    // 🚨 F539, on the path that spends the budget. A wedged server serves the
    // right model and answers nothing, so the listing above confirms it and the
    // run then spends both attempts against something that cannot reply. One
    // token settles it before any of that is bought.
    //
    // ▶ **And it now refuses** — David's ruling of 2026-08-30. See
    // `pulse::refusal_advice` for why a measurement may stop a preflight where
    // the rate beside it may not. The note reaches the log **before** the
    // refusal, because the reason a run did not start is the most useful thing
    // an otherwise empty log can carry.
    let beat = pulse::take(&base, &asked, ops::api_key().as_deref());
    let beat_note = format!("pulse before the attempt: {beat}");
    store.append(Event::Note {
        text: beat_note.clone(),
    })?;
    writeln!(out, "{beat_note}")?;

    // 🚨 ADR-0014 §6: the weights are the one ungated input, so the run says on
    // its own log which bytes answered it. `Effort::Cheap` because a full digest
    // is 51.9 s on the champion — see `weights` for the measurement and for why
    // the outcome names which of the two checks actually ran.
    //
    // ⚠ It reports and does not refuse. The ADR asks for *a run-visible event*,
    // and a mismatch is as often an operator's own re-download as it is an
    // attack; inventing a refusal the ADR did not ask for would put this check
    // in the class of `confirm`, which has evidence behind its veto.
    weights_note(store, &asked, weights::Effort::Cheap, out)?;

    // ⚠ The model verdict is checked first and keeps its own advice. Both are
    // fatal; that one is more specific, and an operator sent to reload a wedged
    // server when the real fault is an unconfirmed model reloads the wrong
    // thing.
    if !verdict.confirmed() {
        return Err(AppError::Refused(ops::unconfirmed_advice()));
    }
    if beat.refuses() {
        return Err(AppError::Refused(pulse::refusal_advice(&beat)));
    }
    Ok(Confirmed {
        model: asked,
        evict_window,
    })
}

/// What [`confirm_model`] settled before anything was spent.
pub(crate) struct Confirmed {
    pub model: String,
    /// Set only when `--evict` was asked for and the server reported its window.
    pub evict_window: Option<usize>,
}

/// Check the weights against their pin, write the event, and say so.
///
/// 🚨 **One seam for both entry points.** `abcc fleet` calls `confirm_model`
/// too, so the check lands on a sortie's log by construction rather than by
/// somebody remembering to add it to a second place (F330's rule).
///
/// ⚠ A failure to read or write the pin file is reported and does not stop the
/// run. *The weights changed* and *the pin file is unreadable* are different
/// facts and only the first is an alarm; failing the run on the second would
/// make a chore look like an attack.
pub(crate) fn weights_note(
    store: &mut Store,
    asked: &str,
    effort: weights::Effort,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let path = weights::pin_path(&home::shared_root());
    let mut pins = match weights::Pins::load(&path) {
        Ok(pins) => pins,
        Err(e) => {
            let text = format!("weights unchecked: {e}");
            store.append(Event::Note { text: text.clone() })?;
            writeln!(out, "{text}")?;
            return Ok(());
        }
    };

    // ⚠ Same reason as the verb's: the first run against a model pays the full
    // read, in the middle of a preflight, and an unexplained minute of silence
    // there is indistinguishable from the hang ADR-0006 exists to detect.
    if weights::reads_the_file(&pins, asked, effort) {
        writeln!(
            out,
            "weights: no pin for this model yet, reading it once to record one \
             (54 s for the 12.67 GiB champion)"
        )?;
        out.flush()?;
    }
    let checked = weights::check(&mut pins, asked, effort, weights::now_ms());
    if checked.pin.is_some()
        && let Err(e) = pins.save(&path)
    {
        let text = format!("the weights pin could not be written: {e}");
        store.append(Event::Note { text: text.clone() })?;
        writeln!(out, "{text}")?;
    }

    store.append(Event::WeightsChecked {
        model: asked.to_owned(),
        digest: checked.digest.clone(),
        outcome: checked.outcome.clone(),
    })?;
    writeln!(out, "weights {}", describe(&checked))?;
    if checked.alarming() {
        writeln!(
            out,
            "\u{26a0} the bytes behind {asked} are not the bytes that were pinned. If you \
             re-downloaded it, run `abcc weights --repin`; if you did not, stop and find out why."
        )?;
    }
    Ok(())
}

/// The operator's sentence for one check. The feed's is `abcc_tui::line`; this
/// one is for the terminal the command was typed into.
pub(crate) fn describe(checked: &weights::Checked) -> String {
    let short = checked
        .digest
        .as_deref()
        .map_or_else(|| "--".to_owned(), |d| d[..d.len().min(12)].to_owned());
    match &checked.outcome {
        WeightsOutcome::Pinned => format!(
            "pinned at {short} — first sight, so this records the bytes rather than vouching \
             for them"
        ),
        WeightsOutcome::Verified => format!("verified: the file hashes to {short}"),
        WeightsOutcome::Unchanged => {
            format!("unchanged at {short} — length and timestamp match, the file was not re-read")
        }
        WeightsOutcome::Changed { was } => {
            format!("CHANGED: pinned {}, now {short}", &was[..was.len().min(12)])
        }
        WeightsOutcome::Unlocated { why } => format!("unchecked: {why}"),
    }
}

/// The flags an operator typed, folded onto the measured defaults.
///
/// `pub` rather than `pub(crate)` so `tests/cli.rs` can assert the mapping
/// itself: `--reasoning-ceiling 0` means *off*, and a mapping that is only
/// read by `main` is a mapping nothing checks.
#[must_use]
pub fn limits_for(run: &cli::Run) -> Limits {
    let mut limits = Limits::default();
    if let Some(rounds) = run.rounds {
        limits.rounds = rounds;
    }
    if let Some(gap) = run.idle_gap {
        limits.idle_gap = gap;
    }
    // ⚠ **`0` is off, not a ceiling of zero.** `reasoning_chars` starts at 0
    // and the check is `>=`, so a literal 0 would end every turn before its
    // first delta. `usize::MAX` is *no turn ever reaches this*, which is what
    // the operator asking for 0 means and is the arm F831 needs.
    if let Some(chars) = run.reasoning_ceiling {
        limits.reasoning_ceiling = if chars == 0 { usize::MAX } else { chars };
    }
    if let Some(temperature) = run.temperature {
        limits.temperature = temperature;
    }
    limits
}

/// What the attempt was, what it cost, and what is owed to a person.
pub(crate) fn report(landed: &Landed, out: &mut impl Write) -> Result<(), AppError> {
    writeln!(out, "\n{} ended {:?}", landed.attempt, landed.outcome)?;
    phase(out, "localize", &landed.localize)?;
    match &landed.change {
        Some(change) => phase(out, "change  ", change)?,
        // Stated rather than omitted: a phase that did not run is a fact about
        // the attempt, and a blank line would read as a phase that cost nothing.
        None => writeln!(out, "change    did not run — Localize produced no artifact")?,
    }
    if let Some(sha) = &landed.kept {
        writeln!(out, "kept      {sha}")?;
    } else {
        writeln!(out, "kept      nothing — the operator said kill")?;
    }
    gate(landed.gate.as_ref(), out)?;
    judge(landed, out)?;
    writeln!(out, "task      {}", Theme::Command.state(&landed.state))?;
    if let Some(next) = &landed.next {
        writeln!(
            out,
            "next      {next:?}  (a recommendation; nothing acted on it)"
        )?;
    }
    if let TaskState::AwaitingOrders { .. } = landed.state {
        writeln!(
            out,
            "\nThis work is not accepted. Read the checkpoint, then:\n\
             \n  abcc accept <task>   you have read it and take responsibility for it\n  \
             abcc reject <task>   it is not good and the task stops"
        )?;
    }
    Ok(())
}

/// Every rung, then the conjunction they add up to.
///
/// 🚨 The rungs are printed **before** the headline rather than after it, because
/// the headline is a summary of them and a summary read first is a summary that
/// gets believed. ⚠ Nothing here is computed: the headline is
/// `Report::headline_at`, the one function entitled to combine these.
fn gate(measured: Option<&Measured>, out: &mut impl Write) -> Result<(), AppError> {
    let Some(measured) = measured else {
        // Stated rather than omitted, for the same reason a phase that did not
        // run is stated: *not asked* and *asked and found nothing* are two
        // different things and this is the first one.
        //
        // 🚨🚨 **F655: this line used to read *the attempt produced no
        // artifact*, and it was false five times in one session.** It described
        // the branch it was printed from rather than the tree — the driver only
        // gated an `Answered` ending, so an attempt that ran out of rounds was
        // told it had produced nothing while its change sat in the checkpoint.
        // ⚠ **A sentence that asserts an absence nothing measured is worse than
        // no sentence**: DEBUG-P4's F656 traces four sessions of write-ups
        // inheriting this one as a fact. Now that `Unmeasured` is gated too,
        // the operator's stop is the only way to reach here, and this says so.
        writeln!(
            out,
            "gate      not asked — the operator stopped this attempt"
        )?;
        return Ok(());
    };
    for outcome in measured.report.outcomes() {
        match outcome {
            Outcome::Measured(m) => {
                let mark = if m.exit == 0 { "  ok " } else { "REFUSED" };
                writeln!(out, "  {mark:7} {:<11} exit {}", m.rung, m.exit)?;
                for line in m.detail.lines().take(4) {
                    writeln!(out, "          {line}")?;
                }
            }
            Outcome::Unmeasured { rung, why } => {
                writeln!(out, "  {:7} {rung:<11} {why}", "--")?;
            }
        }
    }
    let (measured_count, declared) = measured.report.measured_fraction();
    writeln!(
        out,
        "gate      {} ({measured_count} of {declared} rung(s) measured)",
        measured.headline
    )?;
    Ok(())
}

/// The one model call in the gate, and what it said.
///
/// 🚨 **Printed after the headline, and it is the only thing here that is.** The
/// rungs come before their own summary because a summary read first is a summary
/// that gets believed; the review comes after the verdict for the opposite
/// reason — it did not contribute to it, and putting it above would read as
/// though it had. Nothing in `landed` is computed from any of this.
fn judge(landed: &Landed, out: &mut impl Write) -> Result<(), AppError> {
    let Some(ended) = &landed.judge else {
        // *Not asked* and *asked and found nothing* are different facts, and the
        // reason it was not asked is on the log as a `Note`.
        writeln!(out, "judge     not asked — see the log for why")?;
        return Ok(());
    };
    phase(out, "judge   ", ended.report())?;
    match ended {
        // The claim lives on the gate's report, because a `Claim` attaches to a
        // `Report` and to nothing else.
        PhaseEnded::Answered { .. } => {
            for claim in landed.gate.as_ref().map_or(&[][..], |g| g.report.claims()) {
                writeln!(out, "\n{} — a report, and it decided nothing:\n", claim.by)?;
                for line in claim.text.lines() {
                    writeln!(out, "  {line}")?;
                }
            }
        }
        // ⚠ Said plainly, and it is not a failure of the attempt: the ending
        // above was decided by the rungs and would be the same if this call had
        // never been made.
        PhaseEnded::Unmeasured { why, .. } => {
            writeln!(out, "          no review: {why}")?;
        }
        PhaseEnded::Stopped { .. } => {
            writeln!(out, "          no review: the operator stopped it")?;
        }
    }
    Ok(())
}

fn phase(out: &mut impl Write, name: &str, report: &PhaseReport) -> Result<(), AppError> {
    writeln!(
        out,
        "{name}  {} turn(s), {} tool call(s), {} denial(s), {}+{} tokens{}, trace {:?}, {} ms",
        report.turns,
        report.tool_calls,
        report.denials,
        report.prompt_tokens,
        report.completion_tokens,
        // `None` is not zero: it means no call reported one, which is a fact
        // about the server rather than about the reasoning.
        report.reasoning_tokens.map_or_else(
            || " (reasoning not reported)".to_owned(),
            |r| format!(" ({r} reasoning)")
        ),
        report.trace,
        report.elapsed_ms
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::gate;

    /// 🚨 **F655: the unasked-gate sentence, asserted where the operator reads
    /// it.**
    ///
    /// It used to say *the attempt produced no artifact* and that was false five
    /// times in one session — the driver gated only an `Answered` ending, so an
    /// attempt cut off mid-change was told it had produced nothing while its
    /// work sat in the checkpoint. DEBUG-P4's F656 traces four sessions of
    /// write-ups inheriting the sentence as a fact, so the string is worth a
    /// test of its own: the driver arm and the sentence that reports it are two
    /// places, and only one of them is what anybody reads.
    #[test]
    fn an_unasked_gate_names_the_operator_and_never_asserts_an_absent_artifact() {
        let mut out = Vec::new();
        gate(None, &mut out).expect("write");
        let said = String::from_utf8(out).expect("utf-8");

        assert!(said.contains("not asked"), "{said}");
        assert!(said.contains("operator"), "{said}");
        assert!(
            !said.contains("no artifact"),
            "the sentence asserts an absence nothing measured: {said}"
        );
    }
}
