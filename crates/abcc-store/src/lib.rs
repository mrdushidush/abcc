//! The durable record: one SQLite event log, one writer, and a status table that
//! is its projection.
//!
//! ADR-0005. The donors have three stores and none of them is one. v1 has a real
//! database and never used it as one — `prisma.$transaction` occurs **exactly
//! once in the whole repository, and it is a read** (F166) — in a system whose
//! recovery path mutates locks, slot, status, execution rows and agent in eight
//! steps. The Rust donors keep a JSON document and rewrite it whole, so a crash
//! inside the save costs the record (F168).
//!
//! W3 measured the alternatives rather than reasoning about them, on this machine,
//! 2,000 transitions per configuration, where a transition is the real shape used
//! here — an event row appended and the status projection upserted in the same
//! transaction:
//!
//! | configuration | transitions/s |
//! |---|---|
//! | `journal=DELETE`, `synchronous=FULL` (SQLite's default) | 293 |
//! | **WAL, `synchronous=FULL`** | **1076** |
//! | WAL, `synchronous=NORMAL` | 11197 |
//!
//! and the control, a status document rewritten whole, which degrades **47x**
//! from ten rows to two thousand because it is O(size of everything) per O(1)
//! change. The append does not degrade, and this log is *designed* to grow: it is
//! also the console's replay source.
//!
//! `synchronous=FULL` rather than the conventional `NORMAL` is the one place this
//! overrides received practice, and the justification is arithmetic. One decoded
//! token costs ~13–14 ms on this box (F80), and the log is written at **action**
//! granularity, where one event covers hundreds of tokens. A 929 µs transition is
//! well under a tenth of a percent of that. The usual tradeoff assumes the write
//! rate is the bottleneck; here it is three orders of magnitude from being one.
//!
//! # What is a source of truth and what is not
//!
//! **The `event` table is.** Everything else in the schema is a projection: it is
//! deleted and rebuilt by [`Store::rebuild`], which is the same function boot
//! runs, so there is one recovery path and it executes on every start. v1's
//! second recovery path was the one that had never run when it was needed.

mod schema;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use abcc_core::attempt::{Attempt, AttemptOutcome, Cause};
use abcc_core::event::{Event, Logged};
use abcc_core::run::Mode;
use abcc_core::seq::{AttemptId, CheckpointId, MissionId, Seq, TaskId};
use abcc_core::task::{BootAction, Command, Refused, RequeueReason, TaskState};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

/// Anything that can go wrong that is *not* a refusal. A refused command is a
/// normal outcome and is reported as [`Applied::Refused`], never as an error.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serializing an event: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no task {0} in the projection")]
    NoSuchTask(TaskId),
    #[error("the log holds a {kind} at seq {seq} that the projection cannot apply: {detail}")]
    Unprojectable {
        seq: Seq,
        kind: &'static str,
        detail: String,
    },
}

type Result<T> = std::result::Result<T, StoreError>;

/// What happened to a command. A refusal is written to the log too, because a
/// foreign process hammering a command it is not allowed to send is a thing an
/// operator should be able to see.
#[derive(Debug, Clone)]
pub enum Applied {
    Moved(Logged),
    Refused { logged: Logged, refusal: Refused },
}

impl Applied {
    /// The event that was written, either way.
    #[must_use]
    pub fn logged(&self) -> &Logged {
        match self {
            Applied::Moved(l) | Applied::Refused { logged: l, .. } => l,
        }
    }

    #[must_use]
    pub fn moved(&self) -> bool {
        matches!(self, Applied::Moved(_))
    }
}

/// A task as the projection holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub id: TaskId,
    pub mission: MissionId,
    pub title: String,
    pub prompt: String,
    pub state: TaskState,
    /// The seq of the transition that produced `state`. Equal to the `since`
    /// inside the variant wherever the variant has one; `Queued` has none, which
    /// is why the column exists separately.
    pub since: Seq,
}

