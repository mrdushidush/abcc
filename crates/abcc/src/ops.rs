//! The subcommands that are not a run: the board, the operator's two endings,
//! the review measurement, and the two probes.

use std::io::Write;
use std::path::PathBuf;

use abcc_core::event::Event;
use abcc_core::seq::{MissionId, Seq, TaskId};
use abcc_core::task::{AbortReason, Command, TaskState};
use abcc_engine::openai::{API_KEY_ENV, BASE_URL_ENV, DEFAULT_BASE_URL};
use abcc_store::{Applied, Store, TaskRow};
use abcc_tui::Theme;
use abcc_vcs::Repo;

use crate::confirm::{self, FINGERPRINT_ENV, MODEL_ENV};
use crate::feed::StoreFeed;
use crate::pulse;
use crate::{AppError, Home, Invocation, cli, operator};

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
pub fn board(invocation: &Invocation, out: &mut impl Write) -> Result<(), AppError> {
    let ground = ground(invocation)?;
    let store = open_log(&ground.home)?;
    let tasks = store.tasks()?;
    if tasks.is_empty() {
        writeln!(out, "nothing on the board. `abcc task \"<what to do>\"`")?;
        return Ok(());
    }
    for row in &tasks {
        writeln!(
            out,
            "{:<6} {:<22} {}",
            row.id.to_string(),
            Theme::Command.state(&row.state),
            row.title
        )?;
    }
    writeln!(
        out,
        "\n{} task(s), from the projection. `abcc watch` reads the log itself.",
        tasks.len()
    )?;
    Ok(())
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
    // more \u2014 it asks for one real token.
    //
    // \u26a0 It **reports and never gates** (ADR-0010 \u00a74). A silent pulse does not
    // change this command's exit status, because the ruling that only
    // deterministic rungs may refuse is about the gate and the operator is the
    // one who acts on a health report.
    let beat = pulse::take(&base, &asked, api_key().as_deref());
    writeln!(out, "pulse       {beat}")?;
    if !beat.answered() {
        writeln!(
            out,
            "            \u{26a0} the listing above answered and this did not. That is the shape \
             of F539."
        )?;
    }
    if verdict.confirmed() {
        Ok(())
    } else {
        Err(AppError::Refused(unconfirmed_advice()))
    }
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
    let by = by.map_or_else(operator, str::to_owned);
    store.append(Event::ReviewRecorded {
        change: change.to_owned(),
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
