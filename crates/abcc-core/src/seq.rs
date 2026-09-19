//! `Seq` — the log position, and the identity of everything the log creates.
//!
//! ADR-0005 gives one integer four roles: event id, SSE `Last-Event-ID`, paged-read
//! cursor and scrub position. ADR-0004 adds a fifth by making every lifecycle
//! variant carry `since: Seq`, so *when* and *where in the replay* are one fact.
//!
//! This module takes the sixth step and makes it **identity**: a task's id is the
//! `seq` of the event that created it, and likewise for missions, attempts,
//! checkpoints and prompts. That removes an id allocator, removes a uuid
//! dependency, and makes lineage sort correctly by construction — an attempt that
//! forked from another necessarily has the larger id. The one thing it does *not*
//! cover is a unit: a slot is configured hardware (ADR-0003, N=2 set by VRAM),
//! not something an event brings into existence, so [`UnitId`] is a plain index.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A position in the event log. `SQLite` hands these out as
/// `INTEGER PRIMARY KEY AUTOINCREMENT`, so the width is i64's and the sequence
/// never reuses a value even after a delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(i64);

impl Seq {
    /// The position before the first event. Nothing is ever written here; it is
    /// the starting cursor for a reader that has seen nothing.
    pub const ORIGIN: Seq = Seq(0);

    #[must_use]
    pub const fn new(raw: i64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    /// Returns true if this is [`Seq::ORIGIN`].
    #[must_use]
    pub const fn is_origin(self) -> bool {
        self.0 == Seq::ORIGIN.get()
    }

    /// Move this position backwards by `n`, saturating at [`Seq::ORIGIN`].
    #[must_use]
    pub fn back(self, n: u64) -> Seq {
        let Ok(n) = i64::try_from(n) else {
            return Seq::ORIGIN;
        };
        match self.0.checked_sub(n) {
            Some(pos) if pos >= 0 => Seq(pos),
            _ => Seq::ORIGIN,
        }
    }
}

impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_stays_at_origin() {
        assert!(Seq::ORIGIN.back(10).is_origin());
    }

    #[test]
    fn back_moves_backwards() {
        let pos = Seq::new(5);
        assert_eq!(pos.back(3), Seq::new(2));
    }

    #[test]
    fn back_saturates_at_origin() {
        let pos = Seq::new(2);
        assert!(pos.back(10).is_origin());
    }

    #[test]
    fn back_overflow_returns_origin() {
        // u64::MAX as a subtraction count would overflow i64, so should return ORIGIN.
        let pos = Seq::new(u64::MAX.cast_signed());
        assert!(pos.back(u64::MAX).is_origin());
    }

    #[test]
    fn back_exact() {
        assert_eq!(Seq::new(10).back(10), Seq::ORIGIN);
    }
}

/// Declares an identity newtype whose value is the `Seq` of the event that
/// created the thing it names.
macro_rules! seq_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Seq);

        impl $name {
            /// The identity of a thing created by the event at `seq`.
            #[must_use]
            pub const fn at(seq: Seq) -> Self {
                Self(seq)
            }

            /// The event that created it — always readable from the log.
            #[must_use]
            pub const fn born(self) -> Seq {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0.get())
            }
        }
    };
}

seq_id!(
    /// A mission: the operator-level unit of work, holding a set of tasks.
    MissionId,
    "m"
);
seq_id!(
    /// A task: one item on the board, carrying a [`crate::TaskState`].
    TaskId,
    "t"
);
seq_id!(
    /// An attempt: immutable once started (ADR-0004). Retry, edit, re-route and
    /// replay all fork a *new* attempt from a checkpoint rather than mutating one.
    AttemptId,
    "a"
);
seq_id!(
    /// A checkpoint: a commit sha written to a ref (ADR-0007). The id indexes the
    /// event that took it; the sha itself lives in that event.
    CheckpointId,
    "c"
);
seq_id!(
    /// A question put to the operator. `AwaitingOrders` names one, and boot
    /// re-presents it rather than answering it (ADR-0004, F132).
    PromptId,
    "p"
);

/// A runtime slot — a resident model plus a tool policy (ADR-0003). The count is
/// set by VRAM and measured concurrency (N=2), never by the length of the phase
/// list, and a slot is configuration rather than something an event creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnitId(pub u8);

impl fmt::Display for UnitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unit-{}", self.0)
    }
}
