//! The fold: a log in, a screenful out.
//!
//! 🚨 **The reader reads the log and nothing else.** It never touches the `task`
//! projection, even though one exists and is cheaper to query — because the
//! Skeleton exit criterion is *"the reader shows that run without reading
//! anything but the log"*, and a reader that queried the projection would keep
//! showing a healthy board for a milestone that had stopped writing events. The
//! projection is a second reader of the same log (ADR-0005); this is the first.
//!
//! Three things fall out of that, and each has a test:
//!
//! 1. **A card can be built from a transition alone.** `TaskTransitioned` carries
//!    `from` as well as `to` precisely so a reader tailing from the middle of the
//!    log never has to have seen the earlier events. Folding from mid-log gives a
//!    card with the right state and no title, and the screen says so.
//! 2. **An attempt's events are its task's heartbeat.** `ModelCallEnded` names an
//!    attempt and no task, so the fold learns the mapping at `AttemptStarted` and
//!    resolves it. Without that, a task running four model calls inside one span
//!    looks stalled — which is v1's F148 defect 2, exactly.
//! 3. **The feed is bounded.** F112: the donor's frontend OOM is what its ring
//!    buffer was introduced to fix, and the log this reads is *designed* to grow.

use std::collections::{BTreeMap, VecDeque};

use abcc_core::event::{Event, Logged};
use abcc_core::run::Mode;
use abcc_core::seq::{AttemptId, MissionId, Seq, TaskId};
use abcc_core::task::{Liveness, TaskState};

use crate::line::{Line, describe};
use crate::theme::Theme;

/// How many feed lines the reader keeps. Bounded on purpose (F112).
pub const FEED_LINES: usize = 512;

/// W5's bar, in milliseconds: **no gap over ten seconds goes unmarked**.
///
/// 🚨 Re-exported from [`abcc_core::fun`] rather than spelled again here. The
/// reader and the query that grades the reader must not be able to disagree
/// about where the bar is — two constants that have to match are two constants
/// that eventually will not, which is ADR-0012's own argument for one `seq`.
///
/// The reader cannot fix the silence; it can refuse to hide it.
pub use abcc_core::fun::SILENCE_BAR_MS;

/// A task as the reader has folded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub id: TaskId,
    pub mission: Option<MissionId>,
    /// Empty when the reader started after the task was created. [`Card::label`]
    /// falls back to the id rather than inventing one.
    pub title: String,
    pub prompt: String,
    pub state: TaskState,
    /// The `seq` of the transition that produced `state`.
    pub since: Seq,
    /// Wall clock of that transition — the clock a `SinceEntered` or `Human`
    /// state is measured against.
    pub since_at_ms: i64,
    /// The `seq` of the last event about this task **or any of its attempts**.
    /// This is the progress clock, and it is a query over the log rather than a
    /// second clock that can drift from the first.
    pub last_seq: Seq,
    pub last_at_ms: i64,
    pub attempts: usize,
    /// The question the operator has not answered yet.
    pub question: Option<String>,
}

impl Card {
    /// What to call it on screen.
    #[must_use]
    pub fn label(&self) -> String {
        if self.title.is_empty() {
            format!("{} (before this window)", self.id)
        } else {
            self.title.clone()
        }
    }
}

/// The clock a state's contract says to read, already read.
///
/// The variants are not interchangeable and that is the point: ADR-0004 makes the
/// watchdog distinguish *no human yet* from *no progress*, which is the
/// distinction F91's forty silent minutes taught the harness. A human clock is
/// displayed and **never** a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pulse {
    /// Nothing is running and nothing is waiting. There is no clock to read.
    Untimed,
    /// Time since the state was entered.
    Since { ms: i64 },
    /// The attempt's progress clock. `stalled` is the W5 bar, and it is the only
    /// variant that ever raises one.
    Progress { ms: i64, stalled: bool },
    /// A human is expected. Measured, displayed, never used to reap.
    Waiting { ms: i64 },
}

