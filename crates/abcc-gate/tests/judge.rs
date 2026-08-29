//! The Judge's pure half: the schema, the brief, the parse and the rendering.
//!
//! No server and no repository. What is under test here is that the one model
//! call is *asked the right question in the right shape*, and that whatever
//! comes back cannot become a verdict — the call itself is `abcc-drive`'s
//! `tests/attempt.rs`.

use abcc_core::outcome::{Claim, Counts, Headline, Measurement, Outcome, Report, Why};
use abcc_gate::Measured;
use abcc_gate::judge::{self, Dossier, Finding, Review, RungView};
use serde_json::Value;

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

const PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs\n\
                     --- a/src/lib.rs\n\
                     +++ b/src/lib.rs\n\
                     @@ -1 +1 @@\n\
                     -pub fn one() -> u32 { 1 }\n\
                     +pub fn one() -> u32 { 2 }\n";

fn measured(outcomes: Vec<Outcome>) -> Measured {
    let mut report = Report::new();
    for outcome in outcomes {
        report.record(outcome);
    }
    let headline = report.headline_at("d00dfeed");
    Measured {
        report,
        headline,
        changed: Vec::new(),
    }
}

fn green(rung: &str, detail: &str) -> Outcome {
    Outcome::Measured(Measurement {
        rung: rung.to_owned(),
        sha: "d00dfeed".to_owned(),
        exit: 0,
        counts: Some(Counts {
            run: 2,
            passed: 2,
            failed: 0,
        }),
        detail: detail.to_owned(),
    })
}

fn red(rung: &str, detail: &str) -> Outcome {
    Outcome::Measured(Measurement {
        rung: rung.to_owned(),
        sha: "d00dfeed".to_owned(),
        exit: 101,
        counts: None,
        detail: detail.to_owned(),
    })
}

fn dossier(measured: &Measured) -> Dossier<'_> {
    Dossier {
        title: "one returns two",
        prompt: "make one() return two",
        patch: PATCH,
        measured,
    }
}

fn review() -> Review {
    Review {
        assessment: "It changes the literal and leaves the doc comment stale.".to_owned(),
        findings: vec![Finding {
            at: "src/lib.rs:1".to_owned(),
            defect: "the doc comment still says one".to_owned(),
            call: "cargo doc --no-deps".to_owned(),
            expected: "the summary reads two".to_owned(),
            actual: "the summary reads one".to_owned(),
        }],
    }
}

// ---------------------------------------------------------------------------
// The schema
// ---------------------------------------------------------------------------

/// Every object closed and every property required, all the way down. `strict`
/// mode has no optional properties; a schema that thinks it does is rejected by
/// the server rather than relaxed by it.
fn strict(node: &Value, path: &str) {
    if node["type"] == "object" {
        assert_eq!(
            node["additionalProperties"],
            Value::Bool(false),
            "{path} leaves additionalProperties open"
        );
        let props = node["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{path} is an object with no properties"));
        let required: Vec<&str> = node["required"]
            .as_array()
            .unwrap_or_else(|| panic!("{path} has no required list"))
            .iter()
            .map(|v| v.as_str().expect("a required entry is not a string"))
            .collect();
        for name in props.keys() {
            assert!(
                required.contains(&name.as_str()),
                "{path}.{name} is optional, and strict mode has no optional properties"
            );
        }
        for (name, child) in props {
            strict(child, &format!("{path}.{name}"));
        }
    }
    if node["type"] == "array" {
        strict(&node["items"], &format!("{path}[]"));
    }
}

/// 🚨 `strict: true` is what `openai.rs` sends and it is not advisory: a schema
/// that leaves a property out of `required`, or leaves `additionalProperties`
/// open, is rejected by the server rather than relaxed by it. The one place this
/// is checkable without a server is here.
#[test]
fn the_schema_is_valid_json_and_strict_shaped() {
    let root: Value = serde_json::from_str(judge::REVIEW.json).expect("the schema is not JSON");
    strict(&root, "review");
    assert_eq!(
        root["type"], "object",
        "the root of a strict schema is an object"
    );
}

