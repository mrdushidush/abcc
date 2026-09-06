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
//!
//! # 🚨 Two desks, because a sortie moves and a run does not
//!
//! [`Desk`] is one task's, and it is the right shape for `abcc run`: one attempt,
//! one channel, and the task named once when the desk is opened. [`FleetDesk`] is
//! the same edge across a sortie, and it differs in exactly one thing — **the
//! operator names the task on every line.**
//!
//! That is the answer to the trap `abcc fleet` used to state rather than fake: a
//! desk bound to whichever task happened to be in flight would send a verb to a
//! task the operator was not looking at. The window is real and it is the length
//! of a keystroke — the operator reads `t42 flying`, types `kill`, and t42 lands
//! while their hand is moving. Naming the task turns that from a mis-delivery
//! into a refusal, which is the trade: **a verb that misses is recoverable and a
//! verb that lands somewhere else is not.**
//!
//! ⚠ The name is checked and the poke sent under one lock, inside
//! [`InFlight::deliver`] — checking first and poking second is the same race in
//! slow motion. And the row is written against the task the operator **named**,
//! matched or not, because a desk that logged only what it delivered would lose
//! the verb that arrived one moment too late.
//!
//! ⚠ [`FleetDesk`] appends `Note` as well as `ControlRequested`, for the one
//! order that has no task. The two-connection argument above is unchanged: a
//! `Note` also moves nothing, falls to `project`'s catch-all, and inserts one
//! `event` row.

use std::io::{self, BufRead, Write};
use std::path::Path;

use abcc_core::event::{Control, Event};
use abcc_core::seq::{Seq, TaskId};
use abcc_engine::control::{ControlHandle, Delivery, InFlight};
use abcc_fleet::StandDown;
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
            let caveat = caveat(&control);
            match self.request(control) {
                Ok(Delivered::ToTheWorker) => {
                    let _ = writeln!(out, "  > logged and sent");
                }
                // ⚠ A one-task desk holds the channel it was opened with and
                // cannot produce `NotInFlight`. The arm is here because the enum
                // is shared with `FleetDesk`, and it says the same true thing.
                Ok(Delivered::LoggedOnly | Delivered::NotInFlight { .. }) => {
                    let _ = writeln!(
                        out,
                        "  > logged; the attempt had already ended, so nothing received it"
                    );
                }
                Err(e) => {
                    let _ = writeln!(out, "  ! not logged, so not sent: {e}");
                }
            }
            if let Some(caveat) = caveat {
                let _ = writeln!(out, "{caveat}");
            }
            let _ = out.flush();
        }
    }
}

/// How far a verb got. All three are normal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivered {
    /// On the log and on the channel.
    ToTheWorker,
    /// On the log; the worker was already gone. Boot replay is the backstop.
    LoggedOnly,
    /// 🚨 On the log, against the task the operator **named** — and sent nowhere,
    /// because the slot is not on that task.
    ///
    /// Only a [`FleetDesk`] can produce it, and it is the whole reason that type
    /// exists: the alternative to refusing here is re-aiming the verb at whatever
    /// is flying instead, which is a `kill` arriving at a task nobody looked at.
    /// `flying` is what the slot is actually on, so the operator is told rather
    /// than left to guess.
    NotInFlight { flying: Option<TaskId> },
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
    control(head, rest.trim())
}

/// One head word and everything after the address to one verb.
///
/// 🚨 The table lives here once. The two desks differ only in what sits between
/// the verb and its prompt — nothing at a run, a task id at a fleet — and two
/// copies of this `match` would be two places that could disagree about what `k`
/// means.
///
/// `prompt` arrives already trimmed at both ends, and empty when the operator
/// typed nothing after the verb.
fn control(head: &str, prompt: &str) -> Option<Control> {
    let alone = prompt.is_empty();
    match head.to_lowercase().as_str() {
        "pause" | "p" if alone => Some(Control::Pause),
        "halt" | "h" if alone => Some(Control::Halt),
        "kill" | "k" if alone => Some(Control::Kill),
        "resume" if alone => Some(Control::Resume),
        // A redirect with no prompt is not a redirect: the whole verb is the new
        // question, and forking an attempt onto an empty one would spend a slot
        // to ask nothing.
        "redirect" | "r" => (!alone).then(|| Control::Redirect {
            prompt: prompt.to_owned(),
        }),
        _ => None,
    }
}

