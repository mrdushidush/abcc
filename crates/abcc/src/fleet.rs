//! `abcc fleet` — attempts until the board is quiet, on one slot.
//!
//! This is [`crate::run`] with the loop that `PLAN.md`'s Fleet milestone owes:
//! admission picks the task, the budget decides how many attempts it gets, and
//! the recommendation the driver returns is finally received by something. The
//! setup is `run`'s and deliberately identical — boot first, `RunStarted` before
//! anything else, the model confirmed before the board is touched — because two
//! entry points that set up differently are two things that can disagree about
//! what a run is.
//!
//! # ⚠ No control desk, and that is stated rather than faked
//!
//! [`Desk`](crate::desk::Desk) is **one task's** control channel and a sortie
//! moves between tasks, so this command runs without one: there is no `pause`,
//! `halt`, `kill` or `redirect` here, and `Ctrl-C` is the only stop. Wiring an
//! operator surface across a sortie is the Console milestone's, not this one's —
//! and a desk bound to whichever task happened to be in flight would send a
//! verb to a task the operator was not looking at.
//!
//! Use `abcc run --task t42` when you want the verbs. It runs one attempt, which
//! is what a desk is for.

use std::io::Write;

use abcc_core::event::Event;
use abcc_core::run::Mode;
use abcc_core::task::TaskState;
use abcc_engine::control::ControlPoint;
use abcc_engine::openai::OpenAiCompat;
use abcc_fleet::{Fleet, Grounded, Sortie, budget};
use abcc_store::Store;

use crate::{AppError, Invocation, cli, ops, run};

/// Fly attempts until nothing is admissible, and report every one of them.
///
/// # Errors
///
/// [`AppError`] — no repository, an unconfirmed model, or anything the fleet
/// could not do.
pub fn sortie(
    invocation: &Invocation,
    args: &cli::Run,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let ground = ops::ground(invocation)?;
    let log_path = ground.home.log();
    let mut store = Store::open(&log_path)?;
    let reconciled = store.boot()?;
    run::report_boot(&reconciled, out)?;

    store.append(Event::RunStarted {
        mode: Mode::SinglePlayer,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        pid: std::process::id(),
    })?;

    // Before the board is touched, for the same reason `run` does it here:
    // everything after this point costs real time and would be about the wrong
    // model.
    let model = run::confirm_model(&mut store, args, out)?;

    let base = ops::base_url(args.base_url.as_deref());
    let mut provider = OpenAiCompat::new(&base)?;
    if let Some(key) = ops::api_key() {
        provider = provider.with_api_key(key);
    }

    writeln!(
        out,
        "\nfleet   one slot, {} attempts per task, on {model}\n\
         watch:  abcc watch, in another terminal\n\
         \u{26a0} no control desk here \u{2014} `abcc run --task t42` is the one with the verbs\n",
        budget::ATTEMPTS
    )?;
    out.flush()?;

    let mut control = ControlPoint::new().0;
    let mut fleet = Fleet::new(
        &mut store,
        &ground.repo,
        &provider,
        model,
        ground.home.worktrees(),
    )
    .limits(run::limits_for(args));
    let flown = fleet.sortie(&mut control)?;

    report(&flown, out)
}

/// Every attempt, then why the sortie came down.
///
/// 🚨 The attempts are printed in full and **before** the summary, for the
/// reason the gate's rungs are printed before their headline: a summary read
/// first is a summary that gets believed.
fn report(sortie: &Sortie, out: &mut impl Write) -> Result<(), AppError> {
    for landed in &sortie.flown {
        run::report(landed, out)?;
    }

    let flown = sortie.flown.len();
    writeln!(out, "\n{}", "-".repeat(60))?;
    writeln!(
        out,
        "sortie  {flown} attempt(s) flown, {}",
        match sortie.grounded {
            Grounded::Quiet => "and nothing is standing by".to_owned(),
            Grounded::Operator => "and you stopped one".to_owned(),
            // Not softened: this is the landing having got something wrong, and
            // the sortie stopped rather than spending the GPU on a loop.
            Grounded::HeldBack { task } => format!(
                "then stopped: {task} is standing by with its budget already spent, \
                 which the landing should have made impossible"
            ),
        }
    )?;

    let waiting: Vec<String> = sortie
        .flown
        .iter()
        .filter(|l| matches!(l.state, TaskState::AwaitingOrders { .. }))
        .map(|l| l.attempt.to_string())
        .collect();
    if !waiting.is_empty() {
        writeln!(
            out,
            "\n{} attempt(s) are waiting on you: {}\n\
             Read each checkpoint, then `abcc accept <task>` or `abcc reject <task>`.",
            waiting.len(),
            waiting.join(", ")
        )?;
    }
    Ok(())
}
