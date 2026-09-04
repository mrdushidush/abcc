//! The roster: who is on the field, where they stand, and what the picture
//! cannot say.
//!
//! 🚨 **All of this is the mapping, and the mapping is testable without eyes.**
//! *Does it look right* is the operator's; *is the right unit standing on the
//! right cell* is arithmetic, and every claim ADR-0012 makes about the field
//! being wired to the log is one of the assertions below.

use abcc_core::attempt::Cause;
use abcc_core::event::Event;
use abcc_core::run::Mode;
use abcc_core::seq::{AttemptId, CheckpointId, MissionId, PromptId, Seq, TaskId, UnitId};
use abcc_core::task::{AbortReason, Command, TaskState, Watchdog};
use abcc_tui::assets::{Design, Facing};
use abcc_tui::roster::{Post, RANK, Roster, Standing};
use abcc_tui::view::View;
use abcc_tui::{Feed, Replay, Theme};

// ---------------------------------------------------------------------------
// building a board
// ---------------------------------------------------------------------------

/// A log under construction, with one mission and whatever tasks a test wants.
struct Board {
    log: Replay,
    mission: MissionId,
}

impl Board {
    fn new() -> Board {
        let mut log = Replay::new();
        log.push(Event::RunStarted {
            mode: Mode::SinglePlayer,
            version: "0.1.0".to_string(),
            pid: 1,
        });
        let mission = MissionId::at(log.push(Event::MissionCreated {
            title: "Console".to_string(),
        }));
        Board { log, mission }
    }

    /// A task, created queued.
    fn task(&mut self, title: &str) -> TaskId {
        TaskId::at(self.log.advance(1).push(Event::TaskCreated {
            mission: self.mission,
            title: title.to_string(),
            prompt: format!("do {title}"),
        }))
    }

    /// Move a task, the way the store does: allocate the seq, then transition.
    fn move_to(&mut self, task: TaskId, from: TaskState, command: Command) -> TaskState {
        let at = self.log.advance(1).next_seq();
        let to = from.apply(&command, at).expect("a legal transition");
        self.log.push(Event::TaskTransitioned {
            task,
            command,
            from,
            to: to.clone(),
        });
        to
    }

    /// Deploy a task into a slot and start its attempt: the two steps that put a
    /// task on the line, and the only place the slot is ever named.
    fn engage(&mut self, task: TaskId, unit: UnitId) -> TaskState {
        let deployed = self.move_to(task, TaskState::Queued, Command::Deploy { unit });
        let attempt = AttemptId::at(self.log.advance(1).push(Event::AttemptStarted {
            task,
            unit,
            cause: Cause::Fresh,
            checkpoint_from: None,
        }));
        self.move_to(task, deployed, Command::Engage { attempt })
    }

    /// Everything from the origin, folded.
    fn view(&self) -> View {
        self.view_from(Seq::ORIGIN)
    }

    /// Folded from a position, so a test can open a window part-way through a
    /// run the way a console opened late does.
    fn view_from(&self, since: Seq) -> View {
        let mut view = View::new(Theme::Command);
        view.fold_all(&self.log.read_from(since, 4_096).expect("read"));
        view
    }
}

/// Every state, built with placeholder evidence, so a test can walk the whole
/// enum rather than the three variants that were convenient.
fn every_state() -> Vec<TaskState> {
    let (seq, attempt) = (Seq::new(7), AttemptId::at(Seq::new(5)));
    vec![
        TaskState::Queued,
        TaskState::Deployed {
            unit: UnitId(1),
            since: seq,
        },
        TaskState::Engaged {
            attempt,
            since: seq,
        },
        TaskState::AwaitingOrders {
            attempt,
            prompt: PromptId::at(Seq::new(6)),
            since: seq,
        },
        TaskState::Holding {
            checkpoint: CheckpointId::at(Seq::new(6)),
            since: seq,
        },
        TaskState::Commandeered {
            operator_since: seq,
        },
        TaskState::Accomplished { attempt },
        TaskState::Failed { attempt },
        TaskState::Aborted {
            reason: AbortReason::Operator {
                by: "operator".to_string(),
            },
            since: seq,
        },
    ]
}

// ---------------------------------------------------------------------------
// 🚨 the rank, against the contract that already decides everything else
// ---------------------------------------------------------------------------

