//! The subcommands that are not a run: the board, the operator's two endings,
//! the review measurement, and the two probes.

use std::io::Write;
use std::path::PathBuf;

use abcc_core::event::Event;
use abcc_core::redact::Secrets;
use abcc_core::replay::Replay;
use abcc_core::seq::{MissionId, Seq, TaskId};
use abcc_core::task::{AbortReason, Command, TaskState};
use abcc_engine::openai::{API_KEY_ENV, BASE_URL_ENV, DEFAULT_BASE_URL};
use abcc_store::{Applied, Store, TaskRow};
use abcc_tui::Theme;
use abcc_vcs::Repo;

use crate::confirm::{self, FINGERPRINT_ENV, MODEL_ENV};
use crate::feed::StoreFeed;
use crate::pulse;
use crate::{AppError, Home, Invocation, cli, home, operator, run, weights};

/// The repository and the state directory beside it, resolved together because
/// the second is keyed by the first.
pub struct Ground {
    pub repo: Repo,
    pub home: Home,
}

/// Find the checkout and the directory its state lives in.
///
/// # Errors
///
/// [`AppError::Vcs`] if there is no repository here, [`AppError::Io`] if the
/// state directory cannot be made or would sit inside the checkout.
pub fn ground(invocation: &Invocation) -> Result<Ground, AppError> {
    let at = invocation
        .repo
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    let repo = Repo::open(&at)?;
    let home = Home::resolve(invocation.home.clone(), repo.root())?;
    Ok(Ground { repo, home })
}

/// Open the log.
///
/// 🚨 **This does not call [`Store::boot`], and that is not an omission.** Boot's
/// orphan sweep tombstones the attempt behind any slot-holding state and requeues
/// its task — which is exactly right for a process that has just started, and
/// exactly wrong for a second process running beside a live attempt. `abcc board`
/// during a run would otherwise kill the run it was there to look at. Only
/// [`crate::run::attempt`] reconciles, because only it is the run.
///
/// `Store::open` still rebuilds the projection, which is a fold over the log and
/// writes no events.
///
/// ⚠ The consequence, stated rather than left to be found: after a crash the
/// board shows the state the log last recorded — a task still `ENGAGING TARGET`
/// with nothing behind it — until the next `abcc run` reconciles. That is the log
/// telling the truth about itself, and it is better than a listing command that
/// changes what it lists.
///
/// # Errors
///
/// [`AppError::Store`] if the log will not open.
pub fn open_log(home: &Home) -> Result<Store, AppError> {
    Ok(Store::open(&home.log())?)
}

// ---------------------------------------------------------------------------
// where
// ---------------------------------------------------------------------------

