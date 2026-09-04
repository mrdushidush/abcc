//! The roster: which of the log's tasks are on the field, where each one
//! stands, and which of the corpus's four pictures it is drawn with.
//!
//! ADR-0012's battlefield was a **directory listing** until this module existed
//! — `abcc paint` decoded up to nine sprites and stood them in a square, which
//! answers *does this terminal draw our composite* and nothing else. This is the
//! part that makes it a console: **the roster comes from the event log.**
//!
//! Three rules, and each is a test rather than an intention:
//!
//! 1. 🚨 **It is a fold over the log, never a query on the projection.** The
//!    input is [`View`], which is `abcc-tui`'s own fold, for the same reason the
//!    reader will not read `Store::tasks`: a field drawn from the projection
//!    would keep showing a healthy fleet for a run that had stopped writing
//!    events.
//! 2. 🚨 **A task's rank is decided by an exhaustive `match` on its state**, so
//!    a tenth [`TaskState`] cannot ship without somebody deciding where it
//!    stands — the same mechanism [`crate::theme`] uses for labels. And the
//!    decision is cross-checked against [`StateContract`](abcc_core::task::StateContract)
//!    in the tests, so the rule stays *stated* rather than incidental.
//! 3. 🚨 **The slot a unit stands on comes from the fold, not from the state.**
//!    `Deployed` names its unit and **`Engaged` does not**, so *positioned by
//!    slot* is impossible from a state alone; [`Card::slot`] is where the log
//!    remembers it, and a window that opened after the grant honestly says it
//!    does not know.
//!
//! # What the picture cannot say
//!
//! ⚠ **The field cannot name its units.** Two tasks in the same post are the
//! same picture, and there is no text on the canvas — so the field shows the
//! *shape* of the fleet and the legend beside it shows which task is which.
//! ⚠ **The corpus has four pictures for nine states** (F565: `*-attacking`
//! only, east and west), so the rank carries the state and the sprite does not.
//! Neither of these is a defect to be worked around by inventing art.

use std::collections::BTreeSet;

use abcc_core::seq::{MissionId, TaskId, UnitId};
use abcc_core::task::TaskState;

use crate::assets::{Design, Facing, Pose};
use crate::view::View;

/// How many things one rank draws.
///
/// **Bounded on purpose, and the argument is F112's**: the log is designed to
/// grow, the queue has no ceiling, and a rank with no limit turns the field into
/// a stack of silhouettes the moment a board gets busy — the tile shrinks to fit
/// (`geometry`'s clamp, F144) and every unit ends up behind its neighbour.
/// A picture that stops being readable is worse than a picture that says *and
/// six more*, which is what [`Roster::crowded`] is for.
///
/// Six, because six coders at the default `--px 150` span 475 px of a 640 px
/// field — the width the tile clamp starts biting at.
pub const RANK: usize = 6;

/// Where a thing stands on the field, and why it stands there.
///
/// The order is the story the picture tells, back to front: **your base, what is
/// waiting to go in, what is in the fight, and what has come back to you.**
/// Each one is a line of constant depth — see [`Post::depth`], which is where
/// the arithmetic for that is and why it is not simply a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Post {
    /// A mission. Structures at the back; the units stand in front of them.
    Base,
    /// `Queued` — eligible for admission, holding nothing.
    Reserve,
    /// Holding a runtime slot. **`cx` is the slot**, so slot zero is always the
    /// same place on the screen and an operator can learn it.
    ///
    /// `None` is a task the log says holds a slot without this window having
    /// seen which — see [`Card::slot`](crate::view::Card::slot).
    Line { slot: Option<UnitId> },
    /// 🚨 **The operator's rank, nearest the camera.** `AwaitingOrders`,
    /// `Holding` and `Commandeered` are exactly the states whose contract is
    /// [`Watchdog::WatchNeverReap`](abcc_core::task::Watchdog): **nothing moves
    /// them but a person.** They stand in front because they are the only ones
    /// that cannot resolve themselves.
    Waiting,
}

impl Post {
    /// Every rank, back to front.
    pub const ALL: [Post; 4] = [
        Post::Base,
        Post::Reserve,
        Post::Line { slot: None },
        Post::Waiting,
    ];