/// 🚨🚨 **The two derivations agree, on all nine states.**
///
/// `Post::of` is an exhaustive `match`, which makes a tenth `TaskState`
/// impossible to ship without deciding where it stands — but exhaustiveness only
/// forces an *answer*, never the right one. So the answer is checked against
/// [`StateContract`], which was written for the watchdog and knows nothing about
/// a battlefield:
///
/// * `terminal` ⇔ off the field
/// * `holds_slot` ⇔ [`Post::Line`]
/// * `Watchdog::WatchNeverReap` ⇔ [`Post::Waiting`]
///
/// The third is the one worth having. The operator's rank stands nearest the
/// camera because those states cannot resolve themselves, and *cannot resolve
/// itself* is `WatchNeverReap` — so if somebody later moves a state between
/// ranks for how it looks, this fails and says which promise it broke.
#[test]
fn where_a_task_stands_agrees_with_the_contract_it_already_has() {
    for state in every_state() {
        let contract = state.contract();
        let post = Post::of(&state, Some(UnitId(0)));
        let name = state.name();

        assert_eq!(
            post.is_none(),
            contract.terminal,
            "{name}: terminal and on the field disagree"
        );
        assert_eq!(
            matches!(post, Some(Post::Line { .. })),
            contract.holds_slot,
            "{name}: holds a slot but does not stand on the line, or the reverse"
        );
        assert_eq!(
            post == Some(Post::Waiting),
            contract.watchdog == Watchdog::WatchNeverReap,
            "{name}: the operator's rank is exactly the states nothing but a person moves"
        );
    }
}

/// The ranks are ordered back to front, and the operator's is the front one.
#[test]
fn the_operators_rank_is_nearest_the_camera() {
    let depths: Vec<i32> = Post::ALL.iter().map(|p| p.depth()).collect();
    assert_eq!(depths, vec![-1, 2, 4, 6], "the ranks moved");
    assert!(
        Post::Waiting.depth() > Post::Line { slot: None }.depth(),
        "the rank nothing but a person can move is not in front"
    );
    // A bigger `cx + cy` is drawn later, which is what puts it in front.
    assert!(Post::Waiting.depth() > Post::Base.depth());
}

/// 🚨 **A building never stands in the same screen column as a unit.**
///
/// Screen x is `cx - cy`, which is `2i - depth` on a rank — so ranks of the same
/// depth parity occupy the same columns and the one in front covers the one
/// behind. Decoding a painted frame is what found it: the base at an even depth
/// put the only building exactly behind a queued unit, and the decoder returned
/// the two as one region. The base's depth is odd for this reason and this test
/// is what says so.
#[test]
fn a_building_is_never_directly_behind_a_unit() {
    let mut board = Board::new();
    for i in 0..6 {
        board.task(&format!("queued {i}"));
    }
    let roster = Roster::muster(&board.view());

    let screen_x = |p: &abcc_tui::roster::Placed| p.cell.0 - p.cell.1;
    let buildings: Vec<i32> = roster.rank(Post::Base).map(screen_x).collect();
    let units: Vec<i32> = roster
        .placed()
        .iter()
        .filter(|p| !p.post.same_rank(Post::Base))
        .map(screen_x)
        .collect();
    assert!(!buildings.is_empty() && units.len() == 6);
    for b in &buildings {
        assert!(
            !units.contains(b),
            "a building at screen column {b} is hidden behind a unit: {units:?}"
        );
    }
    // And it stands more than one rank back, or the interleave buys nothing: a
    // gap of `tile/4` under a figure a whole tile tall is no gap at all.
    assert!(
        Post::Reserve.depth() - Post::Base.depth() >= 3,
        "the base is too close to the rank in front of it"
    );
}

