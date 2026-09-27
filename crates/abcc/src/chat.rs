//! `abcc chat` — work on one task together: the operator talks, the model edits,
//! in a worktree of this repository (PLAN-TOOL Phase C1).
//!
//! The terminal half. The lifecycle — slot, worktree, attempt, gate, Judge — is
//! [`Driver::chat`]'s, so a chat is an ordinary task on the board and `board`,
//! `diff`, `land` and `replay` all work on it.
//!
//! 1. **The task.** `abcc chat "<ask>"` puts a new one on the board, `--task t42`
//!    picks one up (continuing from its latest checkpoint), and a bare
//!    `abcc chat` asks for the ask at the prompt.
//! 2. **The conversation.** Replies stream as they arrive, tool calls print one
//!    line each, Esc or Ctrl-C stops the turn in flight (the next line is the
//!    redirect). `/diff` shows the change so far.
//! 3. **`/done`.** The gate runs. Green: the diff is shown, and `land` on a
//!    keypress makes the commit. The seconds between the diff appearing and
//!    that keypress are recorded as an observed review (C2), never typed.
//!    Refused: the refusal goes into the conversation and it carries on.
//! 4. **`/quit`** keeps the work: the task holds, and `--task` resumes it.
//!
//! Eviction (B3) is always on here: a conversation outgrows the window in a way
//! a single batch phase rarely does.

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use abcc_core::attempt::Cause;
use abcc_core::event::{Control, Event};
use abcc_core::redact::Scrubbed;
use abcc_core::run::Mode;
use abcc_core::seq::{AttemptId, Seq, TaskId, UnitId};
use abcc_core::task::TaskState;
use abcc_drive::{Driver, Operator, Said};
use abcc_engine::control::ControlHandle;
use abcc_engine::openai::OpenAiCompat;
use abcc_engine::provider::{Body, Delta};
use abcc_engine::tools::Tier;
use abcc_engine::turn::PhaseEnded;
use abcc_store::Store;
use crossterm::event::{self as keys, KeyCode, KeyEventKind, KeyModifiers};

use crate::line_editor::{LineEditor, ReadOutcome};
use crate::run::{Confirmed, confirm_model, limits_for, report, report_boot};
use crate::{AppError, Invocation, cli, land, ops};

const HELP: &str = "\
  /diff   what this chat has changed so far
  /done   run the checks; if they pass, see the diff and land it
  /quit   stop and keep the work (abcc chat --task <id> picks it up)
  Esc or Ctrl-C while it works stops that turn; then say what to do instead";

/// Run `abcc chat`.
///
/// # Errors
///
/// [`AppError`] — no repository, an unconfirmed model, a task that cannot be
/// chatted on, or anything the driver could not do.
pub fn chat(
    invocation: &Invocation,
    args: &cli::Chat,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let ground = ops::ground(invocation)?;
    let mut store = Store::open(&ground.home.log())?;
    report_boot(&store.boot()?, out)?;
    store.append(Event::RunStarted {
        mode: Mode::SinglePlayer,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        pid: std::process::id(),
    })?;

    let mut terminal = Terminal::new(LineEditor::new(Some(
        ground.home.root().join("chat_history"),
    )));
    let task = match (&args.run.task, &args.prompt) {
        (Some(named), _) => TaskId::at(Seq::new(named.0)),
        (None, Some(ask)) => ops::new_task(&mut store, &ground, ask, args.title.as_deref())?.0,
        (None, None) => {
            eprintln!("what should we work on? (one line; /quit to leave)");
            let Said::Ask(ask) = terminal.next() else {
                return Ok(());
            };
            ops::new_task(&mut store, &ground, &ask, args.title.as_deref())?.0
        }
    };
    let row = ops::row(&store, task)?;
    if !matches!(row.state, TaskState::Queued | TaskState::Holding { .. }) {
        return Err(AppError::Refused(format!(
            "{task} is not waiting to be worked on (`abcc board` says where it is)"
        )));
    }

    let run = cli::Run {
        evict: true,
        ..args.run.clone()
    };
    let Confirmed {
        model,
        evict_window,
    } = confirm_model(&mut store, &run, out)?;
    let base = ops::base_url(run.base_url.as_deref());
    let mut provider = OpenAiCompat::new(&base)?;
    if let Some(key) = ops::api_key() {
        provider = provider.with_api_key(key);
    }
    let mut limits = limits_for(&run);
    limits.evict_window = evict_window;
    let ceiling = run.ceiling.unwrap_or(Tier::Exec);

    writeln!(out, "\nchat on {task}: {}\n{HELP}\n", row.title)?;
    out.flush()?;

    let stream = Stream::default();
    let deltas = |d: &Delta| stream.take(d);
    let mut cause = continuing(&store, task)?;
    let mut body = Body::new();
    let landed = loop {
        let mut driver = Driver::new(
            &mut store,
            &ground.repo,
            &provider,
            model.clone(),
            ground.home.worktrees(),
        )
        .limits(limits)
        .ceiling(ceiling)
        .secrets(ops::secrets());
        let landed = driver.chat(task, UnitId(0), cause, &mut body, &mut terminal, &deltas)?;
        report(&landed, out)?;
        // A refused tree comes back to the operator, and the operator is here.
        if matches!(landed.state, TaskState::AwaitingOrders { .. })
            && landed.gate.as_ref().is_some_and(|g| !g.headline.is_pass())
            && terminal.confirm("the checks refused it. keep going here? [Y/n] ", true)
        {
            driver.keep_going(task, "keep going (abcc chat)")?;
            cause = Cause::Retry { of: landed.attempt };
            continue;
        }
        break landed;
    };

    match landed.state {
        TaskState::Accomplished { .. } => {
            review_and_land(invocation, &mut store, task, &mut terminal, out)
        }
        TaskState::Holding { .. } => {
            writeln!(out, "\nkept. `abcc chat --task {task}` picks it up again.")?;
            Ok(())
        }
        _ => {
            writeln!(out, "\n`abcc board` says what {task} is waiting for.")?;
            Ok(())
        }
    }
}