/// What a boot reconciliation did, so it can be shown rather than guessed at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub events_replayed: usize,
    pub tasks: usize,
    /// Tasks whose state named an in-process resource that did not survive.
    pub requeued: Vec<TaskId>,
    /// Prompts to put back in front of the operator. **Never auto-answered.**
    pub prompts_to_represent: Vec<TaskId>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open or create the log, apply the pragmas, and rebuild the projection.
    ///
    /// The rebuild is not conditional. Boot is replay, it runs every time, and
    /// that is what stops it from rotting.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be opened, the schema cannot be applied, or the
    /// log holds an event the projection cannot fold.
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path)?;
        let mut store = Store { conn };
        store.configure()?;
        schema::apply(&store.conn)?;
        Ok(store)
    }

    /// An in-memory log, for tests. Same schema, same pragmas that apply.
    ///
    /// # Errors
    ///
    /// Fails if the schema cannot be applied.
    pub fn in_memory() -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        let mut store = Store { conn };
        store.configure()?;
        schema::apply(&store.conn)?;
        Ok(store)
    }

    fn configure(&mut self) -> Result<()> {
        // Two of these are not defaults and one is not the WAL convention.
        // `query_row` rather than `execute` for journal_mode: it returns the
        // resulting mode, and a bare `execute` errors on the returned row.
        let mode: String = self
            .conn
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .unwrap_or_else(|_| "unknown".to_owned());
        // An in-memory database cannot do WAL and reports `memory`; a file
        // database that silently stayed on the rollback journal would be running
        // at 293 transitions/s, so it is worth not discovering that later.
        debug_assert!(
            mode == "wal" || mode == "memory",
            "journal_mode came back as {mode}"
        );
        self.conn.pragma_update(None, "synchronous", "FULL")?;
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        // The console is a reader and must never see a bare SQLITE_BUSY.
        self.conn.pragma_update(None, "busy_timeout", 5000)?;
        Ok(())
    }

    /// Read a pragma back as text. The configuration this store depends on is a
    /// claim about a live connection, not about the code that set it — a file
    /// that silently stayed on the rollback journal runs at 293 transitions/s and
    /// looks identical from here.
    ///
    /// # Errors
    ///
    /// Fails if the pragma does not exist or does not return a row.
    pub fn pragma_text(&self, name: &str) -> Result<String> {
        Ok(self
            .conn
            .query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))?)
    }

    /// Read a pragma back as an integer. `synchronous` is 2 for `FULL`.
    ///
    /// # Errors
    ///
    /// Fails if the pragma does not exist or does not return a row.
    pub fn pragma_int(&self, name: &str) -> Result<i64> {
        Ok(self
            .conn
            .query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))?)
    }

    /// The last position written. `Seq::ORIGIN` on an empty log.
    ///
    /// # Errors
    ///
    /// Fails if the query fails.
    pub fn head(&self) -> Result<Seq> {
        let raw: i64 = self
            .conn
            .query_row("SELECT COALESCE(MAX(seq), 0) FROM event", [], |r| r.get(0))?;
        Ok(Seq::new(raw))
    }

    // -- writing -----------------------------------------------------------

    /// Append an event that is not a lifecycle transition.
    ///
    /// The projection is folded in the same transaction, so a reader can never
    /// see an event whose consequence has not landed.
    ///
    /// # Errors
    ///
    /// Fails if the write fails or the event cannot be projected.
    pub fn append(&mut self, event: Event) -> Result<Logged> {
        let at_ms = now_ms();
        let tx = self.conn.transaction()?;
        let logged = write_event(&tx, at_ms, event)?;
        project(&tx, &logged)?;
        tx.commit()?;
        Ok(logged)
    }

    /// 🚨 **The one write path.** Every lifecycle transition goes through here:
    /// it appends the event and updates the status row in the same transaction,
    /// nothing else writes status, and no generic set-status endpoint exists.
    ///
    /// # Errors
    ///
    /// Fails if the task is unknown or the write fails. A command the state
    /// machine rejects is **not** an error — it comes back as
    /// [`Applied::Refused`], with the refusal recorded on the log.
    pub fn apply(&mut self, task: TaskId, command: Command) -> Result<Applied> {
        let at_ms = now_ms();
        let tx = self.conn.transaction()?;

        let from = read_state(&tx, task)?.ok_or(StoreError::NoSuchTask(task))?;

        // The seq is reserved before the decision, because the resulting state
        // carries it: `since` is a position in this log, which is what makes
        // *when* and *where in the replay* the same fact. Reserving means one
        // extra UPDATE inside a transaction that is already fsync-bound.
        let seq = reserve(&tx)?;

        let applied = match from.apply(&command, seq) {
            Ok(to) => {
                let event = Event::TaskTransitioned {
                    task,
                    command,
                    from,
                    to,
                };
                let logged = fill(&tx, seq, at_ms, event)?;
                project(&tx, &logged)?;
                Applied::Moved(logged)
            }
            Err(refusal) => {
                let event = Event::CommandRefused {
                    task,
                    command,
                    state: from,
                    refusal: refusal.to_string(),
                };
                let logged = fill(&tx, seq, at_ms, event)?;
                Applied::Refused { logged, refusal }
            }
        };

        tx.commit()?;
        Ok(applied)
    }

    // -- reading -----------------------------------------------------------

    /// A page of the log, in `seq` order, starting after `since`.
    ///
    /// This is the console's read path and the SSE resume path: `since` is a
    /// `Last-Event-ID`, a paged-read cursor and a scrub position, all being the
    /// same integer.
    ///
    /// # Errors
    ///
    /// Fails if the query fails or a stored event will not deserialize.
    pub fn read_from(&self, since: Seq, limit: usize) -> Result<Vec<Logged>> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, at_ms, body FROM event WHERE seq > ?1 ORDER BY seq LIMIT ?2")?;
        let rows = stmt.query_map(
            params![since.get(), i64::try_from(limit).unwrap_or(i64::MAX)],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, at_ms, body) = row?;
            out.push(Logged {
                seq: Seq::new(seq),
                at_ms,
                event: serde_json::from_str(&body)?,
            });
        }
        Ok(out)
    }

    /// One task's history, in `seq` order.
    ///
    /// # Errors
    ///
    /// Fails if the query fails or a stored event will not deserialize.
    pub fn task_history(&self, task: TaskId) -> Result<Vec<Logged>> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, at_ms, body FROM event WHERE task = ?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![task.born().get()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, at_ms, body) = row?;
            out.push(Logged {
                seq: Seq::new(seq),
                at_ms,
                event: serde_json::from_str(&body)?,
            });
        }
        Ok(out)
    }

    /// The board, as the projection holds it.
    ///
    /// # Errors
    ///
    /// Fails if the query fails or a stored state will not deserialize.
    pub fn tasks(&self) -> Result<Vec<TaskRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, mission, title, prompt, state, since FROM task ORDER BY id")?;
        let rows = stmt.query_map([], task_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// One task.
    ///
    /// # Errors
    ///
    /// Fails if the query fails or the stored state will not deserialize.
    pub fn task(&self, id: TaskId) -> Result<Option<TaskRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, mission, title, prompt, state, since FROM task WHERE id = ?1",
                params![id.born().get()],
                task_row,
            )
            .optional()?)
    }

    /// One attempt.
    ///
    /// # Errors
    ///
    /// Fails if the query fails or a stored value will not deserialize.
    pub fn attempt(&self, id: AttemptId) -> Result<Option<Attempt>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, task, cause, checkpoint_from, started, ended, outcome
                 FROM attempt WHERE id = ?1",
                params![id.born().get()],
                attempt_row,
            )
            .optional()?)
    }

    /// The mode this run is effectively in: the mode it started in, moved by any
    /// downgrade on the log. A projection rather than a variable, so replay
    /// reproduces it.
    ///
    /// # Errors
    ///
    /// Fails if the query fails or a stored event will not deserialize.
    pub fn effective_mode(&self) -> Result<Option<Mode>> {
        let mut stmt = self.conn.prepare(
            "SELECT body FROM event WHERE kind IN ('run_started', 'mode_downgraded')
             ORDER BY seq DESC LIMIT 1",
        )?;
        let body: Option<String> = stmt.query_row([], |r| r.get(0)).optional()?;
        let Some(body) = body else { return Ok(None) };
        Ok(match serde_json::from_str::<Event>(&body)? {
            Event::RunStarted { mode, .. } => Some(mode),
            Event::ModeDowngraded { to, .. } => Some(to),
            _ => None,
        })
    }

    // -- boot --------------------------------------------------------------

    /// Rebuild the projection from the log. **This is boot, and it is also
    /// replay** — one function, run on every start, so it cannot be the recovery
    /// path that had never executed when it was finally needed.
    ///
    /// F170 measured the cost rather than assuming it: a 100,000-event log
    /// replays in **94 ms**, 1,059,017 events/s, at 263 bytes per event. No
    /// snapshot mechanism ships, because the crossover where replay reaches one
    /// second is ~1M events and W5 projects months of real driving at ~16k.
    ///
    /// # Errors
    ///
    /// Fails if the log cannot be read or holds an event the projection cannot
    /// fold.
    pub fn rebuild(&mut self) -> Result<usize> {
        let tx = self.conn.transaction()?;
        // Order matters: the children first, because the projection declares its
        // foreign keys and the pragma that enforces them is on.
        tx.execute("DELETE FROM depends_on", [])?;
        tx.execute("DELETE FROM attempt", [])?;
        tx.execute("DELETE FROM task", [])?;
        tx.execute("DELETE FROM mission", [])?;

        let mut stmt = tx.prepare("SELECT seq, at_ms, body FROM event ORDER BY seq")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut n = 0usize;
        for row in rows {
            let (seq, at_ms, body) = row?;
            let logged = Logged {
                seq: Seq::new(seq),
                at_ms,
                event: serde_json::from_str(&body)?,
            };
            project(&tx, &logged)?;
            n += 1;
        }
        drop(stmt);
        tx.commit()?;
        Ok(n)
    }

    /// Rebuild, then act on each task's [`BootAction`].
    ///
    /// The reconciliation is itself written to the log — a requeue is a
    /// `Requeue { OrphanedByRestart }` command through [`Store::apply`], not a
    /// silent column edit — so a second boot over the same log is a no-op and
    /// the console can show what the restart did.
    ///
    /// 🚨 A prompt is **re-presented and never auto-answered**. The arrival time
    /// is part of the contract, and this function only reports which tasks are
    /// waiting.
    ///
    /// # Errors
    ///
    /// Fails if the rebuild fails or a reconciling command cannot be written.
    pub fn boot(&mut self) -> Result<Reconciled> {
        let events_replayed = self.rebuild()?;
        let mut out = Reconciled {
            events_replayed,
            ..Reconciled::default()
        };

        let tasks = self.tasks()?;
        out.tasks = tasks.len();
        for t in tasks {
            match t.state.contract().boot {
                BootAction::Stands => {}
                BootAction::RepresentPrompt => out.prompts_to_represent.push(t.id),
                BootAction::Requeue => {
                    // Tombstone the attempt that died with the process before the
                    // task moves, so the record says what happened to it rather
                    // than leaving an attempt open forever (F151).
                    if let Some(a) = t.state.attempt_in_flight() {
                        self.append(Event::AttemptEnded {
                            task: t.id,
                            attempt: a,
                            outcome: AttemptOutcome::HardFailure {
                                why: abcc_core::outcome::Why::EngineError {
                                    detail: "orphaned by restart".to_owned(),
                                },
                            },
                        })?;
                    }
                    self.apply(
                        t.id,
                        Command::Requeue {
                            why: RequeueReason::OrphanedByRestart,
                        },
                    )?;
                    out.requeued.push(t.id);
                }
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// The write primitives
// ---------------------------------------------------------------------------

/// Take the next position without deciding yet what goes in it.
///
/// The placeholder never escapes the transaction: either the `fill` lands and
/// commits, or the whole thing rolls back and the row was never there.
fn reserve(tx: &Transaction<'_>) -> Result<Seq> {
    tx.execute(
        "INSERT INTO event (at_ms, kind, task, attempt, body) VALUES (0, 'reserved', NULL, NULL, '')",
        [],
    )?;
    Ok(Seq::new(tx.last_insert_rowid()))
}

fn fill(tx: &Transaction<'_>, seq: Seq, at_ms: i64, event: Event) -> Result<Logged> {
    let body = serde_json::to_string(&event)?;
    tx.execute(
        "UPDATE event SET at_ms = ?1, kind = ?2, task = ?3, attempt = ?4, body = ?5 WHERE seq = ?6",
        params![
            at_ms,
            event.kind(),
            event.task().map(|t| t.born().get()),
            event.attempt().map(|a| a.born().get()),
            body,
            seq.get(),
        ],
    )?;
    Ok(Logged { seq, at_ms, event })
}

fn write_event(tx: &Transaction<'_>, at_ms: i64, event: Event) -> Result<Logged> {
    let seq = reserve(tx)?;
    fill(tx, seq, at_ms, event)
}

// ---------------------------------------------------------------------------
// The projection — the only place that writes anything but `event`
// ---------------------------------------------------------------------------

/// Fold one event into the projection.
///
/// Every caller runs this inside the same transaction as the append, and
/// [`Store::rebuild`] runs it over the whole log. That is the point: there is
/// one fold, so the state after a restart is the state before it by
/// construction rather than by two pieces of code agreeing.
fn project(tx: &Transaction<'_>, logged: &Logged) -> Result<()> {
    let seq = logged.seq;
    match &logged.event {
        Event::MissionCreated { title } => {
            tx.execute(
                "INSERT INTO mission (id, title, created) VALUES (?1, ?2, ?3)",
                params![seq.get(), title, logged.at_ms],
            )?;
        }
        Event::TaskCreated {
            mission,
            title,
            prompt,
        } => {
            let state = serde_json::to_string(&TaskState::Queued)?;
            tx.execute(
                "INSERT INTO task (id, mission, title, prompt, state, since)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    seq.get(),
                    mission.born().get(),
                    title,
                    prompt,
                    state,
                    seq.get()
                ],
            )?;
        }
        Event::TaskDependsOn { task, on } => {
            tx.execute(
                "INSERT OR IGNORE INTO depends_on (task, on_task) VALUES (?1, ?2)",
                params![task.born().get(), on.born().get()],
            )?;
        }
        Event::TaskTransitioned { task, to, .. } => {
            let state = serde_json::to_string(to)?;
            let n = tx.execute(
                "UPDATE task SET state = ?1, since = ?2 WHERE id = ?3",
                params![state, seq.get(), task.born().get()],
            )?;
            if n != 1 {
                return Err(StoreError::Unprojectable {
                    seq,
                    kind: "task_transitioned",
                    detail: format!("{task} is not in the projection"),
                });
            }
        }
        Event::AttemptStarted {
            task,
            cause,
            checkpoint_from,
            ..
        } => {
            tx.execute(
                "INSERT INTO attempt (id, task, cause, checkpoint_from, started, ended, outcome)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL)",
                params![
                    seq.get(),
                    task.born().get(),
                    serde_json::to_string(cause)?,
                    checkpoint_from.map(|c| c.born().get()),
                    seq.get(),
                ],
            )?;
        }
        Event::AttemptEnded {
            attempt, outcome, ..
        } => {
            let n = tx.execute(
                "UPDATE attempt SET ended = ?1, outcome = ?2 WHERE id = ?3 AND ended IS NULL",
                params![
                    seq.get(),
                    serde_json::to_string(outcome)?,
                    attempt.born().get()
                ],
            )?;
            // An attempt is immutable: ending one twice is a bug in the caller,
            // not something to absorb quietly.
            if n != 1 {
                return Err(StoreError::Unprojectable {
                    seq,
                    kind: "attempt_ended",
                    detail: format!("{attempt} is missing or already ended"),
                });
            }
        }
        // Everything else is a record and changes no projected state. This arm is
        // deliberately a catch-all rather than an exhaustive list: an event that
        // needs a projection is a deliberate act, and adding one to this match is
        // where that act belongs.
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Row decoding
// ---------------------------------------------------------------------------

fn read_state(tx: &Transaction<'_>, task: TaskId) -> Result<Option<TaskState>> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT state FROM task WHERE id = ?1",
            params![task.born().get()],
            |r| r.get(0),
        )
        .optional()?;
    match raw {
        None => Ok(None),
        Some(s) => Ok(Some(serde_json::from_str(&s)?)),
    }
}

fn task_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    let state: String = r.get(4)?;
    Ok(TaskRow {
        id: TaskId::at(Seq::new(r.get(0)?)),
        mission: MissionId::at(Seq::new(r.get(1)?)),
        title: r.get(2)?,
        prompt: r.get(3)?,
        state: serde_json::from_str(&state).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?,
        since: Seq::new(r.get(5)?),
    })
}

fn attempt_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Attempt> {
    let cause: String = r.get(2)?;
    let outcome: Option<String> = r.get(6)?;
    let conv = |i: usize, e: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    };
    Ok(Attempt {
        id: AttemptId::at(Seq::new(r.get(0)?)),
        task: TaskId::at(Seq::new(r.get(1)?)),
        cause: serde_json::from_str::<Cause>(&cause).map_err(|e| conv(2, e))?,
        checkpoint_from: r
            .get::<_, Option<i64>>(3)?
            .map(|v| CheckpointId::at(Seq::new(v))),
        started: Seq::new(r.get(4)?),
        ended: r.get::<_, Option<i64>>(5)?.map(Seq::new),
        outcome: match outcome {
            None => None,
            Some(s) => Some(serde_json::from_str::<AttemptOutcome>(&s).map_err(|e| conv(6, e))?),
        },
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
