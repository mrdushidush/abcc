//! The rung reading, and the five claims underneath it that arithmetic alone
//! would not notice going wrong.
//!
//! 1. **The window is the most recent ten, never the best ten.** A reading that
//!    searched a history for its flattest run would report a milestone reached
//!    and stay reporting it while every recent review got worse.
//! 2. **One outlier does not flip the direction.** Theil–Sen is chosen over a
//!    least-squares line for exactly this, so the test computes the
//!    least-squares numerator too and asserts it points the *other* way — the
//!    property is worthless if it is never exercised against the thing it
//!    replaced.
//! 3. **The two estimators cannot disagree about direction**, because every
//!    x-gap is positive and so the pairwise slope signs *are* Mann–Kendall's
//!    terms. Asserted over random windows rather than argued in a comment.
//! 4. **The countable half is two clauses of four**, and the other two are named
//!    in the type so they cannot be forgotten by a printer.
//! 5. **A change nobody called a boundary crossing does not break the run** —
//!    it is not an M2 task, so it is not a break in them — but it is counted and
//!    handed over, because the stricter reading is a different sentence.
//!
//! 🚨 **Every number in here is exact.** There is no floating point in the
//! module, so a test may assert a slope and a p to the last digit, and does.

use abcc_core::climb::{Climb, Direction, Trend};
use abcc_core::replay::Ladder;

/// A ladder built from `(seconds, crossed_boundary)` pairs, one change each.
fn ladder(rows: &[(u32, bool)]) -> Ladder {
    let mut ladder = Ladder::default();
    for (i, (seconds, crossed)) in rows.iter().enumerate() {
        ladder.record(&format!("c{i}"), *seconds, "david", *crossed);
    }
    ladder
}

/// Ten changes, every one of them a boundary crossing, at the given seconds.
fn crossings(seconds: &[u32]) -> Ladder {
    let rows: Vec<(u32, bool)> = seconds.iter().map(|s| (*s, true)).collect();
    ladder(&rows)
}

#[test]
fn a_straight_line_reads_its_own_slope() {
    let up = Trend::read(&[10, 20, 30]).expect("three points is a trend");
    assert_eq!(up.slope.parts(), (10, 1));
    assert_eq!(up.direction(), Direction::Rising);
    assert!(!up.flat_or_falling(), "rising is not M3's clause 2");
    assert_eq!(up.slope.per_change_ms(), 10_000);

    let down = Trend::read(&[30, 20, 10]).expect("three points is a trend");
    assert_eq!(down.slope.parts(), (-10, 1));
    assert_eq!(down.direction(), Direction::Falling);
    assert!(down.flat_or_falling());
}

#[test]
fn flat_is_its_own_word_and_every_pair_is_tied() {
    let flat = Trend::read(&[5, 5, 5]).expect("three points is a trend");
    assert_eq!(flat.slope.parts(), (0, 1));
    assert_eq!(flat.direction(), Direction::Flat);
    assert!(flat.flat_or_falling(), "flat satisfies M3's clause 2");
    assert_eq!(flat.s, 0);
    assert_eq!(flat.tied_pairs, 3, "all three pairs are equal");
    // ⚠ Ties are the caveat on the p, not a bar to computing one: a window
    // that never moves is as unextreme as a window gets, so p is 1.
    assert_eq!(flat.p_milli(), Some(1000));
}

#[test]
fn fewer_than_two_points_is_an_absence_and_never_a_flat() {
    assert!(Trend::read(&[]).is_none());
    assert!(Trend::read(&[7]).is_none(), "one review is not a direction");
    assert!(
        Trend::read(&[7, 7]).is_some(),
        "two is the least that can be one"
    );
}

/// 🚨 Claim 2 — and it asserts the least-squares line points the *other* way,
/// because a robustness property that is never pointed at the thing it replaced
/// is a claim rather than a test.
#[test]
fn one_outlier_does_not_flip_a_falling_window() {
    let window: [u64; 10] = [100, 90, 80, 70, 60, 50, 40, 30, 20, 10_000];
    let trend = Trend::read(&window).expect("ten points is a trend");
    assert_eq!(
        trend.slope.parts(),
        (-10, 1),
        "the median pair is still -10 s"
    );
    assert_eq!(trend.direction(), Direction::Falling);

    // The least-squares slope has the sign of `n*Σxy - Σx*Σy`. One four-hour
    // review at the end drags it positive; the median of the pairwise slopes
    // does not move at all.
    let n = i64::try_from(window.len()).expect("ten");
    let sum_x: i64 = (0..n).sum();
    let sum_y: i64 = window
        .iter()
        .map(|y| i64::try_from(*y).expect("fits"))
        .sum();
    let cross_sum: i64 = (0..n)
        .zip(window.iter())
        .map(|(x, y)| x * i64::try_from(*y).expect("fits"))
        .sum();
    let least_squares = n * cross_sum - sum_x * sum_y;
    assert_eq!(least_squares, 441_300);
    assert!(
        least_squares > 0,
        "the line this estimator replaces reads RISING"
    );
}