/// 🚨 **What a verb does not do yet, said at the moment it is used.**
///
/// ADR-0012 §4 names the risk in one sentence — *eight verbs that half-work are
/// worse than three that work* — and three of these five have a mechanism all the
/// way down while two do not. The honest place to say so is the line the operator
/// gets back: a verb that is quietly inert teaches them to trust a control that
/// is not there, and they find out on the run where it mattered.
///
/// ⚠ Both are still parsed, still written to the log, and still spelled the same
/// at both desks. The gap is in the mechanism and not in the word, and removing
/// the word would only move the surprise to `abcc run`.
///
/// * **`redirect`** reaches [`abcc_engine::control::Keep::AndFork`] and the
///   landing turns it into `NextAction::Attempt { Cause::Edit }` — a
///   recommendation nothing acts on. A sortie admits `Queued` tasks and a redirect
///   leaves this one `Holding`, so no attempt is ever forked from the prompt.
/// * **`resume`** is not a stop, so the control point ignores it; and
///   `Command::Resume`, the lifecycle's way out of `Holding` *back onto a slot*,
///   has no caller in the binary at all. ⚠ `abcc take` is not that
///   edge and does not close this gap: it moves the task to `Commandeered` and
///   hands the tree to a person, which is the operator taking the work off the
///   fleet rather than the fleet picking it back up.
#[must_use]
pub fn caveat(control: &Control) -> Option<&'static str> {
    match control {
        Control::Pause | Control::Halt | Control::Kill => None,
        Control::Redirect { .. } => Some(
            "    \u{26a0} it stops the attempt and records the prompt; \
             no attempt is forked from it yet",
        ),
        Control::Resume => Some(
            "    \u{26a0} nothing acts on resume yet \u{2014} `abcc take <task>` puts you in \
             the tree, and `abcc accept <task>` / `abcc reject <task>` end it",
        ),
    }
}

/// Whether a head word is one of the five that address a task.
fn addresses_a_task(head: &str) -> bool {
    matches!(
        head.to_lowercase().as_str(),
        "pause" | "p" | "halt" | "h" | "kill" | "k" | "resume" | "redirect" | "r"
    )
}

// ---------------------------------------------------------------------------
// The fleet's desk
// ---------------------------------------------------------------------------

/// The orders a fleet desk takes, as an operator types them.
pub const ORDERS: &str = "pause <task> | halt <task> | kill <task> | \
                          redirect <task> <prompt> | resume <task> | ground | slot";

/// What the log says when a sortie is stood down.
///
/// ⚠ It is a `Note` and not a `ControlRequested`, because a stand-down has no
/// task and that event's whole point is that it does. ⚠ And unlike a control
/// verb, **this row is a record and not a backstop**: nothing replays it, because
/// a stand-down is about this process's admission loop and the next process is a
/// sortie somebody started on purpose. It is written first anyway — a sortie that
/// stopped for a reason the log does not carry is a sortie nobody can explain
/// afterwards.
pub const GROUND_NOTE: &str =
    "operator: ground — the sortie admits nothing more; what is in flight is flown out";

/// One order, addressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Order {
    /// One of the five verbs, for a named task.
    ToTask { task: TaskId, control: Control },
    /// 🚨 The one order whose scope is the sortie rather than a task.
    Ground,
    /// What is the slot on? Reads, changes nothing, and writes nothing.
    Slot,
}