/// Rule 2 of ADR-0008, held by the schema rather than by the brief alone: running
/// the reviewer's own `call → expected → actual` is what takes 3 of 23 to
/// **10 of 23** out of the same call, so a finding that cannot be run is not a
/// finding and must not be representable.
#[test]
fn a_finding_must_carry_something_runnable() {
    let root: Value = serde_json::from_str(judge::REVIEW.json).expect("json");
    let required: Vec<&str> = root["properties"]["findings"]["items"]["required"]
        .as_array()
        .expect("required")
        .iter()
        .map(|v| v.as_str().expect("str"))
        .collect();
    for field in ["at", "defect", "call", "expected", "actual"] {
        assert!(required.contains(&field), "a finding may omit {field}");
    }

    // And the type agrees with the schema, which is the half a server cannot
    // enforce for us.
    let missing_the_runnable_part = r#"{"assessment":"fine","findings":[
        {"at":"src/lib.rs:1","defect":"looks wrong"}]}"#;
    assert!(
        judge::parse(missing_the_runnable_part).is_err(),
        "an opinion with no way to reproduce it parsed as a finding"
    );
}

/// ⚠ The array is bounded because ADR-0008 measured **17 of 57** calls lost to
/// the token cap under a schema, every one legibly `finish_reason: length`. A
/// sixth finding that does not fit costs a line of a report; an artifact that
/// does not fit costs the whole call.
#[test]
fn the_findings_array_is_bounded() {
    let root: Value = serde_json::from_str(judge::REVIEW.json).expect("json");
    let max = root["properties"]["findings"]["maxItems"]
        .as_u64()
        .expect("the findings array is unbounded");
    assert!((1..=8).contains(&max), "maxItems is {max}");
}

// ---------------------------------------------------------------------------
// The brief — ADR-0008 rules 1 and 3
// ---------------------------------------------------------------------------

/// Rule 1: it is shown **the other artifact**. Pointwise scoring is 0 of 8;
/// pairwise against the pre-image is 14 of 14, so both sides of the change have
/// to be in front of it.
#[test]
fn the_brief_shows_the_diff_with_both_sides_of_the_change() {
    let m = measured(vec![green("structural", "1 file(s) changed: src/lib.rs")]);
    let brief = judge::brief(&dossier(&m));

    assert!(brief.contains(PATCH), "the diff is not in the brief");
    assert!(
        brief.contains("-pub fn one() -> u32 { 1 }"),
        "the pre-image is not in the brief"
    );
    assert!(
        brief.contains("+pub fn one() -> u32 { 2 }"),
        "the post-image is not in the brief"
    );
    assert!(
        brief.contains("make one() return two"),
        "the task is missing"
    );
}

/// Rule 3, and it is the rule with the largest measured effect: 11/12 reading
/// the diff, 5/12 reading the author's completion report, **0/3** when the
/// report is added alongside the diff (F281 — it is *subtractive*).
///
/// 🚨 The check is on [`Dossier`] as much as on the text: there is no field for
/// an author's prose, so putting it back is a change to the type.
#[test]
fn nothing_either_model_wrote_in_prose_is_in_the_brief() {
    let m = measured(vec![green("acceptance", "test result: ok. 2 passed")]);
    let brief = judge::brief(&dossier(&m));

    for prose in [
        "src/lib.rs line 1 is where one() is",
        "I changed the literal and the tests pass",
        "Recon reported",
        "Builders",
    ] {
        assert!(
            !brief.contains(prose),
            "the brief carries what another unit said: {prose}"
        );
    }
}