/// 🚨 **A rank is a line of constant depth, not a row of one `cy`.**
///
/// Every cell on a rank sums to that rank's depth, which is what puts its units
/// side by side instead of receding one behind the other — 134 px apart under a
/// 100 px figure on this project's own log, against 60 px for a row. Break this
/// and the picture still draws; it just quietly goes back to a huddle.
#[test]
fn every_cell_on_a_rank_sums_to_that_ranks_depth() {
    let mut board = Board::new();
    for i in 0..3 {
        board.task(&format!("queued {i}"));
    }
    let a = board.task("a");
    let b = board.task("b");
    board.engage(a, UnitId(0));
    board.engage(b, UnitId(2));

    let roster = Roster::muster(&board.view());
    assert!(roster.len() >= 6, "the fixture stopped filling the ranks");
    for placed in roster.placed() {
        assert_eq!(
            placed.cell.0 + placed.cell.1,
            placed.post.depth(),
            "{:?} is off its rank at {:?}",
            placed.what,
            placed.cell
        );
    }

    // And consecutive units on one rank are two apart in screen x, which is a
    // whole tile rather than half of one.
    let xs: Vec<i32> = roster
        .rank(Post::Reserve)
        .map(|p| p.cell.0 - p.cell.1)
        .collect();
    assert!(xs.windows(2).all(|w| w[1] - w[0] == 2), "{xs:?}");
}

// ---------------------------------------------------------------------------
// 🚨 positioned by slot, which is a fact about the log and not about the state
// ---------------------------------------------------------------------------

/// 🚨🚨 **`Engaged` does not name its slot, so the fold has to remember it.**
///
/// This is the whole reason the roster takes a [`View`] rather than a list of
/// states: `Deployed { unit }` is the only variant that names a unit, and a task
/// leaves that state the moment its attempt starts. Delete the slot-carrying
/// line in `View::project` and this fails — the task lands to the *left* of slot
/// zero, where the roster puts a unit whose slot it does not know.
#[test]
fn a_running_task_stands_on_the_slot_the_log_granted_it() {
    let mut board = Board::new();
    let t = board.task("engage");
    board.engage(t, UnitId(1));

    let view = board.view();
    let card = view.card_of(t).expect("the task");
    assert_eq!(card.slot, Some(UnitId(1)), "the fold lost the slot");
    assert!(
        matches!(card.state, TaskState::Engaged { .. }),
        "the fixture no longer leaves the task engaged: {}",
        card.state.name()
    );

    let roster = Roster::muster(&view);
    let placed = roster.placed();
    let unit = placed
        .iter()
        .find(|p| p.what == Standing::Task(t))
        .expect("on the field");
    assert_eq!(unit.cell, Post::Line { slot: None }.cell(1));
}

/// 🚨 **A gap in the line stays a gap.**
///
/// `cx` is the slot number and not an index into the occupied ones, because the
/// value of positioning by slot is that slot zero is always the same place. Two
/// tasks in slots 0 and 3 stand three columns apart with nothing between them —
/// and a roster that packed them would put slot 3 where slot 1 belongs.
#[test]
fn the_line_is_positioned_by_slot_and_not_by_arrival() {
    let mut board = Board::new();
    let far = board.task("slot three");
    let near = board.task("slot zero");
    board.engage(far, UnitId(3));
    board.engage(near, UnitId(0));

    let roster = Roster::muster(&board.view());
    let column = |task: TaskId| {
        roster
            .placed()
            .iter()
            .find(|p| p.what == Standing::Task(task))
            .expect("on the field")
            .cell
            .0
    };
    assert_eq!((column(near), column(far)), (0, 3));
}

/// 🚨 **A window that opened late says it does not know, rather than guessing.**
///
/// A console started beside a run already in flight never sees the `Deploy`, so
/// the slot is genuinely unknown. Standing the unit at `UnitId(0)` would put it
/// on top of whatever really is in slot zero — a picture that is wrong in a way
/// the operator cannot see. It stands to the *left* of the numbered slots
/// instead, on the line, because that is where the log says it is.
#[test]
fn a_reader_that_arrived_late_stands_the_unit_off_the_numbered_slots() {
    let mut board = Board::new();
    let known = board.task("granted in this window");
    let late = board.task("granted before it");
    // Deploy and start the attempt, then open the window: the transition that
    // makes it `Engaged` is inside, and the grant that named the slot is not.
    let deployed = board.move_to(late, TaskState::Queued, Command::Deploy { unit: UnitId(0) });
    let attempt = AttemptId::at(board.log.advance(1).push(Event::AttemptStarted {
        task: late,
        unit: UnitId(0),
        cause: Cause::Fresh,
        checkpoint_from: None,
    }));
    let after_the_grant = board.log.head();
    board.move_to(late, deployed, Command::Engage { attempt });
    board.engage(known, UnitId(1));

    let view = board.view_from(after_the_grant);
    assert_eq!(
        view.card_of(late).expect("still on the board").slot,
        None,
        "the window saw a grant it should not have"
    );

    let roster = Roster::muster(&view);
    let cell = |task: TaskId| {
        roster
            .placed()
            .iter()
            .find(|p| p.what == Standing::Task(task))
            .expect("on the field")
            .cell
    };
    let line = Post::Line { slot: None };
    assert_eq!(
        cell(late),
        line.cell(-1),
        "an unplaced unit took a real slot"
    );
    assert_eq!(cell(known), line.cell(1));
}