#[test]
fn the_exact_p_of_a_monotone_triple_is_two_of_six() {
    let trend = Trend::read(&[1, 2, 3]).expect("three points is a trend");
    assert_eq!(trend.s, 3, "every pair rises");
    assert_eq!(
        trend.p,
        Some((2, 6)),
        "two of the six orderings are this extreme"
    );
    assert_eq!(trend.p_milli(), Some(333));
    assert_eq!(trend.tied_pairs, 0);
}

/// The denominator of the exact p is `n!` — the whole permutation space — so
/// checking it against the factorials checks the table the p is read off.
#[test]
fn the_exact_p_is_taken_over_every_ordering() {
    for (n, factorial) in [
        (2_usize, 2_u128),
        (3, 6),
        (4, 24),
        (5, 120),
        (6, 720),
        (8, 40_320),
    ] {
        let flat = vec![0_u64; n];
        let trend = Trend::read(&flat).expect("two or more points is a trend");
        let (favourable, total) = trend.p.expect("inside the exact range");
        assert_eq!(total, factorial, "n = {n}");
        assert_eq!(
            favourable, factorial,
            "a window that never moves is never extreme"
        );
    }
    // A strictly rising window of ten is the most extreme of the 3,628,800
    // orderings, and so is its mirror — which rounds below a thousandth.
    let rising: Vec<u64> = (1..=10).collect();
    let trend = Trend::read(&rising).expect("ten points is a trend");
    assert_eq!(trend.s, 45, "all forty-five pairs rise");
    assert_eq!(trend.p, Some((2, 3_628_800)));
    assert_eq!(
        trend.p_milli(),
        Some(0),
        "below a thousandth, and the caller must say so"
    );
}

/// ⚠ Above the exact range there is no p rather than an approximate one.
#[test]
fn a_window_too_wide_for_an_exact_p_gets_none_and_not_a_guess() {
    let wide: Vec<u64> = (0..=u64::try_from(Trend::EXACT_TO).expect("small")).collect();
    let trend = Trend::read(&wide).expect("wide is still a trend");
    assert!(trend.p.is_none(), "no p above EXACT_TO");
    assert!(trend.p_milli().is_none());
    assert_eq!(
        trend.direction(),
        Direction::Rising,
        "but the direction is still exact"
    );
}

#[test]
fn an_empty_ladder_climbs_nothing_and_says_so() {
    let climb = Ladder::default().climb();
    assert_eq!(climb.crossing, 0);
    assert_eq!(climb.have(), 0);
    assert_eq!(climb.short_by(), Climb::WANT);
    assert!(climb.trend.is_none());
    assert!(!climb.countable_half());
}

#[test]
fn ten_falling_crossings_are_the_countable_half() {
    let climb = crossings(&[1000, 900, 800, 700, 600, 500, 400, 300, 200, 100]).climb();
    assert_eq!(climb.crossing, 10);
    assert_eq!(climb.have(), 10);
    assert_eq!(climb.short_by(), 0);
    assert_eq!(climb.interleaved, 0);
    let trend = climb.trend.expect("ten points is a trend");
    assert_eq!(trend.slope.parts(), (-100, 1));
    assert_eq!(trend.direction(), Direction::Falling);
    assert!(climb.countable_half());
}

/// 🚨 Claim 1. The same ladder holds a ten-change falling run and the reading
/// still refuses, because the run is no longer the *recent* ten.
#[test]
fn the_window_is_the_most_recent_ten_and_never_the_best_ten() {
    let mut seconds = vec![1000, 900, 800, 700, 600, 500, 400, 300, 200, 100];
    assert!(
        crossings(&seconds).climb().countable_half(),
        "the first ten fall"
    );

    seconds.extend_from_slice(&[5000, 6000, 7000, 8000, 9000]);
    let climb = crossings(&seconds).climb();
    assert_eq!(
        climb.crossing, 15,
        "every one of them still crossed a boundary"
    );
    assert_eq!(climb.have(), 10, "but only ten are in the window");
    assert_eq!(
        climb.window,
        vec![500, 400, 300, 200, 100, 5000, 6000, 7000, 8000, 9000],
        "the tail, oldest first"
    );
    assert_eq!(climb.trend.expect("a trend").direction(), Direction::Rising);
    assert!(
        !climb.countable_half(),
        "a falling run earlier in the history does not survive five bad reviews"
    );
}

