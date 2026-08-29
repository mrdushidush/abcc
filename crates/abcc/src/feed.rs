//! The durable [`Feed`]: the reader's paged read, against the `SQLite` log.
//!
//! ADR-0012 §3 gives one integer four roles and [`abcc_tui::Feed`] is the shape
//! they share. `abcc-tui` deliberately does not depend on `abcc-store` — putting
//! rusqlite behind that trait would make the seam decorative — so **this is where
//! the trait meets the database**, and it is the whole of the adapter the reader
//! was written against.
//!
//! 🚨 It reads `event` and nothing else. The `task` projection exists and is
//! cheaper to query, and a reader on it would keep drawing a healthy board for a
//! run that had stopped writing events. That rule lives in `abcc-tui`; this
//! module is the place it could most easily be broken, so it is restated here.

use std::path::Path;

use abcc_core::event::Logged;
use abcc_core::seq::Seq;
use abcc_store::{Store, StoreError};
use abcc_tui::feed::{Feed, FeedError};

/// A read-only view of the log for the reader.
///
/// ⚠ It owns its own connection. The writer — the run — owns another, and
/// `busy_timeout` is set on both (`abcc-store`), which is what makes a console
/// watching a live run a supported thing rather than a race.
pub struct StoreFeed {
    store: Store,
}

impl StoreFeed {
    /// Open the log for reading.
    ///
    /// # Errors
    ///
    /// [`StoreError`] if the file will not open or the projection will not
    /// rebuild.
    pub fn open(path: &Path) -> Result<StoreFeed, StoreError> {
        Ok(StoreFeed {
            store: Store::open(path)?,
        })
    }

    /// The last `seq` written at the moment of asking.
    ///
    /// # Errors
    ///
    /// [`StoreError`] if the query fails.
    pub fn head(&self) -> Result<Seq, StoreError> {
        self.store.head()
    }
}

impl Feed for StoreFeed {
    /// # Errors
    ///
    /// A read that did not happen, carried to the screen rather than swallowed.
    fn read_from(&self, since: Seq, limit: usize) -> Result<Vec<Logged>, FeedError> {
        self.store
            .read_from(since, limit)
            .map_err(|e| FeedError::new(since, e.to_string()))
    }
}