/// 🚨 **Two tasks on one slot are counted, because the field draws them
/// stacked.**
///
/// A slot holds one task at a time on a healthy log. After a crash it does not:
/// only `abcc run` calls `Store::boot`, so a console opened beside a dead run
/// reads both as slot-holding and the depth sort hides one behind the other.
/// Counting it is what keeps two units from looking like one.
#[test]
fn two_tasks_claiming_one_slot_are_counted_rather_than_hidden() {
    let mut board = Board::new();
    let first = board.task("the orphan");
    let second = board.task("the live one");
    board.engage(first, UnitId(0));
    board.engage(second, UnitId(0));

    let roster = Roster::muster(&board.view());
    assert_eq!(roster.contested(), 1, "a stacked slot went unreported");
    assert_eq!(roster.len(), 3, "both units and the base are on the field");
}

// ---------------------------------------------------------------------------
// the board, folded into a field
// ---------------------------------------------------------------------------

/// The whole mapping in one board: a mission, one of each live rank, and a
/// finished task that is not drawn at all.
#[test]
fn every_live_task_gets_a_unit_and_a_finished_one_does_not() {
    let mut board = Board::new();
    let queued = board.task("standing by");
    let running = board.task("engaging");
    let asking = board.task("intervention");
    let done = board.task("finished");

    board.engage(running, UnitId(0));
    let engaged = board.engage(asking, UnitId(1));
    let attempt = engaged.attempt_in_flight().expect("an attempt");
    board.move_to(
        asking,
        engaged,
        Command::RequestOrders {
            attempt,
            prompt: PromptId::at(Seq::new(1)),
        },
    );
    let ended = board.engage(done, UnitId(0));
    let attempt = ended.attempt_in_flight().expect("an attempt");
    board.move_to(done, ended, Command::Fail { attempt });

    let roster = Roster::muster(&board.view());
    assert_eq!(roster.off(), 1, "the finished task is off the field");
    assert_eq!(roster.crowded(), 0);

    let post_of = |task: TaskId| {
        roster
            .placed()
            .iter()
            .find(|p| p.what == Standing::Task(task))
            .map(|p| p.post)
    };
    assert_eq!(post_of(queued), Some(Post::Reserve));
    assert_eq!(
        post_of(running),
        Some(Post::Line {
            slot: Some(UnitId(0))
        })
    );
    assert_eq!(post_of(asking), Some(Post::Waiting));
    assert_eq!(post_of(done), None, "a finished task was drawn");

    // One building, for the one mission, standing behind everything.
    let base: Vec<_> = roster.rank(Post::Base).collect();
    assert_eq!(base.len(), 1);
    assert_eq!(base[0].what, Standing::Mission(board.mission));
    assert_eq!(base[0].pose.design, Design::Building);
    let depth = |cell: (i32, i32)| cell.0 + cell.1;
    assert!(depth(base[0].cell) < depth(post_cell(&roster, queued)));
}

/// The cell of a task on the field.
fn post_cell(roster: &Roster, task: TaskId) -> (i32, i32) {
    roster
        .placed()
        .iter()
        .find(|p| p.what == Standing::Task(task))
        .expect("on the field")
        .cell
}

/// A mission is a building and a task is a unit — the one thing the design axis
/// carries, and the reason the field has a subject rather than a row of clones.
#[test]
fn a_mission_is_a_building_and_a_task_is_a_coder() {
    let mut board = Board::new();
    board.task("one");
    let roster = Roster::muster(&board.view());

    for placed in roster.placed() {
        let expected = match placed.what {
            Standing::Mission(_) => Design::Building,
            Standing::Task(_) => Design::Coder,
        };
        assert_eq!(placed.pose.design, expected);
    }
    assert_eq!(roster.len(), 2, "a mission and its one task");
}

