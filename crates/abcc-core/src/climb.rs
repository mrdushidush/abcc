//! W13 §7's rungs, read off the ladder — **the arithmetic M0–M3 were defined
//! in and never given.**
//!
//! `research/W13-codev.md` §7 promises *four rungs, each strictly harder, each
//! countable from the event log W3 already specifies*. [`Ladder`] has held the
//! rows since Skeleton and `abcc review` has written them since Skeleton too;
//! what was missing was the reading. `grep -rn consecutive crates/` found the
//! word in doc comments and **nowhere else**, so *which rung is this project
//! on?* was a question the archive could not answer about its own eleven
//! thousand events — a measurement with a writer, a line and no reader, which
//! is F718 one layer up.
//!
//! # 🚨 This module reads ONE rung, and the refusal is the point
//!
//! M3 is four clauses, and they are not equally knowable:
//!
//! | # | clause | on the log? |
//! |---|---|---|
//! | 1 | **ten consecutive M2 tasks** | ✅ [`Reviewed::crossed_boundary`] |
//! | 2 | **review minutes per merged change flat or falling** | ✅ [`Reviewed::seconds`] |
//! | 3 | **no human edit to the agent's diff** | ❌ nothing writes it |
//! | 4 | **surviving defects tracked at 30 and 90 days** | ❌ OQ-W13-3, unspecified |
//!
//! Clauses 1 and 2 are arithmetic over rows that exist. Clauses 3 and 4 have
//! **no writer anywhere in the workspace**. So [`Climb`] computes *M3's
//! countable half*, and [`Climb::countable_half`] is named that way so the
//! caller cannot spell it `reached_m3` by accident. A reading that returned M3
//! from two clauses of four would be the most expensive kind of wrong: **the
//! milestone claimed by the instrument built to judge it.**
//!
//! # The three choices this reading makes, stated rather than buried
//!
//! 1. 🚨 **The window is the most recent ten and never the best ten.** Searching
//!    a history for its flattest run is choosing the answer — the same defect as
//!    quoting one sortie's `apply_patch` rate out of eleven (F657). The trailing
//!    window can only get worse when a bad review lands, which is the direction
//!    a milestone test has to be able to move in.
//! 2. ⚠ **The order is the order reviews were recorded**, because it is the only
//!    order [`Ladder`] has. It is merge order exactly when changes are reviewed
//!    in the order they land, and nothing enforces that. [`Landed::seq`](crate::replay::Landed::seq) gives
//!    landing order for changes that came through `abcc land`, and the day every
//!    row on the ladder has one this should be re-read against it.
//! 3. ⚠ **A change that crossed no boundary does not break the run**, because
//!    M3 counts *M2 tasks* and a change that is not one is not a break in them.
//!    The strict reading — *anything at all in between resets it* — is a
//!    different sentence, so [`Climb::interleaved`] hands the caller its input
//!    rather than the reading picking one silently.
//!
//! ⚠ **What a module boundary is, is still OQ-W13-1** — `--boundary` is an
//! operator switch with no written rule behind it, so clause 1 is only as sharp
//! as the flag a person typed. This module counts the flag; it does not define
//! it.
//!
//! # Why two estimators
//!
//! *Flat or falling* is a claim about direction, and ten noisy points support
//! one weakly. [`Trend::slope`] is Theil–Sen — the median of the pairwise
//! slopes — because it is **in the ladder's own unit** (seconds per change, so
//! the criterion is read off the quantity the criterion names) and because one
//! four-hour review in the middle of ten does not flip it, where a least-squares
//! line would. [`Trend::p`] is Mann–Kendall's exact two-sided p beside it, as a
//! **caution and never a gate**: M3's criterion is the descriptive one, and
//! turning it into a significance test would be this module rewriting the
//! milestone it was built to measure.
//!
//! 🚨 **There is no floating point in here.** Every slope is an exact rational
//! and every p is a count over a count, so the reading is the same on every
//! platform and a test can assert the whole of it.

use crate::replay::{Ladder, Reviewed};

/// Which way the review minutes are going.
///
/// ⚠ **Three words and not a `bool`**, for the crate's third rule: *flat* and
/// *falling* both satisfy M3 and they are not the same fact about a project.
/// Collapsing them would throw away the only one of the two that says the
/// burden has stopped moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Minutes per change are going down.
    Falling,
    /// Exactly flat. Reachable because the arithmetic is exact.
    Flat,
    /// Minutes per change are going up — M3's criterion is not met.
    Rising,
}

impl Direction {
    /// The word a console prints.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Direction::Falling => "falling",
            Direction::Flat => "flat",
            Direction::Rising => "rising",
        }
    }

    /// M3's clause 2, as the one predicate it is.
    #[must_use]
    pub const fn flat_or_falling(self) -> bool {
        matches!(self, Direction::Falling | Direction::Flat)
    }
}