/// # Errors
///
/// [`AppError`] if the ground cannot be resolved or `out` will not take the text.
pub fn show_where(invocation: &Invocation, out: &mut impl Write) -> Result<(), AppError> {
    let ground = ground(invocation)?;
    let log = ground.home.log();
    writeln!(out, "repository  {}", ground.repo.root().display())?;
    writeln!(out, "state       {}", ground.home.root().display())?;
    writeln!(
        out,
        "log         {} ({})",
        log.display(),
        if log.exists() {
            "exists"
        } else {
            "not yet written"
        }
    )?;
    writeln!(out, "worktrees   {}", ground.home.worktrees().display())?;
    writeln!(
        out,
        "\nBoth are deliberately outside the repository: a checkpoint stages the whole\n\
         tree, and a log that changed on every event would land in every snapshot."
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// task, board
// ---------------------------------------------------------------------------

/// Put one task on the board, under a mission, creating the mission if the log
/// has none.
///
/// # Errors
///
/// [`AppError`] if the log will not take the writes.
pub fn task(
    invocation: &Invocation,
    prompt: &str,
    title: Option<&str>,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let ground = ground(invocation)?;
    let mut store = open_log(&ground.home)?;
    let mission = match latest_mission(&store)? {
        Some(id) => id,
        None => MissionId::at(
            store
                .append(Event::MissionCreated {
                    title: default_mission_title(&ground),
                })?
                .seq,
        ),
    };
    // The title is what the board shows and the brief opens with, so it is worth
    // something even when nobody typed one: the first line of the prompt is what
    // the operator would have written anyway.
    let title = title.map_or_else(|| first_line(prompt), str::to_owned);
    let created = store.append(Event::TaskCreated {
        mission,
        title: title.clone(),
        prompt: prompt.to_owned(),
    })?;
    let id = TaskId::at(created.seq);
    writeln!(out, "{id}  queued  {title}")?;
    writeln!(out, "\nrun it with:  abcc run --task {id}")?;
    Ok(())
}

/// The board, from the projection.
///
/// ⚠ This is a **projection** query and says so. The reader — `abcc watch` — is
/// the thing that reads the log and only the log, because a board drawn from the
/// projection stays healthy-looking when the run stops writing events. This
/// listing is for the operator who wants one line per task before starting one,
/// which is a different question.
///
/// # Errors
///
/// [`AppError`] if the log will not open.
pub fn board(invocation: &Invocation, out: &mut impl Write, all: bool) -> Result<(), AppError> {
    let ground = ground(invocation)?;
    let store = open_log(&ground.home)?;
    let tasks = store.tasks()?;
    if tasks.is_empty() {
        writeln!(out, "nothing on the board. `abcc task \"<what to do>\"`")?;
        return Ok(());
    }

    //   A green task that already became a commit is finished; one that has not
    // is the most actionable row on the board. The landings say which, and they
    // are the same rows `abcc review` resolves against.
    let landed: Vec<TaskId> = if all {
        Vec::new()
    } else {
        Replay::over(&crate::fun::read_all(&store)?)
            .ladder
            .landings
            .iter()
            .map(|l| l.task)
            .collect()
    };

    let mut hidden = 0usize;
    for row in &tasks {
        if !all && !wants_you(&row.state, !landed.contains(&row.id)) {
            hidden += 1;
            continue;
        }
        writeln!(
            out,
            "{:<6} {:<22} {:<46} {}",
            row.id.to_string(),
            Theme::Command.state(&row.state),
            truncate(&row.title, 46),
            next_step(row.id, &row.state, !landed.contains(&row.id)),
        )?;
    }

    if hidden == 0 {
        writeln!(
            out,
            "\n{} task(s), from the projection. `abcc watch` reads the log itself.",
            tasks.len()
        )?;
    } else if hidden == tasks.len() {
        writeln!(
            out,
            "\nnothing is waiting on you. {hidden} finished task(s) are hidden — \
             `abcc board --all` for the lot, `abcc task \"<what to do>\"` for more work."
        )?;
    } else {
        writeln!(
            out,
            "\n{} task(s) waiting on you; {hidden} finished and hidden — \
             `abcc board --all` for the lot.",
            tasks.len() - hidden
        )?;
    }
    Ok(())
}

/// Whether this row is asking the operator for something.
///
/// 🚨 **Terminality is not the question, and using it alone hid the one row
/// that matters.** `Accomplished` is terminal \u2014 the gate measured every rung
/// green \u2014 and a green task that has not been landed yet is precisely the row
/// an operator opened the board to find. So the rule is *not finished*, where
/// finished means terminal **and** nothing left to do about it.
fn wants_you(state: &TaskState, unlanded: bool) -> bool {
    match state {
        TaskState::Accomplished { .. } => unlanded,
        other => !other.is_terminal(),
    }
}

/// The command this row is waiting for, in the words the operator types.
///
/// One line of the runbook, rendered where it is needed rather than in a file
/// somebody has to remember to open.
fn next_step(id: TaskId, state: &TaskState, unlanded: bool) -> String {
    match state {
        TaskState::Queued => format!("abcc run --task {id}"),
        TaskState::Accomplished { .. } if unlanded => format!("abcc land {id}"),
        //   Every exit from AwaitingOrders is an operator's (F732), and the
        // first move is always to read what happened rather than to guess.
        TaskState::AwaitingOrders { .. } | TaskState::Holding { .. } => {
            format!("abcc replay {id}")
        }
        TaskState::Commandeered { .. } => format!("abcc release {id}"),
        TaskState::Deployed { .. } | TaskState::Engaged { .. } => "running — abcc watch".to_owned(),
        _ => String::new(),
    }
}

/// A title, cut to fit beside the column that tells you what to type.
fn truncate(title: &str, width: usize) -> String {
    if title.chars().count() <= width {
        return title.to_owned();
    }
    let kept: String = title.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// The reader, over the durable log.
///
/// # Errors
///
/// [`AppError`] if the log will not open or the terminal will not go into raw
/// mode.
pub fn watch(invocation: &Invocation, theme: Theme) -> Result<(), AppError> {
    let ground = ground(invocation)?;
    // 🚨 Opened as a `StoreFeed` and never as a `Store` the caller can query:
    // the reader gets a paged read of `event` and has no way to ask the
    // projection anything, which is the rule enforced by what is in scope.
    let feed = StoreFeed::open(&ground.home.log())?;
    abcc_tui::run(&feed, theme)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

/// Ask the server which model it is holding, and say whether that is the one.
///
/// Needs no repository and no log: it is a probe of a socket and a piece of
/// operator configuration.
///
/// # Errors
///
/// [`AppError::Confirm`] if the server cannot be asked, [`AppError::Refused`] if
/// the answer does not confirm the model.
pub fn check(
    model: Option<&str>,
    url: Option<&str>,
    fingerprint: Option<&str>,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let asked = model_name(model)?;
    let base = base_url(url);
    let listing = confirm::served(&base, api_key().as_deref())?;
    let verdict = confirm::decide(&asked, fingerprint_for(fingerprint).as_deref(), &listing);
    writeln!(out, "server      {base}")?;
    writeln!(
        out,
        "{:<11} [{}]",
        match listing.evidence() {
            // 🚨 F495: these are two different answers, so they get two different
            // words. LM Studio's OpenAI listing is everything downloaded.
            confirm::Evidence::Loaded => "loaded",
            confirm::Evidence::Listed => "listed",
        },
        listing.ids().join(", ")
    )?;
    writeln!(out, "{}", verdict.note(&asked))?;
    // 🚨 F539. The listing above is exactly the check that missed the only
    // outage this project has had: it answered normally for 60 s while every
    // completion returned nothing. So the check does not end on a listing any
    // more — it asks for one real token.
    //
    // ▶ **And a silent one fails this command.** That follows David's ruling of
    // 2026-08-30 rather than extending it: `abcc check` already refuses on the
    // *weaker* fault of an unconfirmed model, and a health command that exits 0
    // about a server nothing can run against is a health check that lies to
    // whatever script asked it. `harness/scripts/ping.sh` exits 1 on this same
    // condition, and two health checks on one box disagreeing is worse than
    // either answer alone.
    let beat = pulse::take(&base, &asked, api_key().as_deref());
    writeln!(out, "pulse       {beat}")?;
    if !verdict.confirmed() {
        return Err(AppError::Refused(unconfirmed_advice()));
    }
    if beat.refuses() {
        return Err(AppError::Refused(pulse::refusal_advice(&beat)));
    }
    Ok(())
}

/// What to do about an unconfirmed model, said once and used by both callers.
pub(crate) fn unconfirmed_advice() -> String {
    format!(
        "Nothing will run against a model that has not been confirmed. LM Studio answers a \
         request naming a model it does not have by using whichever model *is* loaded, so an \
         unconfirmed run produces numbers that look exactly like good ones. Load the model, or \
         set {FINGERPRINT_ENV} to a substring of the id the server reports."
    )
}

/// The model to ask for.
///
/// # Errors
///
/// [`AppError::Refused`] if neither `--model` nor the environment names one.
pub fn model_name(explicit: Option<&str>) -> Result<String, AppError> {
    explicit
        .map(str::to_owned)
        .or_else(|| std::env::var(MODEL_ENV).ok())
        .map(|m| m.trim().to_owned())
        .filter(|m| !m.is_empty())
        .ok_or_else(|| {
            AppError::Refused(format!(
                "no model named. Pass --model, or set {MODEL_ENV}. There is no default: a run \
                 whose model was guessed is a run whose numbers are about something nobody chose."
            ))
        })
}

pub(crate) fn base_url(explicit: Option<&str>) -> String {
    explicit
        .map(str::to_owned)
        .or_else(|| std::env::var(BASE_URL_ENV).ok())
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned())
}

pub(crate) fn api_key() -> Option<String> {
    std::env::var(API_KEY_ENV).ok().filter(|k| !k.is_empty())
}

/// 🚨 **The one recipe for the denylist** (ADR-0014 §5).
///
/// A free function with one caller per entry point rather than a `Secrets` built
/// beside each provider, for `abcc_drive::snapshot`'s reason (F330): two places
/// that assemble one policy are two places that agree until the day somebody
/// adds a credential to the first of them.
///
/// What goes in is what this process actually holds — today that is the model
/// API key and nothing else, because [`ENV_ALLOWLIST`] is what a tool child
/// inherits and it names no credential. ⚠ A key shorter than the redactor's
/// floor is dropped rather than kept, so `Secrets::literals` can be 0 here and
/// that is the honest answer, not a failure.
///
/// [`ENV_ALLOWLIST`]: abcc_engine::child::ENV_ALLOWLIST
pub(crate) fn secrets() -> Secrets {
    match api_key() {
        Some(key) => Secrets::default().with_literal(key),
        None => Secrets::default(),
    }
}

pub(crate) fn fingerprint_for(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::to_owned)
        .or_else(|| std::env::var(FINGERPRINT_ENV).ok())
        .filter(|f| !f.trim().is_empty())
}

// ---------------------------------------------------------------------------
// review
// ---------------------------------------------------------------------------

/// 🚨 W13's ladder, which is measured in **human review minutes per merged
/// change** and never in agent-authored commits.
///
/// `PLAN.md` §5 requires this from Skeleton onward. It arrives here already in
/// seconds — the minutes an operator types are converted once, at the argument
/// edge, because a float in a record whose whole purpose is to be summed over
/// months stops summing exactly.
///
/// # Errors
///
/// [`AppError`] if the log will not take the write.
pub fn review(
    invocation: &Invocation,
    change: &str,
    seconds: u32,
    by: Option<&str>,
    crossed_boundary: bool,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let ground = ground(invocation)?;
    let mut store = open_log(&ground.home)?;
    let change = landed(&store, change)?;
    let by = by.map_or_else(operator, str::to_owned);
    store.append(Event::ReviewRecorded {
        change: change.clone(),
        seconds,
        by: by.clone(),
        crossed_boundary,
    })?;
    writeln!(
        out,
        "recorded  {change}  {:.1} min  by {by}{}",
        f64::from(seconds) / 60.0,
        if crossed_boundary {
            "  (crossed a module boundary)"
        } else {
            ""
        }
    )?;
    Ok(())
}

/// What the operator typed, resolved to the sha a landing actually made.
///
/// 🚨 **F796: the ladder's two halves join by exact string, and until this
/// nothing enforced it.** [`Event::ChangeLanded`] writes the full forty-character
/// sha; `review` wrote whatever it was handed. Four landings and four reviews
/// produced **zero** joinable pairs -- three of the reviews named an *attempt*
/// and one an abbreviation -- so both counters read four while the intersection
/// was empty. [`abcc_core::replay::Landed::change`] already carried the warning
/// that the two are compared as strings and that whoever reviews a landing must
/// name it the way the landing printed it. **A comment is not a guard.**
///
/// ⚠ **A review may not name a change `abcc land` did not make**, and that is
/// a ruling rather than an omission. F727: the ladder is *of the changes abcc
/// landed* and never *of the repository*, so a commit merged by hand is outside
/// it by construction. This refusal is how that stays true.
///
/// # Errors
///
/// [`AppError::Refused`] if nothing the log landed answers to `named`.
fn landed(store: &Store, named: &str) -> Result<String, AppError> {
    let log = crate::fun::read_all(store)?;
    let landings = Replay::over(&log).ladder.landings;
    let named = named.trim();

    if let Some(hit) = landings.iter().find(|l| l.change == named) {
        return Ok(hit.change.clone());
    }
    if let Some(hit) = task_named(named).and_then(|task| landings.iter().find(|l| l.task == task)) {
        return Ok(hit.change.clone());
    }
    //   An abbreviation, but only an unambiguous one. Two landings sharing a
    // prefix is a question this cannot answer, and guessing at it is the whole
    // of what F796 is about. Four is git's own floor for a short sha.
    if named.len() >= 4 {
        let mut hits = landings.iter().filter(|l| l.change.starts_with(named));
        if let (Some(hit), None) = (hits.next(), hits.next()) {
            return Ok(hit.change.clone());
        }
    }
    Err(AppError::Refused(format!(
        "no change `abcc land` made here is called {named}. A review names the \
         commit: its full sha, an unambiguous abbreviation of it, or the task it \
         came from. The log holds {} landing(s).",
        landings.len()
    )))
}

/// `t42` or `42`, which is how every other operator verb spells a task.
fn task_named(named: &str) -> Option<TaskId> {
    named
        .strip_prefix('t')
        .unwrap_or(named)
        .parse::<i64>()
        .ok()
        .map(|n| TaskId::at(Seq::new(n)))
}

// ---------------------------------------------------------------------------
// accept / reject
// ---------------------------------------------------------------------------

/// The operator's two endings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    /// The work is good and the operator takes responsibility for saying so.
    Accept,
    /// The work is not good and the task stops.
    Reject,
}

/// End a task the way a person decided it should end.
///
/// 🚨 **Neither of these is `Accomplished`, and that is the point.** There is no
/// gate until the Gate milestone, so `abcc-drive` ends a working attempt
/// `Uncertain { NoCheckerForArtifact }` and leaves the task in `AwaitingOrders` —
/// which is not terminal. `accept` is the honest way out: the operator
/// commandeers the task and finishes it by hand, which is
/// `Aborted { CompletedByOperator }`, the variant that exists for exactly this.
/// Mapping a model's answer onto `Accomplished` would be a claim standing where a
/// measurement belongs, and it is the one shortcut that would make the milestone
/// dishonest.
///
/// ⚠ `reject` is `Aborted { Operator }` rather than `Failed`, because `Fail`
/// names an attempt and is legal only from `Engaged`: from `AwaitingOrders` the
/// attempt is already over, and the thing being ended is the task.
///
/// 🚨 **Both close the operator's workspace on the way out, and the reason is in
/// the type.** All three terminal states have `holds_workspace: false` on their
/// [`abcc_core::task::StateContract`], and until `abcc take` existed that was a
/// claim nothing could break: the driver removes its own worktree before it sends
/// the landing command, so no task had one left to hold. A task the operator took
/// over does, and a task may not go terminal still holding a directory. The work
/// is snapshotted before the tree comes down — which is what `reject` already
/// promises when it says the checkpoints are still on refs.
///
/// # Errors
///
/// [`AppError::Refused`] if the task is not there or a command was refused;
/// [`AppError::Store`] if the log will not take the writes.
pub fn finish(
    invocation: &Invocation,
    task: cli::TaskRef,
    note: Option<&str>,
    finish: Finish,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let ground = ground(invocation)?;
    let mut store = open_log(&ground.home)?;
    let id = TaskId::at(Seq::new(task.0));
    let row = row(&store, id)?;

    // The answer is recorded against the standing question when there is one. A
    // task that is not waiting on a person has no prompt to answer, and inventing
    // a `PromptId` to satisfy the type would put a question on the log that was
    // never asked.
    let said = note.map_or_else(
        || match finish {
            Finish::Accept => "read and accepted by the operator".to_owned(),
            Finish::Reject => "read and stopped by the operator".to_owned(),
        },
        str::to_owned,
    );
    if let TaskState::AwaitingOrders { prompt, .. } = row.state {
        store.append(Event::OperatorAnswered {
            task: id,
            prompt,
            answer: said.clone(),
        })?;
    } else {
        store.append(Event::Note {
            text: format!("{id}: {said}"),
        })?;
    }

    // Before the abort, because the abort is what makes it terminal. `None` for
    // every task nobody took over, which is every task on a log written before
    // the verb existed.
    let kept = crate::takeover::hand_back(&mut store, &ground, &row, out)?;

    let by = operator();
    let reason = match finish {
        Finish::Accept => {
            // The keyboard is taken first, because that is what happened: nothing
            // measured the work and a person is standing behind it instead.
            command(&mut store, id, Command::Commandeer)?;
            AbortReason::CompletedByOperator { by: by.clone() }
        }
        Finish::Reject => AbortReason::Operator { by: by.clone() },
    };
    let state = command(&mut store, id, Command::Abort { reason })?;

    writeln!(out, "{id}  {}", Theme::Command.state(&state))?;
    match finish {
        Finish::Accept => writeln!(
            out,
            "Recorded as completed by {by}. It is not `Accomplished`: nothing measured this \
             work, so the record says a person accepted it, not that a gate passed it."
        )?,
        Finish::Reject => writeln!(out, "Stopped by {by}. The checkpoints are still on refs.")?,
    }
    if let Some(sha) = kept {
        writeln!(out, "Your workspace is closed; what was in it is at {sha}.")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// the pieces
// ---------------------------------------------------------------------------

/// Send a command and read back where the task landed.
pub(crate) fn command(
    store: &mut Store,
    task: TaskId,
    command: Command,
) -> Result<TaskState, AppError> {
    let name = command.name();
    match store.apply(task, command)? {
        Applied::Moved(_) => Ok(row(store, task)?.state),
        Applied::Refused { refusal, .. } => {
            Err(AppError::Refused(format!("{name} was refused: {refusal}")))
        }
    }
}

pub(crate) fn row(store: &Store, task: TaskId) -> Result<TaskRow, AppError> {
    store.task(task)?.ok_or_else(|| {
        AppError::Refused(format!(
            "there is no {task} on this board — try `abcc board`"
        ))
    })
}

/// The mission the last task went on, so a second `abcc task` joins the first
/// rather than starting a second mission nobody asked for.
fn latest_mission(store: &Store) -> Result<Option<MissionId>, AppError> {
    let mut latest = None;
    let mut since = Seq::ORIGIN;
    loop {
        let page = store.read_from(since, 512)?;
        if page.is_empty() {
            return Ok(latest);
        }
        for logged in &page {
            since = logged.seq;
            if matches!(logged.event, Event::MissionCreated { .. }) {
                latest = Some(MissionId::at(logged.seq));
            }
        }
    }
}

fn default_mission_title(ground: &Ground) -> String {
    ground.repo.root().file_name().map_or_else(
        || "mission".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn first_line(prompt: &str) -> String {
    const CAP: usize = 72;
    let line = prompt.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return "untitled".to_owned();
    }
    if line.chars().count() <= CAP {
        return line.to_owned();
    }
    let short: String = line.chars().take(CAP).collect();
    format!("{short}...")
}

/// 🚨 `abcc weights` — the one ungated input, looked at on purpose.
///
/// Three shapes, and they are deliberately three words rather than one flag with
/// a mode:
///
/// * bare — the same cheap check a run does, said out loud;
/// * `--verify` — read the whole file, **51.9 s on the champion**, and the only
///   thing that answers the question the threat-model row asks;
/// * `--repin` — accept what is there as the new reference. ⚠ Separate from
///   `--verify` because re-pinning after a mismatch is a **decision**, not a
///   repair, and a `--verify` that quietly re-pinned would turn the alarm into
///   a formality.
///
/// # Errors
///
/// Fails if the repository or its log cannot be opened, or if the pin file
/// exists and cannot be read.
pub fn weights(
    invocation: &Invocation,
    model: Option<&str>,
    verify: bool,
    repin: bool,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let ground = ground(invocation)?;
    let mut store = open_log(&ground.home)?;
    let asked = model_name(model)?;
    let path = weights::pin_path(&home::shared_root());
    let mut pins = weights::Pins::load(&path)?;

    if repin {
        // Forget this model's pin first, so the check that follows takes what is
        // there as a first sight. ⚠ It goes through the same `check` rather than
        // writing a `Pin` directly: one recipe for what a pin contains (F330).
        pins.forget(&asked);
    }
    let effort = if verify || repin {
        weights::Effort::Full
    } else {
        weights::Effort::Cheap
    };
    // ⚠ Asked rather than inferred from the flags. A **first sight reads the
    // file too**, so a bare `abcc weights` on an unpinned model pauses for the
    // better part of a minute — and a pause with no sentence in front of it is
    // how a check gets reported as a hang.
    if weights::reads_the_file(&pins, &asked, effort) {
        writeln!(
            out,
            "reading the whole file — 54 s on this box for the 12.67 GiB champion"
        )?;
        out.flush()?;
    }

    let checked = weights::check(&mut pins, &asked, effort, weights::now_ms());
    if checked.pin.is_some() {
        pins.save(&path)?;
    }
    store.append(Event::WeightsChecked {
        model: asked.clone(),
        digest: checked.digest.clone(),
        outcome: checked.outcome.clone(),
    })?;

    writeln!(out, "{asked}")?;
    if let Some(pin) = pins.get(&asked) {
        writeln!(out, "  file    {}", pin.path)?;
        writeln!(out, "  bytes   {}", pin.identity.len)?;
    }
    writeln!(out, "  {}", run::describe(&checked))?;
    if checked.alarming() {
        writeln!(
            out,
            "\n\u{26a0} If you re-downloaded this model, `abcc weights --repin` accepts what is \
             there. If you did not, the bytes changed underneath the name and that is the thing \
             this check exists to tell you."
        )?;
    }
    Ok(())
}