    /// 🚨 **How far down the field the rank stands — `cx + cy`, which is the
    /// depth key itself.**
    ///
    /// A rank is a line of *constant depth*, so its units are laid out along
    /// `(i, depth - i)` rather than across a row of one `cy`. That is what makes
    /// it a rank rather than a queue receding into the distance, and it is
    /// measurable rather than a matter of taste: on this project's own log the
    /// row layout puts neighbours **60 px apart under a 100 px figure — 40%
    /// of every unit behind the next one** — and constant depth puts them
    /// **134 px apart, which is no overlap at all**. On a full field of four
    /// ranks of six it is 48% against 34%. Consecutive cells on one rank differ
    /// by **two** in `cx - cy`, so a rank spreads by a whole tile per unit where
    /// a row spreads by half of one.
    ///
    /// The stride between ranks is two, which leaves half a tile of ground
    /// between them; one would put the ranks 27 px apart under 150 px figures.
    ///
    /// 🚨 **The base is at `-1`: odd, and three steps back rather than two.**
    /// Both halves of that are load-bearing, and **decoding a painted frame is
    /// what found them** — the arithmetic looked right at every stage.
    ///
    /// *Odd*, because screen x is `cx - cy`, which on a rank at depth `d` is
    /// `2i - d`: **two ranks of the same parity stand in the same screen
    /// columns**, so the one in front covers the one behind. With the base at 0
    /// the single building stood exactly behind a queued unit and the decoder
    /// returned the two as **one connected region** of 100x146. An odd depth
    /// interleaves buildings between the units ahead of them.
    ///
    /// *Three steps*, because one is not enough room. At depth 1 the building
    /// and the rank in front shared a horizontal band — 246x129 of merged art
    /// even on a 1280x720 field — since a rank gap of `tile/4` under a figure a
    /// whole tile tall leaves nothing to see.
    ///
    /// ⚠ **It does not remove occlusion between ranks and nothing can.** A unit
    /// in front covers part of one behind; that is F144 from the layout side,
    /// screen area binds, and `--size` is the answer. What this buys is that a
    /// whole design never disappears.
    ///
    /// ⚠ A negative depth costs one step of vertical budget, because `geometry`
    /// measures its span from the origin outward and so always includes 0.
    #[must_use]
    pub const fn depth(self) -> i32 {
        match self {
            Post::Base => -1,
            Post::Reserve => 2,
            Post::Line { .. } => 4,
            Post::Waiting => 6,
        }
    }

    /// The cell the `i`-th thing on this rank stands on.
    ///
    /// `i` is a column — an index for the ranks that have no better one, and
    /// **the slot number** for the line, which is the whole of *positioned by
    /// slot*.
    #[must_use]
    pub const fn cell(self, column: i32) -> (i32, i32) {
        (column, self.depth() - column)
    }

    /// What the legend calls it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Post::Base => "base",
            Post::Reserve => "reserve",
            Post::Line { .. } => "line",
            Post::Waiting => "waiting",
        }
    }

    /// Two posts are the same rank when they stand at the same depth — a `Line`
    /// on slot 0 and a `Line` on slot 3 are one rank, and grouping has to say so.
    #[must_use]
    pub const fn same_rank(self, other: Post) -> bool {
        self.depth() == other.depth()
    }

    /// 🚨 **Where a task in this state stands, or `None` if it is off the
    /// field.**
    ///
    /// Exhaustive, with no wildcard arm: a tenth [`TaskState`] does not compile
    /// until somebody has decided where it stands, which is the whole reason
    /// this is a `match` and not a lookup on
    /// [`StateContract`](abcc_core::task::StateContract). The contract is what
    /// the tests check it *against* — `holds_slot` is exactly [`Post::Line`],
    /// `WatchNeverReap` is exactly [`Post::Waiting`], and `terminal` is exactly
    /// `None`. Two derivations that must agree, checked, rather than one that
    /// cannot be wrong by construction and says nothing.
    ///
    /// `slot` is the fold's answer to *which unit*, and it matters only for
    /// `Engaged`, which holds a slot and does not name it.
    #[must_use]
    pub const fn of(state: &TaskState, slot: Option<UnitId>) -> Option<Post> {
        match state {
            TaskState::Queued => Some(Post::Reserve),
            TaskState::Deployed { unit, .. } => Some(Post::Line { slot: Some(*unit) }),
            TaskState::Engaged { .. } => Some(Post::Line { slot }),
            TaskState::AwaitingOrders { .. }
            | TaskState::Holding { .. }
            | TaskState::Commandeered { .. } => Some(Post::Waiting),
            // Terminal. It holds nothing, nobody is waiting on it, and a field
            // that kept drawing finished work would fill up with history.
            TaskState::Accomplished { .. }
            | TaskState::Failed { .. }
            | TaskState::Aborted { .. } => None,
        }
    }
}

