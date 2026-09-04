//! The reader: the event log, on a screen, in plain text.
//!
//! `PLAN.md` §3 puts this in **Skeleton** rather than at Console, and David's
//! decision of 2026-08-28 says why: the event log is Console's only input, so
//! *"every milestone emits events in the shape Console reads"* is otherwise **a
//! written rule that nothing checks**. A real reader turns it into a
//! compile-and-run failure the day it is violated. It is cheap, nothing throws it
//! away — Console extends it — and it retires the rework risk with code instead
//! of discipline.
//!
//! Four rules hold here, and each one is a test rather than an intention:
//!
//! 1. 🚨 **It reads the log and nothing else.** Not the `task` projection, which
//!    exists and is cheaper to query. Skeleton's exit criterion is *"the reader
//!    shows that run without reading anything but the log"*, and a reader that
//!    queried the projection would keep drawing a healthy board for a milestone
//!    that had stopped writing events. See [`view`].
//! 2. 🚨 **Every event variant has a line, and the compiler enforces it.**
//!    [`line::describe`] is one exhaustive match with no wildcard arm. See
//!    [`line`].
//! 3. **Every state has a label in every theme, and the compiler enforces that
//!    too.** A theme is a label map over a fixed enum (ADR-0012 §6), never a
//!    rename of it. See [`theme`].
//! 4. **One integer positions everything.** `seq` is the event id, the paged-read
//!    cursor and the scrub position; the wall clock is data and never a cursor.
//!    Replay is the live view at a different position, running the same code.
//!    See [`feed`] and [`reader`].
//!
//! What is deliberately not here: **sprites**. The sixel battlefield is ADR-0012's
//! flagship and it lands at the Console milestone, on this read path once it is
//! proven. Adding one here is a milestone, not a patch.
//!
//! ```no_run
//! use abcc_tui::{Replay, Theme, run};
//!
//! let log = Replay::new(); // in the binary this is the SQLite log
//! run(&log, Theme::Command)?;
//! # Ok::<(), std::io::Error>(())
//! ```

pub mod assets;
pub mod battlefield;
pub mod feed;
pub mod line;
pub mod reader;
pub mod render;
pub mod roster;
pub mod sixel;
pub mod theme;
pub mod view;

pub use assets::{AssetError, Corpus, Design, Facing, Pose, Poses};
pub use battlefield::{Battlefield, Grid, Unit};
pub use feed::{Feed, FeedError, Replay};
pub use line::{Line, describe};
pub use reader::{Intent, Reader, run};
pub use render::draw;
pub use roster::{Placed, Post, Roster, Standing};
pub use sixel::{Canvas, Encoder, Sprite};
pub use theme::Theme;
pub use view::{Card, Pulse, View};