/// Why a typed line is not an order.
///
/// 🚨 Five variants rather than one, because they are five different things for
/// an operator to do next and a desk that answers all of them with *"not one of
/// the verbs"* makes the operator guess which. [`Misread::Unaddressed`] is the
/// one that matters: it is not a typo, it is the mis-delivery being refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Misread {
    /// Not one of the words at all.
    NotAnOrder(String),
    /// A task verb with no task after it.
    Unaddressed(String),
    /// Something in the task's place that is not a task id.
    NotATask(String),
    /// `redirect t42` with nothing to redirect it to.
    EmptyRedirect,
    /// A word that takes nothing, given something.
    Trailing { order: String, extra: String },
}

impl std::fmt::Display for Misread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Misread::NotAnOrder(typed) => write!(f, "{typed:?} is not one of: {ORDERS}"),
            Misread::Unaddressed(head) => write!(
                f,
                "{head} needs a task. The slot moves between tasks here, so a verb with no \
                 name would go to whatever is flying when you press enter — which may not be \
                 what you were looking at. Type `{head} t42`, or `slot` to see."
            ),
            Misread::NotATask(typed) => {
                write!(f, "{typed:?} is not a task id — they look like `t42`")
            }
            Misread::EmptyRedirect => write!(
                f,
                "a redirect is its prompt: `redirect t42 look in src/lib.rs instead`"
            ),
            Misread::Trailing { order, extra } => write!(
                f,
                "{order} takes nothing, and {extra:?} was on the line too"
            ),
        }
    }
}