/// Show the diff, ask, and land on a yes. The seconds between the two are the
/// operator's review, observed rather than typed (C2).
fn review_and_land(
    invocation: &Invocation,
    store: &mut Store,
    task: TaskId,
    terminal: &mut Terminal,
    out: &mut impl Write,
) -> Result<(), AppError> {
    let named = cli::TaskRef(task.born().get());
    writeln!(out)?;
    land::diff(invocation, named, out)?;
    out.flush()?;
    let shown = Instant::now();
    if !terminal.confirm("land this change on your branch? [y/N] ", false) {
        writeln!(out, "not landed. `abcc land {task}` when you are ready.")?;
        return Ok(());
    }
    let seconds = u32::try_from(shown.elapsed().as_secs()).unwrap_or(u32::MAX);
    land::land(invocation, named, out)?;
    store.append(Event::ReviewRecorded {
        change: task.to_string(),
        seconds,
        by: "observed".to_owned(),
        crossed_boundary: false,
    })?;
    writeln!(out, "review: {seconds} s, observed by abcc chat")?;
    Ok(())
}

/// A task with an attempt behind it carries on from its latest checkpoint; a
/// fresh one starts from the operator's checkout.
fn continuing(store: &Store, task: TaskId) -> Result<Cause, AppError> {
    let last = store
        .task_history(task)?
        .into_iter()
        .rev()
        .find(|l| matches!(l.event, Event::AttemptStarted { .. }));
    Ok(match last {
        Some(logged) => Cause::Retry {
            of: AttemptId::at(logged.seq),
        },
        None => Cause::Fresh,
    })
}

// ---------------------------------------------------------------------------
// the terminal
// ---------------------------------------------------------------------------

/// The operator's side of the conversation.
struct Terminal {
    editor: LineEditor,
    interactive: bool,
    watcher: Option<Watcher>,
}

impl Terminal {
    fn new(editor: LineEditor) -> Terminal {
        Terminal {
            editor,
            interactive: io::stdin().is_terminal() && io::stderr().is_terminal(),
            watcher: None,
        }
    }

    /// A yes/no question. End of input, or an empty answer, is `default`.
    fn confirm(&mut self, question: &str, default: bool) -> bool {
        let width = u16::try_from(question.chars().count()).unwrap_or(u16::MAX);
        match self.editor.read_line(question, width) {
            Ok(ReadOutcome::Line(line)) => match line.trim().to_ascii_lowercase().as_str() {
                "" => default,
                answer => answer.starts_with('y'),
            },
            _ => default,
        }
    }
}

impl Operator for Terminal {
    fn next(&mut self) -> Said {
        loop {
            match self.editor.read_line("you> ", 5) {
                Ok(ReadOutcome::Line(line)) => {
                    let said = line.trim();
                    if said.is_empty() {
                        continue;
                    }
                    self.editor.push_history(said);
                    return match said {
                        "/done" => Said::Done,
                        "/diff" => Said::Diff,
                        "/quit" | "/exit" => Said::Quit,
                        "/help" => {
                            eprintln!("{HELP}");
                            continue;
                        }
                        other if other.starts_with('/') && !other.contains(char::is_whitespace) => {
                            eprintln!("{other} is not a command here.\n{HELP}");
                            continue;
                        }
                        _ => Said::Ask(line.trim_end().to_owned()),
                    };
                }
                Ok(ReadOutcome::Interrupted) => {}
                Ok(ReadOutcome::Eof) | Err(_) => return Said::Quit,
            }
        }
    }

    fn turn_starting(&mut self, interrupt: ControlHandle) {
        if self.interactive {
            self.watcher = Some(Watcher::start(interrupt));
        }
    }