/// A slope in **seconds per change**, held exactly as a rational.
///
/// 🚨 Kept as a fraction rather than rounded on construction because the *sign*
/// is the criterion. A Theil–Sen median over ten points is routinely a rational
/// like `-7/2`, and a slope of `-1/3` rounds to a integer zero that would print
/// as *flat* and read as *the burden has stopped falling* — a wrong answer to
/// the only question the number is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slope {
    num: i128,
    /// Always strictly positive, so the sign lives entirely in `num`.
    den: i128,
}

impl Slope {
    /// Reduced, with the sign carried by the numerator. `den` of zero is not
    /// constructible here: every caller divides by a difference of distinct
    /// indices.
    fn new(num: i128, den: i128) -> Self {
        let (num, den) = if den < 0 { (-num, -den) } else { (num, den) };
        let g = gcd(num.abs(), den);
        if g == 0 {
            return Slope { num: 0, den: 1 };
        }
        Slope {
            num: num / g,
            den: den / g,
        }
    }

    /// The exact fraction, numerator then denominator.
    #[must_use]
    pub const fn parts(self) -> (i128, i128) {
        (self.num, self.den)
    }

    /// Which way it points. This is the criterion.
    #[must_use]
    pub const fn direction(self) -> Direction {
        if self.num < 0 {
            Direction::Falling
        } else if self.num > 0 {
            Direction::Rising
        } else {
            Direction::Flat
        }
    }

    /// Milliseconds per change, truncated toward zero — **for display only**.
    ///
    /// ⚠ A slope of `-1/3` s per change truncates to `-333` ms here and a slope
    /// of `-1/10000` truncates to `0`. Read [`Slope::direction`] for the
    /// criterion and this for the size; a printer that decided *flat* from a
    /// zero here would be reading the rounding.
    #[must_use]
    pub fn per_change_ms(self) -> i64 {
        let ms = self.num * 1000 / self.den;
        // Saturating rather than wrapping, and unreachable for any window a
        // person could review: a slope this large is hours per change.
        i64::try_from(ms).unwrap_or(if ms < 0 { i64::MIN } else { i64::MAX })
    }
}

/// Greatest common divisor, for reducing a slope. Both arguments non-negative
/// except that `a` may be any magnitude; `b` is a positive denominator.
const fn gcd(mut a: i128, mut b: i128) -> i128 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// The trend over one window of review seconds — a direction, a size, and how
/// much ten points can be asked to support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trend {
    /// Theil–Sen's median pairwise slope, in seconds per change. Exact.
    pub slope: Slope,
    /// Mann–Kendall's `S`: concordant pairs minus discordant ones.
    pub s: i64,
    /// Pairs of the window that are equal. ⚠ [`Trend::p`] is the exact
    /// distribution **for distinct values**; a window with ties is not drawn
    /// from it, so a non-zero count here is the caveat on the p beside it.
    pub tied_pairs: usize,
    /// Exact two-sided p as `(favourable, total)` — the permutations of the
    /// window at least as extreme as this one, over all of them. `None` above
    /// [`Trend::EXACT_TO`], where the total stops fitting.
    pub p: Option<(u128, u128)>,
}

impl Trend {
    /// The largest window an exact p is computed for. `30!` is about `2.65e32`
    /// and a `u128` holds `3.4e38`; the reading's own window is ten.
    pub const EXACT_TO: usize = 30;

    /// Read the trend over `window`, oldest first. `None` for fewer than two
    /// points — **an absence rather than a flat**, because one review is not a
    /// direction and reporting it as one is how a ladder starts lying early.
    #[must_use]
    pub fn read(window: &[u64]) -> Option<Self> {
        let slope = theil_sen(window)?;
        let (s, tied_pairs) = mann_kendall(window);
        Some(Trend {
            slope,
            s,
            tied_pairs,
            p: exact_p(window.len(), s),
        })
    }

