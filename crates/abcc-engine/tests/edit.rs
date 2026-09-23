//! `edit_file`'s string half, specified by claudette's own tests for the tool it
//! reimplements (PLAN-TOOL decision 2a): `tools/shell.rs`'s `edit_file_*` and
//! `tools/near_miss.rs`'s tests at claudette `a450e00`. Where the wording
//! differs, the assertion is on the same fact.

use abcc_engine::edit::{EditError, near_miss_hint, replace};

// ---------------------------------------------------------------------------
// replace — claudette's edit_file_* tests
// ---------------------------------------------------------------------------

#[test]
fn a_unique_match_is_replaced() {
    let out = replace("one\ntwo\nthree\n", "two", "TWO", false).expect("unique");
    assert_eq!(out.content, "one\nTWO\nthree\n");
    assert_eq!(out.replacements, 1);
    assert_eq!(out.line, 2);
}

#[test]
fn an_ambiguous_match_is_refused_and_names_the_lines() {
    let err = replace("alpha\nalpha\nbeta\n", "alpha", "X", false).expect_err("ambiguous");
    assert_eq!(
        err,
        EditError::Ambiguous {
            count: 2,
            lines: vec![1, 2]
        }
    );
    let said = err.to_string();
    assert!(said.contains("appears 2 times"), "{said}");
    assert!(said.contains("at lines 1, 2"), "{said}");
}

#[test]
fn replace_all_replaces_every_occurrence() {
    let out = replace("foo / foo / foo\n", "foo", "bar", true).expect("replace_all");
    assert_eq!(out.content, "bar / bar / bar\n");
    assert_eq!(out.replacements, 3);
}

#[test]
fn without_replace_all_several_matches_are_still_refused() {
    let err = replace("foo / foo / foo\n", "foo", "bar", false).expect_err("ambiguous");
    assert!(err.to_string().contains("appears 3 times"), "{err}");
}

#[test]
fn replace_all_still_fires_the_no_op_guard() {
    let err = replace("foo foo\n", "foo", "foo", true).expect_err("no-op");
    assert_eq!(err, EditError::NoChange);
    assert!(err.to_string().contains("no change"), "{err}");
}

#[test]
fn zero_matches_is_refused() {
    let err = replace("one\ntwo\n", "nonexistent", "X", false).expect_err("absent");
    assert!(err.to_string().contains("not found"), "{err}");
}

/// Dogfood 2026-06-13 in claudette: identical old and new reported success and
/// spiralled the model into re-sending the same edit.
#[test]
fn identical_old_and_new_is_a_loud_no_op() {
    let err = replace("alpha\nbeta\ngamma\n", "beta\n", "beta\n", false).expect_err("no-op");
    assert_eq!(err, EditError::NoChange);
}

#[test]
fn a_zero_match_names_over_escaped_backslashes() {
    let content = "fn pat() {\n    let re = r\"^\\s*fn\";\n}\n";
    let old = "    let re = r\"^\\\\s*fn\";\n";
    let err =
        replace(content, old, "    let re = r\"^\\\\s*struct\";\n", false).expect_err("absent");
    assert!(
        err.to_string().contains("over-escapes backslashes"),
        "{err}"
    );
}

// ---------------------------------------------------------------------------
// Beyond claudette's set
// ---------------------------------------------------------------------------

/// The `SEC-06` shape: a deletion is an empty `new_text`.
#[test]
fn an_empty_new_text_deletes_the_snippet() {
    let content = "keep 1\ndrop a\ndrop b\nkeep 2\n";
    let out = replace(content, "drop a\ndrop b\n", "", false).expect("delete");
    assert_eq!(out.content, "keep 1\nkeep 2\n");
    assert_eq!(out.line, 2);
}

#[test]
fn an_empty_old_text_is_refused() {
    assert_eq!(
        replace("anything\n", "", "x", false),
        Err(EditError::EmptyOldText)
    );
    assert_eq!(
        replace("anything\n", "", "x", true),
        Err(EditError::EmptyOldText)
    );
}

#[test]
fn an_ambiguity_refusal_lists_at_most_eight_sites() {
    let content = "x\n".repeat(20);
    let err = replace(&content, "x", "y", false).expect_err("ambiguous");
    let EditError::Ambiguous { count, lines } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(*count, 20);
    assert_eq!(lines.len(), 8);
    assert!(err.to_string().contains(", …)"), "{err}");
}

// ---------------------------------------------------------------------------
// near_miss_hint — claudette's near_miss.rs tests
// ---------------------------------------------------------------------------

#[test]
fn over_escaped_backslashes_are_detected() {
    let content = "fn pat() {\n    let re = r\"^\\s*fn\\s+\\w+\";\n    re\n}\n";
    let block = "    let re = r\"^\\\\s*fn\\\\s+\\\\w+\";\n    re\n";
    let hint = near_miss_hint(content, block).expect("must diagnose over-escaping");
    assert!(hint.contains("over-escapes backslashes"), "{hint}");
    assert!(
        hint.contains("\\\\s"),
        "the sample line should appear: {hint}"
    );
}

#[test]
fn a_de_doubling_that_still_does_not_match_is_not_reported() {
    let hint = near_miss_hint("alpha\nbeta\ngamma\n", "let re = r\"^\\\\d+\";\n");
    assert!(
        hint.as_deref().is_none_or(|h| !h.contains("over-escapes")),
        "{hint:?}"
    );
}

#[test]
fn the_closest_window_reports_its_first_difference() {
    let content = "fn main() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n";
    let block = "fn main() {\n    let a = 1;\n    let b = 99;\n    let c = 3;\n";
    let hint = near_miss_hint(content, block).expect("must find the near window");
    assert!(hint.contains("Closest match: lines 1-4"), "{hint}");
    assert!(hint.contains("3/4"), "{hint}");
    assert!(hint.contains("First difference at line 3"), "{hint}");
    assert!(
        hint.contains("let b = 2;"),
        "the file side is missing: {hint}"
    );
    assert!(
        hint.contains("let b = 99;"),
        "the block side is missing: {hint}"
    );
}

#[test]
fn a_whitespace_only_mismatch_is_reported_as_such() {
    let hint = near_miss_hint("if x {\n    do_it();\n}\n", "if x {\n        do_it();\n}")
        .expect("must diagnose whitespace");
    assert!(hint.contains("except for whitespace"), "{hint}");
}

#[test]
fn an_unrelated_block_yields_no_hint() {
    assert_eq!(
        near_miss_hint(
            "alpha\nbeta\ngamma\n",
            "fn totally() {\n    different();\n}\n"
        ),
        None
    );
}

#[test]
fn a_below_half_match_yields_no_hint() {
    assert_eq!(
        near_miss_hint("one\ntwo\nthree\nfour\nfive\n", "one\nX\nY\nZ\n"),
        None
    );
}

#[test]
fn oversized_content_is_skipped() {
    assert_eq!(near_miss_hint(&"x\n".repeat(200_000), "x\ny\n"), None);
}

#[test]
fn an_empty_block_yields_no_hint() {
    assert_eq!(near_miss_hint("anything\n", ""), None);
}

#[test]
fn long_lines_are_cut_in_the_hint() {
    let long_a = format!("let value = \"{}A\";", "a".repeat(200));
    let long_b = format!("let value = \"{}B\";", "a".repeat(200));
    let hint = near_miss_hint(
        &format!("start\n{long_a}\nend\n"),
        &format!("start\n{long_b}\nend\n"),
    )
    .expect("must diagnose");
    assert!(hint.contains('…'), "snippets must be cut: {hint}");
}