    fn event(&mut self, event: &Event) {
        let mut out = io::stdout().lock();
        let _ = match event {
            Event::ToolCallStarted { tool, .. } => write!(out, "{}  ▸ {tool}", clear()),
            Event::ToolCallEnded {
                arguments,
                exit,
                unmeasured,
                output,
                ..
            } => {
                let args = arguments
                    .as_ref()
                    .map_or(String::new(), |a| short(a.as_str(), 90));
                let how = match (unmeasured, exit) {
                    (Some(why), _) => format!("refused: {}", short(&why.to_string(), 80)),
                    (None, Some(0) | None) => {
                        let n = output.as_ref().map_or(0, Scrubbed::len);
                        format!("ok, {n} chars")
                    }
                    (None, Some(code)) => format!("exit {code}"),
                };
                write!(out, " {args}  — {how}{}", eol())
            }
            Event::PhaseNudged { .. } => write!(
                out,
                "{}  (abcc nudged the model to answer){}",
                clear(),
                eol()
            ),
            Event::PromptCut { .. } => {
                write!(out, "{}  ⚠ the server cut the prompt{}", clear(), eol())
            }
            Event::Note { text } if text.contains("evict:") => {
                write!(
                    out,
                    "{}  (older tool output stubbed to make room){}",
                    clear(),
                    eol()
                )
            }
            _ => Ok(()),
        };
        let _ = out.flush();
    }

    fn turn_ended(&mut self, ended: &PhaseEnded) {
        if let Some(watcher) = self.watcher.take() {
            watcher.finish();
        }
        let mut out = io::stdout().lock();
        let _ = match ended {
            PhaseEnded::Answered { .. } => write!(out, "{}{}", clear(), eol()),
            PhaseEnded::Stopped { .. } => write!(
                out,
                "{}  (stopped — say what to do instead){}",
                clear(),
                eol()
            ),
            PhaseEnded::Unmeasured { why, .. } => {
                write!(
                    out,
                    "{}  (the turn ended without an answer: {why}){}",
                    clear(),
                    eol()
                )
            }
        };
        let _ = out.flush();
    }

    fn diff(&mut self, patch: &str) {
        if patch.trim().is_empty() {
            eprintln!("(nothing changed yet)");
        } else {
            println!("{patch}");
        }
    }

    fn note(&mut self, text: &str) {
        eprintln!("abcc: {text}");
    }
}

/// Watches the keyboard while a turn runs, so Esc or Ctrl-C can stop it.
///
/// ⚠ Raw mode is what turns Ctrl-C into a key rather than the signal that would
/// end the process — and it is on only while a turn runs, off before the line
/// editor reads again (it uses raw mode itself). On Windows it touches input
/// only; elsewhere output loses its carriage returns, which is why everything
/// printed during a turn ends in [`eol`].
struct Watcher {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    fn start(interrupt: ControlHandle) -> Watcher {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            if crossterm::terminal::enable_raw_mode().is_err() {
                return;
            }
            while !flag.load(Ordering::Acquire) {
                if !matches!(keys::poll(Duration::from_millis(100)), Ok(true)) {
                    continue;
                }
                if let Ok(keys::Event::Key(key)) = keys::read() {
                    let ctrl_c = key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL);
                    if key.kind == KeyEventKind::Press && (ctrl_c || key.code == KeyCode::Esc) {
                        let _ = interrupt.request(Control::Halt);
                    }
                }
            }
            let _ = crossterm::terminal::disable_raw_mode();
        });
        Watcher {
            stop,
            thread: Some(thread),
        }
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The reply as it streams, and a line saying the model is thinking while it
/// is only reasoning.
#[derive(Default)]
struct Stream(Mutex<Streaming>);

#[derive(Default)]
struct Streaming {
    reasoning: usize,
    shown: usize,
    status: bool,
}

impl Stream {
    fn take(&self, delta: &Delta) {
        let Ok(mut s) = self.0.lock() else { return };
        let mut out = io::stdout().lock();
        match delta {
            Delta::Opened { .. } => {
                s.reasoning = 0;
                s.shown = 0;
            }
            Delta::Reasoning(text) => {
                s.reasoning += text.chars().count();
                if s.reasoning >= s.shown + 400 {
                    s.shown = s.reasoning;
                    s.status = true;
                    let _ = write!(out, "\r  · thinking ({} chars)", s.reasoning);
                }
            }
            Delta::Text(text) => {
                if s.status {
                    s.status = false;
                    let _ = write!(out, "{}", clear());
                }
                let _ = write!(out, "{}", text.replace('\n', eol()));
            }
            Delta::ToolCallOpened { .. } if s.status => {
                s.status = false;
                let _ = write!(out, "{}", clear());
            }
            _ => {}
        }
        let _ = out.flush();
    }
}

/// Back to the start of the line and clear it.
const fn clear() -> &'static str {
    "\r\x1b[K"
}

/// A line ending that survives raw mode.
const fn eol() -> &'static str {
    "\r\n"
}

/// At most `max` characters of `text`, on one line.
fn short(text: &str, max: usize) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        return one;
    }
    let cut: String = one.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}
