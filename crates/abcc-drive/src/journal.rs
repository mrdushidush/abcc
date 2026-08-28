//! [`Journal`] over the durable log.
//!
//! The turn loop writes through a trait rather than through a `Store`, because
//! ADR-0006 makes the log the only thing that crosses between a worker and
//! anything else — so the loop's dependency is *there is somewhere to write* and
//! the durable half is the driver's business. This is the durable half.
//!
//! Two facts meet here and they disagree: [`Journal::record`] returns nothing,
//! and [`Store::append`] can fail. 🚨 **A write that did not land is latched and
//! reported, never swallowed.** A log that has quietly stopped accepting writes
//! is the one failure this architecture cannot absorb — status is a projection of
//! it (ADR-0005), so an event that is not there is a consequence that never
//! happened, and the console would show a healthy attempt that is not running.

use abcc_core::event::Event;
use abcc_engine::turn::Journal;
use abcc_store::{Store, StoreError};

/// The turn loop's journal, writing every event to the event log.
pub struct StoreJournal<'a> {
    store: &'a mut Store,
    written: usize,
    failed: Option<StoreError>,
}

impl<'a> StoreJournal<'a> {
    #[must_use]
    pub fn new(store: &'a mut Store) -> StoreJournal<'a> {
        StoreJournal {
            store,
            written: 0,
            failed: None,
        }
    }

    /// How many events landed.
    #[must_use]
    pub fn written(&self) -> usize {
        self.written
    }

    /// The first write that did not land, if any.
    #[must_use]
    pub fn failed(&self) -> Option<&StoreError> {
        self.failed.as_ref()
    }

    /// Give the store back, reporting the first failure if there was one.
    ///
    /// # Errors
    ///
    /// The first [`StoreError`] this journal met.
    pub fn into_result(self) -> Result<usize, StoreError> {
        match self.failed {
            None => Ok(self.written),
            Some(e) => Err(e),
        }
    }
}

impl Journal for StoreJournal<'_> {
    fn record(&mut self, event: Event) {
        match self.store.append(event) {
            Ok(_) => self.written += 1,
            Err(e) => {
                // ⚠ The **first** failure is the one reported, and the writes
                // after it are still attempted. The events that follow a failed
                // write are the record of what the attempt did next, and dropping
                // them would turn one bad write into a hole in the history —
                // which is worse, because a hole is invisible and an error is not.
                self.failed.get_or_insert(e);
            }
        }
    }
}