    /// Which way the minutes go. [`Slope`]'s, and the criterion.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.slope.direction()
    }

    /// M3's clause 2 over this window.
    #[must_use]
    pub const fn flat_or_falling(&self) -> bool {
        self.slope.direction().flat_or_falling()
    }

    /// The p rounded to thousandths, for printing. `None` when there is no
    /// exact p to round.
    #[must_use]
    pub fn p_milli(&self) -> Option<u32> {
        let (favourable, total) = self.p?;
        if total == 0 {
            return None;
        }
        u32::try_from((favourable * 1000 + total / 2) / total).ok()
    }

    /// 🚨 **The two estimators cannot disagree about direction, and this is
    /// where that is written down.**
    ///
    /// I built a `estimators_disagree` predicate for the console to print, and
    /// it is a branch that can never be taken. Every x-gap in a window is a
    /// positive index difference, so the *sign* of each pairwise slope is just
    /// the sign of `y_j - y_i` — which is exactly the term Mann–Kendall sums.
    /// For the median to be negative, more than half the pair signs must be
    /// negative; for `S` to be positive, more must be positive than negative.
    /// Both cannot hold. (The even-length case lands on `S == 0`, not on a
    /// disagreement.) Brute-forced over 221,840 windows: **zero.**
    ///
    /// ▶ So [`Trend::p`] is a statement about **strength only**. It can say the
    /// direction is weakly supported; it can never say it is the other one.
    /// `tests/climb.rs` asserts this over random windows so that a future
    /// estimator swap has to argue with a test rather than with a comment.
    #[must_use]
    pub const fn direction_is_undisputed(&self) -> bool {
        let by_slope = self.slope.num;
        !((by_slope < 0 && self.s > 0) || (by_slope > 0 && self.s < 0))
    }
}

/// Theil–Sen: the median of every pairwise slope. `None` for fewer than two
/// points.
///
/// The x values are positions, so every denominator is a positive index
/// difference and the comparison `a/b < c/d` is exactly `a*d < c*b`. Nothing
/// here divides until [`Slope::new`] reduces, so nothing rounds.
fn theil_sen(y: &[u64]) -> Option<Slope> {
    if y.len() < 2 {
        return None;
    }
    let mut slopes: Vec<(i128, i128)> = Vec::with_capacity(y.len() * (y.len() - 1) / 2);
    for (i, a) in y.iter().enumerate() {
        // ⚠ The gap is counted up rather than cast from the index difference:
        // it keeps the whole function free of a `usize` conversion that would
        // need a fallback nobody could choose honestly.
        let mut dx: i128 = 0;
        for b in &y[i + 1..] {
            dx += 1;
            slopes.push((i128::from(*b) - i128::from(*a), dx));
        }
    }
    slopes.sort_unstable_by(|(an, ad), (bn, bd)| (an * bd).cmp(&(bn * ad)));
    let mid = slopes.len() / 2;
    Some(if slopes.len().is_multiple_of(2) {
        // The even case is the mean of the two middle slopes, taken as one
        // fraction so it is still exact: a/b and c/d average to (ad+cb)/(2bd).
        let (an, ad) = slopes[mid - 1];
        let (bn, bd) = slopes[mid];
        Slope::new(an * bd + bn * ad, 2 * ad * bd)
    } else {
        let (num, den) = slopes[mid];
        Slope::new(num, den)
    })
}

/// Mann–Kendall's `S`, and how many pairs were tied.
///
/// `S` is *later value higher* minus *later value lower* over every pair. A tie
/// contributes zero to `S` by construction and is counted separately, because
/// it is the thing that makes [`exact_p`]'s distribution the wrong one.
fn mann_kendall(y: &[u64]) -> (i64, usize) {
    let mut s: i64 = 0;
    let mut tied = 0usize;
    for (i, a) in y.iter().enumerate() {
        for b in &y[i + 1..] {
            match b.cmp(a) {
                std::cmp::Ordering::Greater => s += 1,
                std::cmp::Ordering::Less => s -= 1,
                std::cmp::Ordering::Equal => tied += 1,
            }
        }
    }
    (s, tied)
}

/// How many permutations of `n` distinct values have exactly `k` inversions,
/// for every `k` — the Mahonian numbers, by the standard sliding-window
/// recurrence.
///
/// Multiplying by `1 + x + … + x^(m-1)` at each step is a running sum of the
/// previous row over a window of `m`, which is why this is linear in the table
/// rather than quadratic in it.
fn inversion_counts(n: usize) -> Vec<u128> {
    let max = n * (n - 1) / 2;
    let mut cur = vec![0u128; max + 1];
    cur[0] = 1;
    for m in 2..=n {
        let mut next = vec![0u128; max + 1];
        let mut running: u128 = 0;
        for k in 0..=max {
            running += cur[k];
            if k >= m {
                running -= cur[k - m];
            }
            next[k] = running;
        }
        cur = next;
    }
    cur
}