impl Pulse {
    /// Whether the operator should be looking at this one.
    #[must_use]
    pub fn stalled(self) -> bool {
        matches!(self, Pulse::Progress { stalled: true, .. })
    }
}

/// Everything the reader knows, folded from the log.
///
/// `PartialEq` is derived on purpose: the claim that replay is the live view at a
/// different position is only worth making if a test can hold the two side by
/// side and compare them field for field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    theme: Theme,
    cursor: Seq,
    events: usize,
    mode: Option<Mode>,
    version: Option<String>,
    pid: Option<u32>,
    missions: BTreeMap<MissionId, String>,
    tasks: BTreeMap<TaskId, Card>,
    attempt_task: BTreeMap<AttemptId, TaskId>,
    feed: VecDeque<Line>,
    first_at_ms: Option<i64>,
    last_at_ms: Option<i64>,
    review_seconds: u32,
    reviews: usize,
}

impl View {
    #[must_use]
    pub fn new(theme: Theme) -> View {
        View {
            theme,
            cursor: Seq::ORIGIN,
            events: 0,
            mode: None,
            version: None,
            pid: None,
            missions: BTreeMap::new(),
            tasks: BTreeMap::new(),
            attempt_task: BTreeMap::new(),
            feed: VecDeque::new(),
            first_at_ms: None,
            last_at_ms: None,
            review_seconds: 0,
            reviews: 0,
        }
    }

    /// Fold one event in. The cursor moves to its `seq` and only to its `seq`.
    pub fn fold(&mut self, logged: &Logged) {
        self.cursor = logged.seq;
        self.events += 1;
        self.first_at_ms.get_or_insert(logged.at_ms);
        self.last_at_ms = Some(logged.at_ms);

        self.project(logged);

        self.feed.push_back(describe(logged, self.theme));
        while self.feed.len() > FEED_LINES {
            self.feed.pop_front();
        }
    }

    /// Fold a page.
    pub fn fold_all(&mut self, page: &[Logged]) {
        for logged in page {
            self.fold(logged);
        }
    }

    fn project(&mut self, logged: &Logged) {
        match &logged.event {
            Event::RunStarted { mode, version, pid } => {
                self.mode = Some(*mode);
                self.version = Some(version.clone());
                self.pid = Some(*pid);
            }
            Event::ModeDowngraded { to, .. } => self.mode = Some(*to),
            Event::MissionCreated { title } => {
                self.missions
                    .insert(MissionId::at(logged.seq), title.clone());
            }
            Event::TaskCreated {
                mission,
                title,
                prompt,
            } => {
                let id = TaskId::at(logged.seq);
                let card = self.card(id, logged);
                card.mission = Some(*mission);
                card.title.clone_from(title);
                card.prompt.clone_from(prompt);
                card.since = logged.seq;
                card.since_at_ms = logged.at_ms;
            }
            Event::TaskTransitioned { task, to, .. } => {
                let (task, at_ms, seq) = (*task, logged.at_ms, logged.seq);
                let card = self.card(task, logged);
                card.state = to.clone();
                card.since = seq;
                card.since_at_ms = at_ms;
            }
            Event::AttemptStarted { task, .. } => {
                let (task, attempt) = (*task, AttemptId::at(logged.seq));
                self.attempt_task.insert(attempt, task);
                self.card(task, logged).attempts += 1;
            }
            Event::OperatorPrompted { task, question, .. } => {
                let (task, question) = (*task, question.clone());
                self.card(task, logged).question = Some(question);
            }
            Event::OperatorAnswered { task, .. } => {
                let task = *task;
                self.card(task, logged).question = None;
            }
            Event::ReviewRecorded { seconds, .. } => {
                self.review_seconds += seconds;
                self.reviews += 1;
            }
            _ => {}
        }
        // The heartbeat, for anything that names a task directly or names one of
        // its attempts. Done after the projection so a card created by this very
        // event gets its own clock set.
        if let Some(task) = self.subject(&logged.event) {
            let card = self.card(task, logged);
            card.last_seq = logged.seq;
            card.last_at_ms = logged.at_ms;
        }
    }