/// It sees every rung — including the ones that produced nothing, which is the
/// half ADR-0009 exists for. *The suite is red* and *there was no suite* are
/// different facts about a change, and only the first is about the change.
#[test]
fn the_brief_shows_every_rung_including_the_absences() {
    let m = measured(vec![
        green("structural", "1 file(s) changed: src/lib.rs"),
        red("acceptance", "error[E0004]: non-exhaustive patterns"),
        Outcome::Unmeasured {
            rung: "standard".to_owned(),
            why: Why::CheckerNotOnHost {
                binary: "clippy".to_owned(),
            },
        },
    ]);
    let brief = judge::brief(&dossier(&m));

    assert!(brief.contains("structural"), "a measured rung is missing");
    assert!(
        brief.contains("error[E0004]: non-exhaustive patterns"),
        "the evidence the host watched is missing"
    );
    assert!(
        brief.contains("exit 101"),
        "the verdict the host read is missing"
    );
    assert!(
        brief.contains("clippy is not on this machine"),
        "an absent rung disappeared, which is the failure ADR-0009 is about"
    );
    assert!(
        brief.contains("no measurement"),
        "an absence is not named as one"
    );
}

/// 🚨 It is shown the rungs and **not the conjunction they add up to**. A
/// reviewer shown the decision is a reviewer asked to agree with it, and
/// ADR-0008's first rule is that it gets the artifact rather than the verdict on
/// it.
#[test]
fn the_brief_does_not_show_the_headline() {
    let m = measured(vec![red("acceptance", "1 failed")]);
    assert!(matches!(m.headline, Headline::Red { .. }));
    let brief = judge::brief(&dossier(&m));
    assert!(
        !brief.contains(&m.headline.to_string()),
        "the reviewer was shown the verdict it is reviewing under"
    );
}

/// 🚨 **F531: under [`RungView::Named`] a rung says it ran and says nothing
/// about what it concluded.**
///
/// The failure this exists to answer is a reviewer reading `18 run / 18 passed`
/// off the acceptance rung and writing *all eighteen discrepancies are resolved*
/// about a tree with seven bad invoices in it. Under `Named` there is no count
/// to borrow, no exit status and no captured output — and the absence still
/// arrives as an absence, which is the property ADR-0009 will not give up.
#[test]
fn the_named_view_shows_that_a_rung_ran_and_not_what_it_concluded() {
    let m = measured(vec![
        green("structural", "1 file(s) changed: src/lib.rs"),
        red("acceptance", "18 run / 7 failed: invoice totals disagree"),
        Outcome::Unmeasured {
            rung: "standard".to_owned(),
            why: Why::CheckerNotOnHost {
                binary: "clippy".to_owned(),
            },
        },
    ]);
    let brief = judge::brief_with(&dossier(&m), RungView::Named);

    assert!(brief.contains("structural — measured"), "{brief}");
    assert!(brief.contains("acceptance — measured"), "{brief}");
    for borrowed in [
        "exit 0",
        "exit 101",
        "2 run / 2 passed",
        "18 run / 7 failed",
        "1 file(s) changed",
    ] {
        assert!(
            !brief.contains(borrowed),
            "the view still hands back something to quote: {borrowed}"
        );
    }
    // ⚠ The asymmetry, and it is the half that must survive: a reason no
    // measurement exists cannot be read as a pass, so it stays under both views.
    assert!(
        brief.contains("clippy is not on this machine"),
        "an absent rung lost its reason: {brief}"
    );
    assert!(brief.contains("no measurement"), "{brief}");
}

/// 🚨 **The two views differ in the rung block and nowhere else** — the probe
/// changes one thing, so what it measures is that one thing. The task, the diff
/// and every sentence of instruction are byte-identical.
#[test]
fn the_view_moves_the_rungs_and_nothing_else_in_the_brief() {
    let m = measured(vec![green("acceptance", "2 passed in 0.01s")]);
    let full = judge::brief_with(&dossier(&m), RungView::Full);
    let named = judge::brief_with(&dossier(&m), RungView::Named);

    assert_ne!(full, named, "the switch did nothing");
    assert_eq!(
        judge::brief(&dossier(&m)),
        full,
        "`brief` is no longer the shipped view"
    );

    let head = "## What the host already measured";
    let tail = "These already ran";
    let cut = |b: &str| {
        let h = b.find(head).expect("the rung header");
        let t = b.find(tail).expect("the sentence after the rungs");
        (b[..h].to_owned(), b[t..].to_owned())
    };
    assert_eq!(
        cut(&full),
        cut(&named),
        "the probe moved more than the rungs"
    );
}