/// The exact two-sided p for an observed `S` over `n` distinct values, as
/// `(favourable, total)`.
///
/// Under the null every ordering is equally likely, and for a permutation
/// `S = N - 2D` where `N` is the pair count and `D` the inversions — so the
/// distribution of `S` is [`inversion_counts`] read through that identity, and
/// *at least as extreme as this* is the rows with `|N - 2D| >= |s|`.
///
/// `None` when `n` is below two (no pairs) or above [`Trend::EXACT_TO`] (the
/// total stops fitting a `u128`). ⚠ It assumes **distinct** values; see
/// [`Trend::tied_pairs`].
fn exact_p(n: usize, s: i64) -> Option<(u128, u128)> {
    if !(2..=Trend::EXACT_TO).contains(&n) {
        return None;
    }
    let pairs = i64::try_from(n * (n - 1) / 2).ok()?;
    let counts = inversion_counts(n);
    let extreme = s.abs();
    let mut favourable: u128 = 0;
    for (inversions, c) in (0_i64..).zip(counts.iter()) {
        if (pairs - 2 * inversions).abs() >= extreme {
            favourable += c;
        }
    }
    Some((favourable, counts.iter().sum()))
}

/// **Where the project stands on W13's ladder** — M3's countable half, and the
/// two clauses it is not allowed to answer.
///
/// See the module docs for the three choices behind it. In one line: the window
/// is the **most recent** boundary-crossing changes, in the order reviews were
/// recorded, and [`Climb::countable_half`] is two of M3's four clauses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Climb {
    /// How many changes M3 asks for. [`Climb::WANT`] unless a caller asked for
    /// another window.
    pub want: usize,
    /// Every change on the ladder that crossed a module boundary — the M2
    /// count, of which the window is the tail.
    pub crossing: usize,
    /// The window's seconds, **oldest first**, at most `want` of them.
    pub window: Vec<u64>,
    /// 🚨 Changes that crossed no boundary and were reviewed *between* the
    /// window's first and last — the input to the stricter reading of
    /// *consecutive*, handed over rather than silently resolved. Zero means the
    /// two readings agree and the distinction does not arise.
    pub interleaved: usize,
    /// The trend over `window`, or `None` below two changes.
    pub trend: Option<Trend>,
}

impl Climb {
    /// Ten. `research/W13-codev.md` §7's M3: *ten consecutive M2 tasks*.
    pub const WANT: usize = 10;

    /// Read the ladder's rows. See [`Ladder::climb`] for the usual entry point.
    #[must_use]
    pub fn read(changes: &[Reviewed], want: usize) -> Self {
        let crossing: Vec<usize> = changes
            .iter()
            .enumerate()
            .filter(|(_, c)| c.crossed_boundary)
            .map(|(i, _)| i)
            .collect();
        let tail = &crossing[crossing.len().saturating_sub(want)..];
        let window: Vec<u64> = tail.iter().map(|&i| changes[i].seconds).collect();
        let interleaved = match (tail.first(), tail.last()) {
            (Some(&first), Some(&last)) => changes[first..=last]
                .iter()
                .filter(|c| !c.crossed_boundary)
                .count(),
            _ => 0,
        };
        let trend = Trend::read(&window);
        Self {
            want,
            crossing: crossing.len(),
            window,
            interleaved,
            trend,
        }
    }
}

impl Climb {
    /// How many of the wanted changes the window actually has.
    #[must_use]
    pub const fn have(&self) -> usize {
        self.window.len()
    }

    /// How many more boundary-crossing changes M3 is waiting on.
    #[must_use]
    pub const fn short_by(&self) -> usize {
        self.want.saturating_sub(self.window.len())
    }

    /// 🚨 **M3's countable half: clause 1 and clause 2, and nothing else.**
    ///
    /// True when the window is full *and* the trend is flat or falling. ⚠ It is
    /// deliberately not called `reached_m3`: clauses 3 and 4 — no human edit to
    /// the agent's diff, and defect survival at 30 and 90 days — have no writer
    /// in this workspace, so **this being true does not make M3 true** and no
    /// caller may print it as though it did.
    #[must_use]
    pub fn countable_half(&self) -> bool {
        self.window.len() == self.want && self.trend.is_some_and(|t| t.flat_or_falling())
    }

    /// The clauses of M3 that no fold over this log can decide, as the sentences
    /// a console prints beside the half it can.
    ///
    /// 🚨 **A list rather than a comment**, so that the day something does
    /// record them, the compiler's users are the ones who find this.
    #[must_use]
    pub const fn cannot_say() -> &'static [&'static str] {
        &[
            "whether a human edited the agent's diff before it landed — nothing writes it",
            "surviving defects at 30 and 90 days — OQ-W13-3, no mechanism specified",
        ]
    }
}

impl Ladder {
    /// **Which rung this ladder supports**, over the window M3 names.
    ///
    /// The reading a project runs on itself. See [`Climb`] for what it refuses
    /// to answer, and [`Climb::read`] to ask for a window other than ten.
    #[must_use]
    pub fn climb(&self) -> Climb {
        Climb::read(&self.changes, Climb::WANT)
    }
}
