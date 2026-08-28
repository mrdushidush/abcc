//! The schema. One source of truth and four projections of it.
//!
//! `STRICT` on every table: SQLite's default is to accept a string into an
//! integer column, and a store whose whole job is being the record should not
//! quietly widen a type. It costs nothing and it fails at the write rather than
//! at the read, which is where a type error is cheap.

use rusqlite::Connection;

/// Bumped when a migration is needed. There is no migration machinery yet and
/// there does not need to be: nothing outside this repository has a log.
const SCHEMA_VERSION: i64 = 1;

const DDL: &str = r"
-- 🚨 THE SOURCE OF TRUTH. Append-only. Nothing updates a row here except the
-- `fill` that completes its own reservation inside one transaction.
--
-- `seq INTEGER PRIMARY KEY AUTOINCREMENT` is the system's only ordering, and
-- AUTOINCREMENT rather than plain rowid because a reused position would make the
-- SSE `Last-Event-ID` resume path silently wrong.
CREATE TABLE IF NOT EXISTS event (
    seq     INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Unix milliseconds. Data, shown to people, and never a cursor.
    at_ms   INTEGER NOT NULL,
    -- The discriminant, so the common filters are index scans rather than JSON
    -- extraction over every row.
    kind    TEXT    NOT NULL,
    task    INTEGER,
    attempt INTEGER,
    -- The event itself, as JSON.
    body    TEXT    NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS event_task_seq ON event (task, seq);
CREATE INDEX IF NOT EXISTS event_kind_seq ON event (kind, seq);

-- Everything below is a PROJECTION: deleted and rebuilt from `event` on every
-- boot. Do not read one of these as evidence of anything the log does not say.

CREATE TABLE IF NOT EXISTS mission (
    id      INTEGER PRIMARY KEY,
    title   TEXT    NOT NULL,
    created INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS task (
    id      INTEGER PRIMARY KEY,
    mission INTEGER NOT NULL REFERENCES mission (id),
    title   TEXT    NOT NULL,
    prompt  TEXT    NOT NULL,
    -- The TaskState as JSON, tagged. Storing the variant name in a column and the
    -- payload beside it would be two things that can disagree.
    state   TEXT    NOT NULL,
    -- The seq of the transition that produced `state`.
    since   INTEGER NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS task_mission ON task (mission);

-- Immutable except for the two columns written once when it ends.
CREATE TABLE IF NOT EXISTS attempt (
    id              INTEGER PRIMARY KEY,
    task            INTEGER NOT NULL REFERENCES task (id),
    cause           TEXT    NOT NULL,
    checkpoint_from INTEGER,
    started         INTEGER NOT NULL,
    ended           INTEGER,
    outcome         TEXT
) STRICT;

CREATE INDEX IF NOT EXISTS attempt_task ON attempt (task);

-- Dependency edges are rows, so `Blocked` can be derived for display without
-- being a lifecycle variant that then has to be reconciled.
CREATE TABLE IF NOT EXISTS depends_on (
    task    INTEGER NOT NULL REFERENCES task (id),
    on_task INTEGER NOT NULL REFERENCES task (id),
    PRIMARY KEY (task, on_task)
) STRICT;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;
";

/// Create the schema if it is not there, and record its version.
///
/// # Errors
///
/// Fails if the DDL cannot be executed.
pub fn apply(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(DDL)?;
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
         ON CONFLICT (key) DO NOTHING",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}