/// What is standing on a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Standing {
    Mission(MissionId),
    Task(TaskId),
}

impl Standing {
    /// A mission is a building and a task is a unit. The one sentence the design
    /// axis carries.
    #[must_use]
    pub const fn design(self) -> Design {
        match self {
            Standing::Mission(_) => Design::Building,
            Standing::Task(_) => Design::Coder,
        }
    }

    /// The id, as the legend prints it.
    #[must_use]
    pub fn id(self) -> String {
        match self {
            Standing::Mission(m) => m.to_string(),
            Standing::Task(t) => t.to_string(),
        }
    }
}

/// One thing, placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub what: Standing,
    pub post: Post,
    pub cell: (i32, i32),
    pub pose: Pose,
}

/// The field, as the log says it is.
///
/// The counts beside the placements are not decoration: an empty field and a
/// field the roster could not draw look identical, and only one of them is the
/// fleet being quiet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roster {
    placed: Vec<Placed>,
    off: usize,
    crowded: usize,
    contested: usize,
}

impl Roster {
    /// Fold the log's board into a field.
    #[must_use]
    pub fn muster(view: &View) -> Roster {
        let (mut reserve, mut line, mut waiting) = (Vec::new(), Vec::new(), Vec::new());
        let mut missions: BTreeSet<MissionId> = BTreeSet::new();
        let mut off = 0usize;

        for card in view.cards() {
            let Some(post) = Post::of(&card.state, card.slot) else {
                off += 1;
                continue;
            };
            match post {
                Post::Reserve => reserve.push(card.id),
                Post::Line { slot } => line.push((card.id, slot)),
                Post::Waiting => waiting.push(card.id),
                // `Post::of` never says `Base`: a mission is not a task, and a
                // building stands because one of its tasks does.
                Post::Base => {}
            }
            if let Some(mission) = card.mission {
                missions.insert(mission);
            }
        }

        let mut crowded = 0usize;
        let mut cells: Vec<(Standing, Post, (i32, i32))> = Vec::new();

        for (i, mission) in missions.iter().take(RANK).enumerate() {
            let Ok(cx) = i32::try_from(i) else { break };
            cells.push((Standing::Mission(*mission), Post::Base, Post::Base.cell(cx)));
        }
        crowded += missions.len().saturating_sub(RANK);

        crowded += rank_of(&mut cells, &reserve, Post::Reserve);
        let contested = the_line(&mut cells, &line, &mut crowded);
        crowded += rank_of(&mut cells, &waiting, Post::Waiting);

        let placed = face(cells);
        Roster {
            placed,
            off,
            crowded,
            contested,
        }
    }

    /// Everything on the field, back to front is *not* guaranteed —
    /// [`Battlefield::deploy`](crate::battlefield::Battlefield::deploy) sorts,
    /// and this is in roster order so a legend reads left to right.
    #[must_use]
    pub fn placed(&self) -> &[Placed] {
        &self.placed
    }

    /// One rank, in the order it stands.
    pub fn rank(&self, post: Post) -> impl Iterator<Item = &Placed> {
        self.placed.iter().filter(move |p| p.post.same_rank(post))
    }

    /// Every cell in use, which is what a layout is measured over.
    #[must_use]
    pub fn cells(&self) -> Vec<(i32, i32)> {
        self.placed.iter().map(|p| p.cell).collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.placed.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.placed.is_empty()
    }

    /// Tasks the log has finished with. An empty field over a large `off` is a
    /// quiet fleet; an empty field over `off == 0` is an empty board.
    #[must_use]
    pub const fn off(&self) -> usize {
        self.off
    }

