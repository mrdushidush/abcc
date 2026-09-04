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
//! # 🚨 The control desk, and the one thing it makes the operator type
//!
//! [`Desk`](crate::desk::Desk) is **one task's** control channel and a sortie
//! moves between tasks, so this command uses [`FleetDesk`] instead. It differs in
//! exactly one thing: **every verb names its task.** `halt t42`, not `halt`.
//!
//! That is not ceremony. A desk bound to whichever task happened to be in flight
//! would send a verb to a task the operator was not looking at, and the window is
//! the length of a keystroke — the operator reads `t42 flying`, types `kill`, and
//! t42 lands while their hand is moving. Naming the task turns a mis-delivery
//! into a refusal, and `slot` answers *what is flying* without changing anything.
//!
//! ⚠ Two orders here are the sortie's rather than a task's. `ground` says **admit
//! nothing more**: what is in flight is flown out and landed normally, which is
//! the stop that was missing — before it, ending a sortie early meant killing an
//! attempt nobody objected to, or `Ctrl-C`. And `slot` is a question, which is
//! the other thing a fleet operator has that a run operator does not: somewhere
//! for the answer to have moved on to.

use std::io::Write;
use std::thread;

use abcc_core::event::Event;
use abcc_core::run::Mode;
use abcc_core::task::TaskState;
use abcc_engine::control::InFlight;
use abcc_engine::openai::OpenAiCompat;
use abcc_engine::tools::Tier;
use abcc_fleet::{Fleet, Grounded, Sortie, StandDown, budget};
use abcc_store::Store;

use crate::desk::{FleetDesk, ORDERS};
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

    let in_flight = InFlight::default();
    let stand_down = StandDown::default();
    // Opened here for `run`'s reason: opening a `Store` rebuilds the projection,
    // which is a write, and doing that underneath a running attempt would be a
    // second writer doing something much larger than one row.
    let desk = FleetDesk::open(&log_path, in_flight.clone(), stand_down.clone())?;

    writeln!(
        out,
        "\nfleet   one slot at {}, {} attempts per task, on {model}\n\
         control: type one of [{ORDERS}] and press enter\n\
         \u{26a0} every verb names its task \u{2014} the slot moves, so `slot` says what is on it\n\
         watch:  abcc watch, in another terminal\n",
        // The ceiling is printed whether or not it was set, because a capped slot
        // is otherwise invisible until a role asks for something and is refused
        // — and the operator reading this line is the one who set it.
        args.ceiling.unwrap_or(Tier::Exec),
        budget::ATTEMPTS
    )?;
    out.flush()?;
    // Detached, and never joined: it blocks on stdin, and on a terminal stdin
    // does not end. `main` returning is what stops it.
    thread::spawn(move || desk.serve(std::io::stderr()));

    let mut fleet = Fleet::new(
        &mut store,
        &ground.repo,
        &provider,
        model,
        ground.home.worktrees(),
    )
    .limits(run::limits_for(args))
    .in_flight(in_flight)
    .stand_down(stand_down);
    if let Some(ceiling) = args.ceiling {
        fleet = fleet.ceiling(ceiling);
    }
    let flown = fleet.sortie()?;

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
            // ⚠ Said as what it is: the sortie came down because it was told to,
            // and whatever was flying at the time landed normally.
            Grounded::StoodDown =>
                "and you stood the sortie down — anything still standing by was not admitted"
                    .to_owned(),
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
