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
use abcc_core::event::Event;
use abcc_core::run::Mode;
use abcc_core::seq::{Seq, TaskId, UnitId};
use abcc_core::task::TaskState;
use abcc_drive::{Driver, Landed};
use abcc_engine::control::ControlPoint;
use abcc_engine::openai::OpenAiCompat;
use abcc_engine::turn::{Limits, PhaseReport};
use abcc_store::{Reconciled, Store};
use abcc_tui::Theme;

use crate::desk::{Desk, VERBS};
use crate::{AppError, Invocation, cli, confirm, ops};

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
        version: env!("CARGO_PKG_VERSION").to_owned(),
        pid: std::process::id(),
    })?;

    let task = choose(&store, run.task, out)?;

    // 🚨 Before the checkpoint, before the worktree, before the slot. Everything
    // after this point costs real time and produces real records, and all of it
    // would be about the wrong model.
    let model = confirm_model(&mut store, run, out)?;

    let base = ops::base_url(run.base_url.as_deref());
    let mut provider = OpenAiCompat::new(&base)?;
    if let Some(key) = ops::api_key() {
        provider = provider.with_api_key(key);
    }

    let limits = limits_for(run);
    let unit = UnitId(run.unit.unwrap_or(0));
    let (mut control, handle) = ControlPoint::new();

    // Opened here, on purpose: see the module docs, point 4.
    let desk = Desk::open(&log_path, handle, task)?;
    writeln!(
        out,
        "\nrunning {task} in unit {unit} on {model}\n\
         control: type one of [{VERBS}] and press enter\n\
         watch:   abcc watch, in another terminal\n"
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
    .run(task, unit, Cause::Fresh, &mut control)?;

    report(&landed, out)
}

// ---------------------------------------------------------------------------
// the pieces
// ---------------------------------------------------------------------------

fn report_boot(reconciled: &Reconciled, out: &mut impl Write) -> Result<(), AppError> {
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
            return Err(AppError::Refused(format!(
                "{id} is {}, and a run starts from standing-by. `abcc board`",
                Theme::Command.state(&row.state)
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
fn confirm_model(
    store: &mut Store,
    run: &cli::Run,
    out: &mut impl Write,
) -> Result<String, AppError> {
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
    if verdict.confirmed() {
        Ok(asked)
    } else {
        Err(AppError::Refused(ops::unconfirmed_advice()))
    }
}

fn limits_for(run: &cli::Run) -> Limits {
    let mut limits = Limits::default();
    if let Some(rounds) = run.rounds {
        limits.rounds = rounds;
    }
    if let Some(gap) = run.idle_gap {
        limits.idle_gap = gap;
    }
    limits
}

/// What the attempt was, what it cost, and what is owed to a person.
fn report(landed: &Landed, out: &mut impl Write) -> Result<(), AppError> {
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
            "\nNothing measured this work — there is no gate until the Gate milestone, so the\n\
             attempt ended `Uncertain` rather than accomplished. Read the checkpoint, then:\n\
             \n  abcc accept <task>   you have read it and take responsibility for it\n  \
             abcc reject <task>   it is not good and the task stops"
        )?;
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
