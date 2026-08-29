//! The argument surface, parsed by hand.
//!
//! No parser dependency. ADR-0006 counts this workspace's dependencies one at a
//! time and each one is argued for; a flag table this small does not earn one,
//! and the parse being a pure function over a `Vec<String>` is what lets every
//! shape below be a test rather than something discovered at a terminal.

use std::path::PathBuf;
use std::time::Duration;

use abcc_tui::Theme;

/// What to do, and the two things every subcommand shares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The operator's checkout. Defaults to the working directory.
    pub repo: Option<PathBuf>,
    /// Where the log and the worktrees live. Defaults per [`crate::home`].
    pub home: Option<PathBuf>,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Print the resolved paths and stop.
    Where,
    /// Put a task on the board.
    Task {
        prompt: String,
        title: Option<String>,
    },
    /// The board, from the projection.
    Board,
    /// One attempt, end to end.
    Run(Box<Run>),
    /// The reader, over the durable log.
    Watch { theme: Theme },
    /// The model-confirmation check on its own.
    Check {
        model: Option<String>,
        base_url: Option<String>,
        fingerprint: Option<String>,
    },
    /// W13's ladder measurement.
    ///
    /// 🚨 Minutes go in and **seconds come out, here at the edge**. The ladder's
    /// unit is minutes and the record's unit is seconds, because a float in a
    /// quantity whose whole purpose is to be summed over months stops summing
    /// exactly — so the one conversion happens once, at the boundary, where it is
    /// a tested function rather than an arithmetic expression somewhere inland.
    Review {
        change: String,
        seconds: u32,
        by: Option<String>,
        crossed_boundary: bool,
    },
    /// The operator has read the work and takes responsibility for it.
    Accept { task: TaskRef, note: Option<String> },
    /// The operator has read the work and stops the task.
    Reject { task: TaskRef, note: Option<String> },
}

/// Everything `run` takes. Boxed in [`Command`] because it is much the largest
/// variant and clippy is right about that.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Run {
    /// The task to run. Defaults to the first `Queued` one on the board.
    pub task: Option<TaskRef>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub fingerprint: Option<String>,
    pub unit: Option<u8>,
    pub rounds: Option<u32>,
    pub idle_gap: Option<Duration>,
}

/// A task named on the command line, as `t42` or `42`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskRef(pub i64);

/// The parse did not produce an invocation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CliError {
    /// `--help`, or no arguments at all. Not a failure.
    #[error("{USAGE}")]
    Help,
    #[error("{0}\n\n{USAGE}")]
    Usage(String),
}

pub const USAGE: &str = "\
abcc — the command center. One attempt at a time, over one repository.

  abcc where                          where this repository's log and worktrees are
  abcc task <prompt> [--title T]      put a task on the board
  abcc board                          the board, from the projection
  abcc run [--task t42] [options]     run one attempt on a queued task
  abcc watch [--theme command|classic]  the reader, over the log alone
  abcc check [--model M]              ask the server which model it is holding
  abcc review <change> <minutes> [--by W] [--boundary]
  abcc accept <task> [--note N]       the work is good; you take responsibility
  abcc reject <task> [--note N]       stop the task

Everywhere:
  --repo <path>     the checkout to work on (default: the working directory)
  --home <path>     where the log and worktrees live (default: outside the repo)

run:
  --model <name>    the model to ask for (default: $ABCC_MODEL)
  --url <base>      the server (default: $ABCC_MODEL_BASE_URL)
  --fingerprint <s> a substring that must appear in the served model id
  --unit <n>        the slot to run in (default: 0)
  --rounds <n>      tool rounds before the phase gives up
  --idle-gap <s>    seconds of silence on the stream that count as a hang

The run reads control verbs from stdin: pause | halt | kill | redirect <prompt>.
";