    /// The task an event is about: the one it names, or the one owning the
    /// attempt it names.
    fn subject(&self, event: &Event) -> Option<TaskId> {
        event.task().or_else(|| {
            event
                .attempt()
                .and_then(|a| self.attempt_task.get(&a).copied())
        })
    }

    /// The card for a task, created empty if this reader has never seen it born.
    fn card(&mut self, id: TaskId, logged: &Logged) -> &mut Card {
        self.tasks.entry(id).or_insert_with(|| Card {
            id,
            mission: None,
            title: String::new(),
            prompt: String::new(),
            state: TaskState::Queued,
            since: logged.seq,
            since_at_ms: logged.at_ms,
            last_seq: logged.seq,
            last_at_ms: logged.at_ms,
            attempts: 0,
            question: None,
        })
    }

    // -- what the screen asks for -------------------------------------------

    #[must_use]
    pub fn theme(&self) -> Theme {
        self.theme
    }

    /// The position this view has been folded to: event id, page cursor and
    /// scrub position, one integer (ADR-0012 §3).
    #[must_use]
    pub fn cursor(&self) -> Seq {
        self.cursor
    }

    #[must_use]
    pub fn events(&self) -> usize {
        self.events
    }

    #[must_use]
    pub fn mode(&self) -> Option<Mode> {
        self.mode
    }

    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    #[must_use]
    pub fn mission(&self, id: MissionId) -> Option<&str> {
        self.missions.get(&id).map(String::as_str)
    }

    /// The board, in task order — which is creation order, because a task's id is
    /// the `seq` of the event that created it.
    #[must_use]
    pub fn cards(&self) -> Vec<&Card> {
        self.tasks.values().collect()
    }

    #[must_use]
    pub fn card_at(&self, index: usize) -> Option<&Card> {
        self.tasks.values().nth(index)
    }

    #[must_use]
    pub fn feed(&self) -> &VecDeque<Line> {
        &self.feed
    }

    /// Wall clock of the first event folded, which is what the feed's run clock
    /// counts from.
    #[must_use]
    pub fn first_at_ms(&self) -> Option<i64> {
        self.first_at_ms
    }

    #[must_use]
    pub fn last_at_ms(&self) -> Option<i64> {
        self.last_at_ms
    }

    /// Review minutes and the number of changes they were spent on — W13's
    /// ladder, which is measured in human review minutes and never in
    /// agent-authored commits. Shown from the first milestone so the ladder has a
    /// baseline; nothing writes it yet, and an empty ladder says so honestly.
    #[must_use]
    pub fn review(&self) -> (u32, usize) {
        (self.review_seconds, self.reviews)
    }

    /// How long the whole run has been silent, against `now`.
    #[must_use]
    pub fn silence_ms(&self, now_ms: i64) -> Option<i64> {
        self.last_at_ms.map(|at| (now_ms - at).max(0))
    }

    /// The clock this card's state contract says to read.
    #[must_use]
    pub fn pulse(&self, card: &Card, now_ms: i64) -> Pulse {
        match card.state.contract().liveness {
            Liveness::None => Pulse::Untimed,
            Liveness::SinceEntered => Pulse::Since {
                ms: (now_ms - card.since_at_ms).max(0),
            },
            Liveness::Progress => {
                let ms = (now_ms - card.last_at_ms).max(0);
                Pulse::Progress {
                    ms,
                    stalled: ms > SILENCE_BAR_MS,
                }
            }
            Liveness::Human => Pulse::Waiting {
                ms: (now_ms - card.since_at_ms).max(0),
            },
        }
    }
}
