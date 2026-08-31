//! The argument surface, parsed by hand.
//!
//! No parser dependency. ADR-0006 counts this workspace's dependencies one at a
//! time and each one is argued for; a flag table this small does not earn one,
//! and the parse being a pure function over a `Vec<String>` is what lets every
//! shape below be a test rather than something discovered at a terminal.

use std::path::PathBuf;
use std::time::Duration;

use abcc_engine::Tier;
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
    /// Attempts until the board is quiet, on one slot.
    ///
    /// ⚠ It takes no `--task`, and that is the point rather than an omission:
    /// admission is a projection of the log (ADR-0004), so naming a task here
    /// would be a second answer to a question the board already answers.
    Fleet(Box<Run>),
    /// The reader, over the durable log.
    Watch { theme: Theme },
    /// The model-confirmation check on its own.
    Check {
        model: Option<String>,
        base_url: Option<String>,
        fingerprint: Option<String>,
    },
    /// 🚨 The breaker's report: one real token out of the server, and F374's
    /// update rule over this log's own population.
    ///
    /// ⚠ It takes no `--fingerprint`. The fingerprint answers *which model*,
    /// and this asks *can this server produce a token at all* — a wedged server
    /// is serving the right model and answering nothing (F539).
    Breaker {
        model: Option<String>,
        base_url: Option<String>,
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
    /// 🚨 One frame of the battlefield, as a sixel, straight to stdout.
    ///
    /// ADR-0012's flagship, made visible before it is wired to the log. It is
    /// deliberately a **still** and deliberately not a screen: what it answers is
    /// *does this terminal draw our composite*, which is the question the whole
    /// Console milestone rests on and the one the W5 spike could only answer for
    /// the spike's own encoder.
    ///
    /// ⚠ It takes a sprite directory rather than knowing one. The corpus lives
    /// outside this repository and a path compiled in here would be a path that
    /// is wrong on every other machine.
    Paint {
        sprites: Option<String>,
        /// Sprite height in pixels. 75-120 is the band David judged reads as
        /// C&C at arm's length (F143).
        px: u32,
        /// Field size in pixels.
        size: (u32, u32),
    },
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
    /// 🚨 **The slot's tool ceiling**, capping every head this run's attempts
    /// use. `None` is *the slot allows what the role asks for*, which is what
    /// every run did before the flag existed.
    ///
    /// It is a tier and never a list of tool names, because an argument check
    /// binds only the tool that has an argument (W7, four times) — the class is
    /// the thing that can be denied.
    pub ceiling: Option<Tier>,
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
  abcc fleet [options]                attempts until the board is quiet, one slot
  abcc watch [--theme command|classic]  the reader, over the log alone
  abcc check [--model M]              ask the server which model it is holding
  abcc breaker [--model M]            one real token, and what the log says about retrying
  abcc review <change> <minutes> [--by W] [--boundary]
  abcc accept <task> [--note N]       the work is good; you take responsibility
  abcc reject <task> [--note N]       stop the task
  abcc paint [--sprites DIR]          one frame of the battlefield, as a sixel

Everywhere:
  --repo <path>     the checkout to work on (default: the working directory)
  --home <path>     where the log and worktrees live (default: outside the repo)

run and fleet:
  --model <name>    the model to ask for (default: $ABCC_MODEL)
  --url <base>      the server (default: $ABCC_MODEL_BASE_URL)
  --fingerprint <s> a substring that must appear in the served model id
  --unit <n>        the slot to run in (default: 0) -- run only, and so is --task
  --rounds <n>      tool rounds before the phase gives up
  --idle-gap <s>    seconds of silence on the stream that count as a hang
  --ceiling <tier>  cap every role in the slot: no-tools | read | write | exec

paint:
  --sprites <dir>   the sprite corpus (default: $ABCC_SPRITES)
  --px <n>          sprite height in pixels (default: 150)
  --size <WxH>      the field, in pixels (default: 640x360)

  Run it in a terminal with sixel. Windows Terminal has had it since 1.22;
  tmux and Zellij strip it, and it does not survive most SSH multiplexers.

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
        "run" => Command::Run(Box::new(run_args(&mut args, true)?)),
        "fleet" => Command::Fleet(Box::new(run_args(&mut args, false)?)),
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
        "paint" => paint_args(&mut args)?,
        "breaker" => {
            let model = take_flag(&mut args, "--model")?;
            let base_url = take_flag(&mut args, "--url")?;
            no_positionals(&args, "breaker")?;
            Command::Breaker { model, base_url }
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

/// The flags `run` and `fleet` share, and the two `run` has to itself.
///
/// ⚠ `--task` and `--unit` are parsed only for `run`. Leaving them unparsed for
/// `fleet` makes `abcc fleet --task t42` an unknown-flag error rather than a flag
/// that is silently ignored — admission is a projection of the log, so naming a
/// task there would be a second answer to a question the board already answers,
/// and `--unit` names a slot in a fleet that has one (ADR-0020).
fn run_args(args: &mut Vec<String>, per_task: bool) -> Result<Run, CliError> {
    let verb = if per_task { "run" } else { "fleet" };
    let run = Run {
        task: if per_task {
            take_flag(args, "--task")?
                .map(|t| task_ref(&t))
                .transpose()?
        } else {
            None
        },
        model: take_flag(args, "--model")?,
        base_url: take_flag(args, "--url")?,
        fingerprint: take_flag(args, "--fingerprint")?,
        unit: if per_task {
            take_flag(args, "--unit")?
                .map(|u| number(&u, "--unit"))
                .transpose()?
        } else {
            None
        },
        rounds: take_flag(args, "--rounds")?
            .map(|r| number(&r, "--rounds"))
            .transpose()?,
        idle_gap: take_flag(args, "--idle-gap")?
            .map(|g| seconds(&g, "--idle-gap"))
            .transpose()?,
        ceiling: take_flag(args, "--ceiling")?
            .map(|c| ceiling(&c))
            .transpose()?,
    };
    no_positionals(args, verb)?;
    Ok(run)
}

/// A ceiling, by the name the log and the refusals print.
///
/// ⚠ The error lists all four rather than only saying no: the operator who typed
/// `--ceiling readonly` needs the spelling, not a verdict.
fn ceiling(given: &str) -> Result<Tier, CliError> {
    given
        .parse()
        .map_err(|e: abcc_engine::UnknownTier| CliError::Usage(e.to_string()))
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

/// Everything `paint` takes. Its own function because the parse is three flags
/// with three different shapes and `parse` is already at clippy's line ceiling —
/// a subcommand's arguments live beside the subcommand, not inside a match arm
/// that keeps growing.
fn paint_args(args: &mut Vec<String>) -> Result<Command, CliError> {
    let sprites = take_flag(args, "--sprites")?;
    let px = match take_flag(args, "--px")? {
        Some(v) => v
            .parse()
            .map_err(|_| CliError::Usage(format!("--px takes a number of pixels, not {v:?}")))?,
        // 🚨 **150, and the number is a judgement rather than a measurement.**
        // The W5 spike put the C&C band at 75-120 (F143) and David first chose
        // 100 from it — while looking at the `cto` poses, which are 161 px
        // across at that height. Those turned out to be unusable art (F565) and
        // the four that replaced them are 67 px across at the same setting, so
        // the same number drew a huddle in an empty field. Re-judged at the
        // corpus that is actually drawn: 200 too big, 100 too small, **150**
        // (2026-08-31). ▶ It moves again when the art does.
        None => 150,
    };
    let size = match take_flag(args, "--size")? {
        Some(v) => {
            let bad = || CliError::Usage(format!("--size takes WxH, like 640x360, not {v:?}"));
            let (w, h) = v.split_once('x').ok_or_else(bad)?;
            (
                w.parse::<u32>().map_err(|_| bad())?,
                h.parse::<u32>().map_err(|_| bad())?,
            )
        }
        None => (640, 360),
    };
    no_positionals(args, "paint")?;
    Ok(Command::Paint { sprites, px, size })
}
