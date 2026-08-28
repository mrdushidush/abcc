//! The read seam: a page of the log, positioned by `seq`.
//!
//! ADR-0012 §3 gives one integer four roles — event id, SSE `Last-Event-ID`,
//! paged-read cursor and scrub position — and this trait is the shape all four
//! share. **Two cursors that must agree are two cursors that will not**, so there
//! is one parameter here and it is a [`Seq`].
//!
//! 🚨 **The store is deliberately not behind this trait in this crate.** The
//! reader depends on *there is a log to page through* and nothing more; putting
//! `abcc-store` in this crate's dependencies would put rusqlite behind the seam
//! and make it decorative, which is the same reason `abcc-drive` exists rather
//! than the turn loop owning a `Store`. The durable implementation is composed in
//! the binary, and a second implementation is already foreseen: ADR-0012 §3 has
//! the console reading over SSE, which is this trait against a socket.

use abcc_core::event::{Event, Logged};
use abcc_core::seq::Seq;

/// A read that did not happen. A reader that swallows this shows a healthy board
/// that stopped being true, which is the one failure mode this whole design is
/// arranged against — so the reader carries it to the screen.
#[derive(Debug, thiserror::Error)]
#[error("reading the log from {since}: {detail}")]
pub struct FeedError {
    pub since: Seq,
    pub detail: String,
}

impl FeedError {
    pub fn new(since: Seq, detail: impl Into<String>) -> FeedError {
        FeedError {
            since,
            detail: detail.into(),
        }
    }
}

/// Somewhere to page a log from.
pub trait Feed {
    /// The events after `since`, in `seq` order, at most `limit` of them.
    ///
    /// An empty page means *nothing new yet*, never *the end*: the live view and
    /// the replay view are the same call at different positions.
    ///
    /// # Errors
    ///
    /// Whatever the underlying log could not do.
    fn read_from(&self, since: Seq, limit: usize) -> Result<Vec<Logged>, FeedError>;
}

/// A log held in memory, for tests and for the after-action views that have
/// already read one.
///
/// ⚠ `at_ms` is set by [`Replay::advance`] and is **never** derived from `seq`.
/// A test double that computed one from the other would quietly bless the
/// coupling ADR-0012 §3 exists to forbid, and would then agree with any reader
/// that made the same mistake.
#[derive(Debug, Default)]
pub struct Replay {
    events: Vec<Logged>,
    clock: i64,
}

impl Replay {
    #[must_use]
    pub fn new() -> Replay {
        Replay::default()
    }

    /// Move the wall clock. Time moves only when the caller says so.
    pub fn advance(&mut self, ms: i64) -> &mut Replay {
        self.clock += ms;
        self
    }

    /// Append an event at the current clock, returning the `seq` it landed at.
    pub fn push(&mut self, event: Event) -> Seq {
        let seq = Seq::new(i64::try_from(self.events.len()).unwrap_or(i64::MAX) + 1);
        self.events.push(Logged {
            seq,
            at_ms: self.clock,
            event,
        });
        seq
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// The last `seq` written, or [`Seq::ORIGIN`] on an empty log.
    #[must_use]
    pub fn head(&self) -> Seq {
        self.events.last().map_or(Seq::ORIGIN, |l| l.seq)
    }

    /// The `seq` the next [`Replay::push`] will land at.
    ///
    /// Needed because a lifecycle state's `since` is the `seq` of the very event
    /// that carries it (ADR-0004), so anything building a log has to know where
    /// it is about to write before it writes there.
    #[must_use]
    pub fn next_seq(&self) -> Seq {
        Seq::new(self.head().get() + 1)
    }
}

impl Feed for Replay {
    fn read_from(&self, since: Seq, limit: usize) -> Result<Vec<Logged>, FeedError> {
        Ok(self
            .events
            .iter()
            .filter(|l| l.seq > since)
            .take(limit)
            .cloned()
            .collect())
    }
}
