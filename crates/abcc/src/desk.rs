//! The console edge: the operator's verbs, on their way to a running attempt.
//!
//! ADR-0006 states the ordering and calls it load-bearing: **the console writes
//! `ControlRequested` to the log first and pokes the channel second.** The poke
//! is a latency optimisation and never the truth — a verb that reaches the log
//! and not the channel is still honoured by boot replay, and one that reaches the
//! channel and not the log never happened.
//!
//! # 🚨 Why this holds a second connection, and what it may write with it
//!
//! [`abcc_drive::Driver`] borrows the [`Store`] mutably for the whole of an
//! attempt, so while a run is in flight there is exactly one thing that can write
//! through that value and it is the driver. Honouring the ordering above
//! therefore needs a second connection, and this is the only place in the binary
//! that opens one for writing.
//!
//! **It may append `ControlRequested` and nothing else.** That is not a
//! weakening of `abcc-store`'s single-writer rule, because that rule is about the
//! *transition* writer: `Store::apply` remains the one path that moves a task and
//! updates the projection in one transaction, and `ControlRequested` moves
//! nothing — it falls to `project`'s catch-all and inserts one `event` row. What
//! makes two connections safe here is written down rather than assumed: `seq` is
//! `INTEGER PRIMARY KEY AUTOINCREMENT` so the position is allocated by SQLite
//! inside the transaction, and `busy_timeout` is 5 s on every connection the
//! store opens.
//!
//! ⚠ The desk thread blocks on stdin and is **never joined**: when a run ends,
//! `main` returns and the process exits under it. Its connection is dropped
//! abruptly, which is a crash as far as SQLite is concerned, and surviving that
//! is the property `abcc-store`'s durability tests exist to prove.

use std::io::{self, BufRead, Write};
use std::path::Path;

use abcc_core::event::{Control, Event};
use abcc_core::seq::TaskId;
use abcc_engine::control::ControlHandle;
use abcc_store::{Store, StoreError};

/// The verbs, as an operator types them, for the prompt and for `--help`.
pub const VERBS: &str = "pause | halt | kill | redirect <prompt> | resume";

/// One task's control channel, with the log in front of it.
pub struct Desk {
    log: Store,
    handle: ControlHandle,
    task: TaskId,
}

impl Desk {
    /// Open the desk's own connection.
    ///
    /// Call this **before** the driver starts: opening a `Store` rebuilds the
    /// projection, which is a write, and doing that underneath a running attempt
    /// would be a second writer doing something much larger than one row.
    ///
    /// # Errors
    ///
    /// [`StoreError`] if the log will not open.
    pub fn open(log: &Path, handle: ControlHandle, task: TaskId) -> Result<Desk, StoreError> {
        Ok(Desk {
            log: Store::open(log)?,
            handle,
            task,
        })
    }

    /// Record the verb, then poke the worker. In that order.
    ///
    /// # Errors
    ///
    /// [`StoreError`] if the log would not take the row — in which case the
    /// worker is **not** poked, because a verb that is not on the log did not
    /// happen.
    pub fn request(&mut self, control: Control) -> Result<Delivered, StoreError> {
        self.log.append(Event::ControlRequested {
            task: self.task,
            control: control.clone(),
        })?;
        // `Gone` is the ordinary race, not a fault: the attempt ended between the
        // operator's keystroke and this line. The durable row above is what makes
        // it recoverable, so it is reported and not raised.
        Ok(match self.handle.request(control) {
            Ok(()) => Delivered::ToTheWorker,
            Err(_) => Delivered::LoggedOnly,
        })
    }

    /// Read verbs from stdin until it ends.
    ///
    /// Runs on its own thread for the length of a run, and returns when stdin
    /// closes — which on an interactive terminal is never, so the process exit is
    /// what ends it.
    pub fn serve(mut self, mut out: impl Write) {
        let stdin = io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            match stdin.lock().read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let typed = line.trim();
            if typed.is_empty() {
                continue;
            }
            let Some(control) = parse_verb(typed) else {
                let _ = writeln!(out, "  ? {typed:?} is not one of: {VERBS}");
                continue;
            };
            match self.request(control) {
                Ok(Delivered::ToTheWorker) => {
                    let _ = writeln!(out, "  > logged and sent");
                }
                Ok(Delivered::LoggedOnly) => {
                    let _ = writeln!(
                        out,
                        "  > logged; the attempt had already ended, so nothing received it"
                    );
                }
                Err(e) => {
                    let _ = writeln!(out, "  ! not logged, so not sent: {e}");
                }
            }
            let _ = out.flush();
        }
    }
}

/// How far a verb got. Both are normal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivered {
    /// On the log and on the channel.
    ToTheWorker,
    /// On the log; the worker was already gone. Boot replay is the backstop.
    LoggedOnly,
}

/// One typed line to one verb.
///
/// Pure, and the half of the desk worth testing. `redirect` takes the rest of the
/// line verbatim, including its spacing, because it is a prompt.
///
/// ⚠ **The four verbs that take nothing accept nothing.** `pause now` is not a
/// pause with a word dropped — an operator who typed something extra meant
/// something by it, and a control channel that silently discards half a line is
/// the same class of defect as a flag that quietly defaults.
#[must_use]
pub fn parse_verb(line: &str) -> Option<Control> {
    let line = line.trim();
    let (head, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let alone = rest.trim().is_empty();
    match head.to_lowercase().as_str() {
        "pause" | "p" if alone => Some(Control::Pause),
        "halt" | "h" if alone => Some(Control::Halt),
        "kill" | "k" if alone => Some(Control::Kill),
        "resume" if alone => Some(Control::Resume),
        // A redirect with no prompt is not a redirect: the whole verb is the new
        // question, and forking an attempt onto an empty one would spend a slot
        // to ask nothing.
        "redirect" | "r" => {
            let prompt = rest.trim();
            (!prompt.is_empty()).then(|| Control::Redirect {
                prompt: prompt.to_owned(),
            })
        }
        _ => None,
    }
}