/// Parse an argument list, without the program name.
///
/// # Errors
///
/// [`CliError::Help`] for `--help` or an empty list, [`CliError::Usage`] for
/// anything that is not an invocation.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Invocation, CliError> {
    let mut args: Vec<String> = args.into_iter().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Err(CliError::Help);
    }
    if args.is_empty() {
        return Err(CliError::Help);
    }

    // The two global flags are pulled out first so they can appear anywhere,
    // which is what an operator expects of `--repo`.
    let repo = take_flag(&mut args, "--repo")?.map(PathBuf::from);
    let home = take_flag(&mut args, "--home")?.map(PathBuf::from);

    let verb = args.remove(0);
    let command = match verb.as_str() {
        "where" => Command::Where,
        "board" => Command::Board,
        "task" => {
            let title = take_flag(&mut args, "--title")?;
            let prompt = one_positional(&args, "task", "a prompt")?;
            Command::Task { prompt, title }
        }
        "run" => {
            let run = Run {
                task: take_flag(&mut args, "--task")?
                    .map(|t| task_ref(&t))
                    .transpose()?,
                model: take_flag(&mut args, "--model")?,
                base_url: take_flag(&mut args, "--url")?,
                fingerprint: take_flag(&mut args, "--fingerprint")?,
                unit: take_flag(&mut args, "--unit")?
                    .map(|u| number(&u, "--unit"))
                    .transpose()?,
                rounds: take_flag(&mut args, "--rounds")?
                    .map(|r| number(&r, "--rounds"))
                    .transpose()?,
                idle_gap: take_flag(&mut args, "--idle-gap")?
                    .map(|g| seconds(&g, "--idle-gap"))
                    .transpose()?,
            };
            no_positionals(&args, "run")?;
            Command::Run(Box::new(run))
        }
        "watch" => {
            let theme = match take_flag(&mut args, "--theme")?.as_deref() {
                None | Some("command") => Theme::Command,
                Some("classic") => Theme::Classic,
                Some(other) => {
                    return Err(CliError::Usage(format!(
                        "--theme takes `command` or `classic`, not {other:?}"
                    )));
                }
            };
            no_positionals(&args, "watch")?;
            Command::Watch { theme }
        }
        "check" => {
            let model = take_flag(&mut args, "--model")?;
            let base_url = take_flag(&mut args, "--url")?;
            let fingerprint = take_flag(&mut args, "--fingerprint")?;
            no_positionals(&args, "check")?;
            Command::Check {
                model,
                base_url,
                fingerprint,
            }
        }
        "review" => {
            let by = take_flag(&mut args, "--by")?;
            let crossed_boundary = take_switch(&mut args, "--boundary");
            if args.len() != 2 {
                return Err(CliError::Usage(
                    "review takes a change and the minutes spent reviewing it: \
                     `abcc review <sha or task> <minutes>`"
                        .to_owned(),
                ));
            }
            Command::Review {
                change: args[0].clone(),
                seconds: minutes_as_seconds(&args[1])?,
                by,
                crossed_boundary,
            }
        }
        "accept" | "reject" => {
            let note = take_flag(&mut args, "--note")?;
            let task = task_ref(&one_positional(&args, &verb, "a task")?)?;
            if verb == "accept" {
                Command::Accept { task, note }
            } else {
                Command::Reject { task, note }
            }
        }
        other => {
            return Err(CliError::Usage(format!("{other:?} is not a command")));
        }
    };

    Ok(Invocation {
        repo,
        home,
        command,
    })
}

/// Minutes as an operator types them, as the seconds the record stores.
///
/// Rejects everything that is not a length of time somebody spent: a negative
/// number, an infinity, and anything that would not fit the column. A review that
/// took no time at all is allowed — the ladder counts it, and zero is a
/// measurement.
fn minutes_as_seconds(raw: &str) -> Result<u32, CliError> {
    let minutes = raw
        .parse::<f64>()
        .map_err(|_| CliError::Usage(format!("{raw:?} is not a number of minutes")))?;
    let seconds = (minutes * 60.0).round();
    if !seconds.is_finite() || !(0.0..=f64::from(u32::MAX)).contains(&seconds) {
        return Err(CliError::Usage(format!(
            "{raw:?} is not a length of time somebody spent reviewing something"
        )));
    }
    // The range is checked immediately above, which is what the cast needs.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(seconds as u32)
}

/// `t42` and `42` are the same task. The prefix is what the id prints as, so
/// accepting it means an operator can paste back what the board showed them.
fn task_ref(raw: &str) -> Result<TaskRef, CliError> {
    let digits = raw.strip_prefix('t').unwrap_or(raw);
    digits
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .map(TaskRef)
        .ok_or_else(|| CliError::Usage(format!("{raw:?} is not a task id — they look like `t42`")))
}

fn number<T: std::str::FromStr>(raw: &str, flag: &str) -> Result<T, CliError> {
    raw.parse::<T>()
        .map_err(|_| CliError::Usage(format!("{flag} takes a whole number, not {raw:?}")))
}

fn seconds(raw: &str, flag: &str) -> Result<Duration, CliError> {
    let secs = raw
        .parse::<f64>()
        .ok()
        .filter(|s| s.is_finite() && *s > 0.0)
        .ok_or_else(|| CliError::Usage(format!("{flag} takes seconds, not {raw:?}")))?;
    Ok(Duration::from_secs_f64(secs))
}

/// Remove `--flag value` wherever it appears and return the value.
fn take_flag(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, CliError> {
    let Some(at) = args.iter().position(|a| a == flag) else {
        return Ok(None);
    };
    if at + 1 >= args.len() {
        return Err(CliError::Usage(format!("{flag} needs a value after it")));
    }
    args.remove(at);
    Ok(Some(args.remove(at)))
}

fn take_switch(args: &mut Vec<String>, flag: &str) -> bool {
    args.iter()
        .position(|a| a == flag)
        .is_some_and(|at| args.remove(at) == flag)
}

fn one_positional(args: &[String], verb: &str, wanted: &str) -> Result<String, CliError> {
    match args {
        [only] => Ok(only.clone()),
        [] => Err(CliError::Usage(format!("{verb} needs {wanted}"))),
        _ => Err(CliError::Usage(format!(
            "{verb} takes {wanted} and nothing else — quote it if it has spaces in it"
        ))),
    }
}

fn no_positionals(args: &[String], verb: &str) -> Result<(), CliError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(CliError::Usage(format!(
            "{verb} takes no arguments, and got {:?}",
            args[0]
        )))
    }
}