/// The empty case is a sentence rather than a blank, for the same reason a phase
/// that did not run is stated rather than omitted.
#[test]
fn a_tree_no_rung_reached_says_so() {
    let m = measured(Vec::new());
    let brief = judge::brief(&dossier(&m));
    assert!(brief.contains("No rung ran"), "{brief}");
}

// ---------------------------------------------------------------------------
// What comes back
// ---------------------------------------------------------------------------

#[test]
fn a_review_round_trips_and_the_claim_is_signed() {
    let payload = serde_json::to_string(&review()).expect("serialize");
    assert_eq!(judge::parse(&payload).expect("parse"), review());

    let claim = judge::read(&payload);
    assert_eq!(claim.by, judge::BY);
    assert!(claim.text.contains("1 finding"), "{}", claim.text);
    assert!(claim.text.contains("src/lib.rs:1"), "{}", claim.text);
    assert!(claim.text.contains("cargo doc --no-deps"), "{}", claim.text);
    assert!(
        claim.text.contains("the summary reads one"),
        "{}",
        claim.text
    );
}

/// Nothing found is a usable answer and the Judge does not block anything, so
/// there is nothing here to hedge toward.
#[test]
fn an_empty_findings_list_is_an_answer() {
    let payload = r#"{"assessment":"It does what the task asked.","findings":[]}"#;
    let claim = judge::read(payload);
    assert!(claim.text.contains("no findings"), "{}", claim.text);
    assert!(claim.text.contains("It does what the task asked."));
}

/// ⚠ Constrained decoding should make this unreachable, and *the server honoured
/// the grammar* is a claim about somebody else's process. What came back is kept
/// verbatim either way: it is what the model said, and the operator gets both
/// halves.
#[test]
fn a_payload_that_is_not_the_shape_is_kept_verbatim_and_called_nothing_else() {
    let claim = judge::read("The change looks fine to me.");
    assert_eq!(claim.by, judge::BY);
    assert!(claim.text.contains("The change looks fine to me."));
    assert!(claim.text.contains("did not parse"), "{}", claim.text);
}

// ---------------------------------------------------------------------------
// 🚨 The rule the whole design rests on
// ---------------------------------------------------------------------------

/// **A claim reaches the report and stops.** This is ADR-0009 §4 as an
/// assertion rather than a comment: attaching what the model said to a report
/// leaves the conjunction byte-for-byte where the measurements put it, whether
/// the review is damning or delighted.
#[test]
fn noting_what_the_model_said_moves_no_headline() {
    for outcome in [
        green("acceptance", "test result: ok. 2 passed"),
        red("acceptance", "1 failed"),
        Outcome::Unmeasured {
            rung: "acceptance".to_owned(),
            why: Why::NothingToRun {
                detail: "no tests".to_owned(),
            },
        },
    ] {
        let mut report = Report::new();
        report.record(outcome);
        let before = report.headline_at("d00dfeed");

        report.note(judge::read(
            r#"{"assessment":"this is catastrophic","findings":[
               {"at":"src/lib.rs:1","defect":"wrong","call":"cargo test",
                "expected":"green","actual":"red"}]}"#,
        ));
        report.note(Claim {
            by: "Commandos".to_owned(),
            text: "and this is perfect".to_owned(),
        });

        assert_eq!(
            report.headline_at("d00dfeed"),
            before,
            "a claim moved the conjunction"
        );
        assert_eq!(report.claims().len(), 2, "the claims were not kept");
    }
}