/// 🚨 **A rank is bounded, and says how many it did not draw.**
///
/// F112's argument, one layer up: the queue has no ceiling and a rank with no
/// limit shrinks the tile until every unit is behind its neighbour. The picture
/// stays readable and the count is what stops *six units* from being read as
/// *the whole board*.
#[test]
fn a_rank_draws_at_most_six_and_reports_the_rest() {
    let mut board = Board::new();
    for i in 0..10 {
        board.task(&format!("queued {i}"));
    }
    let roster = Roster::muster(&board.view());

    assert_eq!(roster.rank(Post::Reserve).count(), RANK);
    assert_eq!(roster.crowded(), 10 - RANK);
    // Left to right, in task order, with no gaps.
    let columns: Vec<i32> = roster.rank(Post::Reserve).map(|p| p.cell.0).collect();
    assert_eq!(columns, (0..6).collect::<Vec<_>>());
}

/// A slot past the field's width is crowded out rather than drawn off the edge.
#[test]
fn a_slot_wider_than_the_field_is_reported_rather_than_clipped() {
    let mut board = Board::new();
    let far = board.task("slot nine");
    board.engage(far, UnitId(9));

    let roster = Roster::muster(&board.view());
    assert_eq!(roster.rank(Post::Line { slot: None }).count(), 0);
    assert_eq!(roster.crowded(), 1);
}

/// 🚨 **An empty field is an answer, not a failure.**
///
/// A board whose tasks have all finished draws nothing, and that is the fleet
/// being quiet. It has to be distinguishable from a board with nothing on it at
/// all, or *the console is broken* and *there is no work* look the same.
#[test]
fn a_quiet_fleet_and_an_empty_board_are_different_empty_fields() {
    let empty = Roster::muster(&View::new(Theme::Command));
    assert!(empty.is_empty());
    assert_eq!(empty.off(), 0, "an empty board has finished nothing");

    let mut board = Board::new();
    let done = board.task("finished");
    let engaged = board.engage(done, UnitId(0));
    let attempt = engaged.attempt_in_flight().expect("an attempt");
    board.move_to(done, engaged, Command::Fail { attempt });

    let quiet = Roster::muster(&board.view());
    assert!(quiet.is_empty(), "a finished task is still on the field");
    assert_eq!(quiet.off(), 1, "a quiet fleet cannot say what it finished");
}

/// The formation faces away from the middle of the field: the left flank looks
/// west, the right flank looks east. Geometry, not state — the corpus has two
/// facings and nine states, and doubling one onto the other would be inventing a
/// distinction the art cannot carry.
#[test]
fn the_formation_faces_outward() {
    let mut board = Board::new();
    for i in 0..5 {
        board.task(&format!("queued {i}"));
    }
    let roster = Roster::muster(&board.view());

    let reserve: Vec<_> = roster.rank(Post::Reserve).collect();
    assert_eq!(reserve.len(), 5);
    // Five in a row: the outer two on each side look outward, and nothing on the
    // left flank looks the same way as anything on the right.
    assert_eq!(reserve[0].pose.facing, Facing::West, "the left flank");
    assert_eq!(reserve[4].pose.facing, Facing::East, "the right flank");
    let west = reserve
        .iter()
        .filter(|p| p.pose.facing == Facing::West)
        .count();
    assert!(
        west > 0 && west < 5,
        "every unit faced the same way: {west} of 5 west"
    );
}

/// Cells are unique across the field except where the roster says they are not,
/// so nothing is silently drawn on top of anything else.
#[test]
fn nothing_shares_a_cell_unless_it_was_counted() {
    let mut board = Board::new();
    board.task("queued");
    let a = board.task("a");
    let b = board.task("b");
    board.engage(a, UnitId(0));
    board.engage(b, UnitId(1));

    let roster = Roster::muster(&board.view());
    let mut cells = roster.cells();
    let before = cells.len();
    cells.sort_unstable();
    cells.dedup();
    assert_eq!(cells.len(), before, "two things stood on one cell");
    assert_eq!(roster.contested(), 0);
}