    /// Things on the field that this field is too narrow to draw ([`RANK`]).
    #[must_use]
    pub const fn crowded(&self) -> usize {
        self.crowded
    }

    /// 🚨 **Units standing on a slot another unit already holds.**
    ///
    /// A slot holds one task at a time, so this is zero on a healthy log. It is
    /// not zero after a crash: `Store::boot`'s orphan sweep is the thing that
    /// requeues a slot-holding state with no worker behind it, and **only `abcc
    /// run` calls boot** — so a console opened beside a dead run reads two
    /// tasks in slot 0 and draws them stacked. Counting it is what keeps that
    /// from looking like one unit.
    #[must_use]
    pub const fn contested(&self) -> usize {
        self.contested
    }
}

/// Lay a rank out left to right, in task order, and say how many did not fit.
fn rank_of(cells: &mut Vec<(Standing, Post, (i32, i32))>, ids: &[TaskId], post: Post) -> usize {
    for (i, id) in ids.iter().take(RANK).enumerate() {
        let Ok(cx) = i32::try_from(i) else { break };
        cells.push((Standing::Task(*id), post, post.cell(cx)));
    }
    ids.len().saturating_sub(RANK)
}

/// 🚨 **The line, positioned by slot** — the one rank whose `cx` is not an index.
///
/// A slot is a *place*: slot 0 stands in the same column whether or not slot 1
/// is occupied, because the value of positioning by slot is that an operator
/// learns where to look. So a gap in the middle of the line stays a gap, and the
/// layout centres the block around it.
///
/// A task holding a slot this window never saw granted stands **to the left of
/// slot zero**, at `-1`, `-2`, and so on: on the line, because that is where the
/// log says it is, and off the numbered slots, because guessing a number would
/// stand it on top of a task that really is there.
///
/// Returns how many stand on a column another unit already has.
fn the_line(
    cells: &mut Vec<(Standing, Post, (i32, i32))>,
    line: &[(TaskId, Option<UnitId>)],
    crowded: &mut usize,
) -> usize {
    let line_post = Post::Line { slot: None };
    let (mut taken, mut contested, mut unknown) = (BTreeSet::new(), 0usize, 0i32);
    for (id, slot) in line {
        let cx = if let Some(unit) = slot {
            if usize::from(unit.0) >= RANK {
                *crowded += 1;
                continue;
            }
            i32::from(unit.0)
        } else {
            unknown += 1;
            -unknown
        };
        if !taken.insert(cx) {
            contested += 1;
        }
        cells.push((
            Standing::Task(*id),
            Post::Line { slot: *slot },
            line_post.cell(cx),
        ));
    }
    contested
}

/// 🚨 **Which way each thing looks: away from the middle of the field.**
///
/// The base is at the back and the units stand in front of it, so a formation
/// facing outward is one screening what it is working on — and the corpus is
/// `*-attacking` only, so *screening* is the only reading its poses support.
///
/// The axis is `cx - cy`, which is screen x on a 2:1 projection, and the compare
/// is doubled rather than halved so a midpoint between two odd columns does not
/// round a unit onto the wrong flank. A thing exactly on the midline looks west,
/// which makes a single unit on an empty field face west — arbitrary, stated,
/// and one character to flip.
///
/// ⚠ **This is geometry and not state.** Doubling a task's state onto its facing
/// would be inventing a distinction the art cannot carry; what the facing buys
/// is a picture that reads as a formation instead of a row of clones.
fn face(cells: Vec<(Standing, Post, (i32, i32))>) -> Vec<Placed> {
    let screen_x = |(cx, cy): (i32, i32)| cx - cy;
    let lo = cells.iter().map(|c| screen_x(c.2)).min().unwrap_or(0);
    let hi = cells.iter().map(|c| screen_x(c.2)).max().unwrap_or(0);
    let mid = i64::from(lo) + i64::from(hi);
    cells
        .into_iter()
        .map(|(what, post, cell)| {
            let facing = if i64::from(screen_x(cell)) * 2 <= mid {
                Facing::West
            } else {
                Facing::East
            };
            Placed {
                what,
                post,
                cell,
                pose: Pose::new(what.design(), facing),
            }
        })
        .collect()
}