/// One typed line to one order.
///
/// Pure, and the half of the fleet desk worth testing. ⚠ **The task is required
/// on every one of the five verbs**, and that is the whole design rather than an
/// ergonomic slip — see this module's docs.
///
/// # Errors
///
/// [`Misread`], one variant per thing the operator can do about it.
pub fn parse_order(line: &str) -> Result<Order, Misread> {
    let line = line.trim();
    let (head, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let rest = rest.trim();

    if addresses_a_task(head) {
        let (named, remainder) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if named.is_empty() {
            return Err(Misread::Unaddressed(head.to_lowercase()));
        }
        let task = task_ref(named).ok_or_else(|| Misread::NotATask(named.to_owned()))?;
        let remainder = remainder.trim();
        return match control(head, remainder) {
            Some(control) => Ok(Order::ToTask { task, control }),
            // The table refused it, and there are exactly two ways it can: a
            // redirect with no prompt, or one of the four bare verbs with
            // something after the task. Both are said as themselves.
            None if remainder.is_empty() => Err(Misread::EmptyRedirect),
            None => Err(Misread::Trailing {
                order: head.to_lowercase(),
                extra: remainder.to_owned(),
            }),
        };
    }

    match head.to_lowercase().as_str() {
        word @ ("ground" | "slot") if !rest.is_empty() => Err(Misread::Trailing {
            order: word.to_owned(),
            extra: rest.to_owned(),
        }),
        "ground" => Ok(Order::Ground),
        "slot" => Ok(Order::Slot),
        _ => Err(Misread::NotAnOrder(line.to_owned())),
    }
}

/// `t42` or `42`, and nothing else. The same shape [`crate::cli`] accepts on the
/// command line, because an operator who has typed `abcc accept t42` has already
/// learned one spelling and should not have to learn a second.
fn task_ref(raw: &str) -> Option<TaskId> {
    raw.strip_prefix('t')
        .unwrap_or(raw)
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .map(|n| TaskId::at(Seq::new(n)))
}

/// The console edge for a sortie: the log, the slot, and the stand-down.
///
/// It holds no task, deliberately — the operator names one on every line. What
/// it holds instead is [`InFlight`], which is *the slot's* answer to which task a
/// verb would reach, republished by the fleet on every attempt.
pub struct FleetDesk {
    log: Store,
    in_flight: InFlight,
    stand_down: StandDown,
}

impl FleetDesk {
    /// Open the desk's own connection.
    ///
    /// Same rule as [`Desk::open`]: **before** the sortie starts, because opening
    /// a `Store` rebuilds the projection and that is a write.
    ///
    /// # Errors
    ///
    /// [`StoreError`] if the log will not open.
    pub fn open(
        log: &Path,
        in_flight: InFlight,
        stand_down: StandDown,
    ) -> Result<FleetDesk, StoreError> {
        Ok(FleetDesk {
            log: Store::open(log)?,
            in_flight,
            stand_down,
        })
    }

    /// Record the verb against the task the operator named, then send it to that
    /// task and to no other.
    ///
    /// # Errors
    ///
    /// [`StoreError`] if the log would not take the row — in which case nothing
    /// is sent, because a verb that is not on the log did not happen.
    pub fn request(&mut self, task: TaskId, control: Control) -> Result<Delivered, StoreError> {
        self.log.append(Event::ControlRequested {
            task,
            control: control.clone(),
        })?;
        Ok(match self.in_flight.deliver(task, control) {
            Delivery::Sent => Delivered::ToTheWorker,
            Delivery::Gone => Delivered::LoggedOnly,
            Delivery::NotFlying { flying } => Delivered::NotInFlight { flying },
        })
    }

    /// Stand the sortie down: it will admit nothing more.
    ///
    /// The log first and the flag second, the same way round as a verb, and for
    /// the weaker of the two reasons — see [`GROUND_NOTE`].
    ///
    /// # Errors
    ///
    /// [`StoreError`] if the log would not take the row, in which case the sortie
    /// is **not** stood down.
    pub fn ground(&mut self) -> Result<(), StoreError> {
        self.log.append(Event::Note {
            text: GROUND_NOTE.to_owned(),
        })?;
        self.stand_down.order();
        Ok(())
    }

    /// What the slot is on right now.
    #[must_use]
    pub fn flying(&self) -> Option<TaskId> {
        self.in_flight.flying()
    }

    /// Read orders from stdin until it ends.
    ///
    /// Runs on its own thread for the length of a sortie, and returns when stdin
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
            match parse_order(typed) {
                Err(misread) => {
                    let _ = writeln!(out, "  ? {misread}");
                }
                Ok(Order::Slot) => {
                    let _ = match self.flying() {
                        Some(task) => writeln!(out, "  > the slot is flying {task}"),
                        None => writeln!(out, "  > the slot is empty — between attempts"),
                    };
                }
                Ok(Order::Ground) => {
                    let _ = match self.ground() {
                        Ok(()) => match self.flying() {
                            Some(task) => writeln!(
                                out,
                                "  > standing down after {task} lands. \
                                 It is still running; `halt {task}` stops it too."
                            ),
                            None => writeln!(out, "  > standing down; the slot is empty"),
                        },
                        Err(e) => writeln!(out, "  ! not logged, so not ordered: {e}"),
                    };
                }
                Ok(Order::ToTask { task, control }) => {
                    let caveat = caveat(&control);
                    let _ = match self.request(task, control) {
                        Ok(Delivered::ToTheWorker) => {
                            writeln!(out, "  > logged and sent to {task}")
                        }
                        Ok(Delivered::LoggedOnly) => writeln!(
                            out,
                            "  > logged against {task}; its attempt had already ended, \
                             so nothing received it"
                        ),
                        // 🚨 Named, not re-aimed. The operator finds out that the
                        // slot moved instead of finding out afterwards what it
                        // moved onto.
                        Ok(Delivered::NotInFlight {
                            flying: Some(other),
                        }) => writeln!(
                            out,
                            "  > logged against {task}, and sent nowhere: the slot moved to {other}, \
                             and {other} was left alone."
                        ),
                        Ok(Delivered::NotInFlight { flying: None }) => writeln!(
                            out,
                            "  > logged against {task}, and sent nowhere: the slot is empty"
                        ),
                        Err(e) => writeln!(out, "  ! not logged, so not sent: {e}"),
                    };
                    if let Some(caveat) = caveat {
                        let _ = writeln!(out, "{caveat}");
                    }
                }
            }
            let _ = out.flush();
        }
    }
}