/// 🚨 Claim 5 — and the count that lets a reader apply the stricter reading.
#[test]
fn a_change_that_crossed_nothing_does_not_break_the_run_but_is_counted() {
    let climb = ladder(&[
        (500, true),
        (400, true),
        (60, false),
        (300, true),
        (200, true),
        (100, true),
    ])
    .climb();
    assert_eq!(
        climb.crossing, 5,
        "the non-crossing change is not an M2 task"
    );
    assert_eq!(
        climb.window,
        vec![500, 400, 300, 200, 100],
        "and is not in the window"
    );
    assert_eq!(
        climb.interleaved, 1,
        "but it is reported, for the stricter reading"
    );
    assert_eq!(climb.short_by(), 5);
    assert!(
        !climb.countable_half(),
        "five is not ten, however well they fall"
    );
}

#[test]
fn a_change_reviewed_twice_is_one_row_at_the_sum_of_its_passes() {
    let mut ladder = Ladder::default();
    ladder.record("c0", 600, "david", true);
    ladder.record("c0", 300, "hadar", true);
    let climb = ladder.climb();
    assert_eq!(
        climb.crossing, 1,
        "two passes over one change are one change"
    );
    assert_eq!(
        climb.window,
        vec![900],
        "and the ladder's unit carries both"
    );
}

/// ⚠ The boundary flag is sticky in the true direction, so a second reviewer
/// leaving it off cannot take a change out of the M2 count.
#[test]
fn a_second_reviewer_cannot_uncross_a_boundary() {
    let mut ladder = Ladder::default();
    ladder.record("c0", 600, "david", true);
    ladder.record("c0", 300, "hadar", false);
    assert_eq!(ladder.climb().crossing, 1);
}

/// 🚨 Claim 3, as a property rather than a comment. `direction_is_undisputed`
/// is a predicate that must never be false, and a future estimator swap has to
/// argue with this test.
#[test]
fn the_two_estimators_can_never_point_opposite_ways() {
    // A fixed xorshift, so a failure is a failure anybody can reproduce from
    // the seed in this file and nothing depends on a dev-dependency.
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut checked = 0_u32;
    for _ in 0..2_000 {
        let n = 2 + usize::try_from(next() % 11).expect("small");
        // Small ranges on purpose: ties are where a disagreement would hide.
        let window: Vec<u64> = (0..n).map(|_| next() % 6).collect();
        let trend = Trend::read(&window).expect("two or more points is a trend");
        assert!(
            trend.direction_is_undisputed(),
            "slope {:?} against S {} on {window:?}",
            trend.slope.parts(),
            trend.s
        );
        checked += 1;
    }
    assert_eq!(checked, 2_000);
}

/// 🚨 Claim 4. The half that is countable is two clauses of four, and the two
/// that are not are carried in the type — so a printer that says *M3 reached*
/// has to delete this list to do it.
#[test]
fn the_countable_half_names_the_half_it_is_not() {
    let missing = Climb::cannot_say();
    assert_eq!(
        missing.len(),
        2,
        "M3 has four clauses and two have no writer"
    );
    assert!(
        missing.iter().any(|m| m.contains("human edited")),
        "clause 3 must stay named"
    );
    assert!(
        missing.iter().any(|m| m.contains("30 and 90 days")),
        "clause 4 must stay named"
    );
    // A full, falling window is the most this reading may ever claim.
    let climb = crossings(&[1000, 900, 800, 700, 600, 500, 400, 300, 200, 100]).climb();
    assert!(
        climb.countable_half(),
        "two clauses of four, and it is named that"
    );
}

/// A window asked for at another width, which is the seam `abcc replay` would
/// use to show a shorter run without the reading pretending it is M3's.
#[test]
fn a_caller_may_ask_for_a_window_other_than_ten() {
    let ladder = crossings(&[900, 800, 700, 600, 500, 400, 300, 200, 100, 50, 25]);
    let five = Climb::read(&ladder.changes, 5);
    assert_eq!(five.want, 5);
    assert_eq!(five.window, vec![300, 200, 100, 50, 25]);
    assert_eq!(five.short_by(), 0);
    assert!(
        five.countable_half(),
        "and the half it is, is this window's"
    );
    assert_eq!(
        five.crossing, 11,
        "the count is of the ladder, not of the window"
    );
}

/// ⚠ The display helper truncates toward zero and the criterion does not.
#[test]
fn a_slope_that_rounds_to_zero_is_still_falling() {
    // Three points whose median pairwise slope is -1/2 s per change.
    let trend = Trend::read(&[1, 1, 0]).expect("three points is a trend");
    assert_eq!(trend.slope.parts(), (-1, 2));
    assert_eq!(
        trend.direction(),
        Direction::Falling,
        "the sign is the criterion"
    );
    assert_eq!(trend.slope.per_change_ms(), -500);
}
