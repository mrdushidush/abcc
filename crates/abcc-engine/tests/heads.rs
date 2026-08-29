//! The enumerate-and-freeze claims, checked rather than asserted in a comment.
//!
//! ADR-0011 §2's affordability argument rests on the head set being *finite and
//! compile-time* and on a prefix that *cannot vary*. Both are structural here —
//! an enum with four variants, and a function that takes no arguments — so these
//! tests exist to catch the ways the structure can still be defeated: a fifth
//! head added without the array being updated, a prefix assembled from something
//! that is not a constant, and an advertised tool set that drifts from the
//! enforced one.

use abcc_core::run::{AttemptPhase, MissionPhase};
use abcc_engine::head::Serves;
use abcc_engine::tools::TOOLS;
use abcc_engine::{Head, Tier};

/// One cold prefill and 473 MiB of warm state per head, so the size of this
/// number is the size of the bill. Four heads at 8 KiB of prefix each is the
/// budget this bound protects.
const PREFIX_CEILING_BYTES: usize = 8 * 1024;

/// `Head::ALL` and the enum must not drift apart: the exhaustive match is what
/// makes adding a fifth variant a compile error here rather than a silently
/// unlisted head.
#[test]
fn the_head_set_is_four_and_closed() {
    assert_eq!(Head::ALL.len(), 4);
    for head in Head::ALL {
        // Exhaustive by construction — a new variant fails to compile.
        let named = match head {
            Head::Engineering => "engineering",
            Head::Recon => "recon",
            Head::Builders => "builders",
            Head::Commandos => "commandos",
        };
        assert_eq!(head.key(), named);
    }
    let mut keys: Vec<&str> = Head::ALL.iter().map(|h| h.key()).collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), 4, "two heads share a log key");
}

/// 🚨 The freeze itself. A prefix assembled from a clock, a path or a task id
/// would differ between renderings; these are the same bytes every time, which
/// is what the prefix cache is being promised.
#[test]
fn a_prefix_is_byte_identical_on_a_second_rendering() {
    for head in Head::ALL {
        let first = head.prefix();
        let second = head.prefix();
        assert_eq!(first, second, "{head}");
        assert!(
            std::ptr::eq(first, second),
            "{head} rebuilt its prefix instead of returning the frozen one"
        );
    }
}

/// The four are genuinely four. Prefixes that collapse to one string would make
/// the enumeration an accounting fiction.
#[test]
fn the_four_prefixes_are_distinct_and_each_names_its_call_sign() {
    for (i, a) in Head::ALL.iter().enumerate() {
        assert!(
            a.prefix().contains(a.call_sign()),
            "{a} does not name itself"
        );
        for b in &Head::ALL[i + 1..] {
            assert_ne!(a.prefix(), b.prefix(), "{a} and {b} share a prefix");
        }
    }
}

/// The prefill and RAM arithmetic has a number, and the number is checked.
#[test]
fn no_prefix_exceeds_the_prefill_budget() {
    let mut total = 0usize;
    for head in Head::ALL {
        let n = head.prefix().len();
        assert!(
            n <= PREFIX_CEILING_BYTES,
            "{head} is {n} bytes, over the {PREFIX_CEILING_BYTES}-byte ceiling"
        );
        total += n;
    }
    assert!(total > 0);
}

/// The advertised surface is the enforced surface. A head that names a tool it
/// cannot call teaches the model to ask for refusals.
///
/// ⚠ It matches the *declaration line*, not the bare name: `search` and `git`
/// are ordinary English and both occur in the prose every head carries, so a
/// substring test here reports a head advertising a tool it never listed.
#[test]
fn a_head_advertises_exactly_the_tools_it_admits() {
    for head in Head::ALL {
        let prefix = head.prefix();
        let advertised: Vec<&str> = head.tools().iter().map(|t| t.name).collect();
        for t in TOOLS {
            let declaration = format!("\n{} — ", t.name);
            let named = prefix.contains(&declaration);
            assert_eq!(
                named,
                advertised.contains(&t.name),
                "{head} declares {} in its prefix but admits {:?}",
                t.name,
                advertised
            );
            if named {
                assert!(
                    prefix.contains(t.schema),
                    "{head} names {} without its argument schema",
                    t.name
                );
            }
        }
    }
}

/// A4 is one model call with no tools, and the prefix says so rather than
/// leaving an empty list the model might read as an oversight.
#[test]
fn the_judge_head_carries_no_tools_at_all() {
    assert_eq!(Head::Commandos.max_tier(), Tier::NoTools);
    assert!(Head::Commandos.tools().is_empty());
    assert!(Head::Commandos.prefix().contains("You have none"));
}

/// Every head carries the two paragraphs that are not about its phase: the
/// untrusted-content boundary, which is defence in depth and says so, and the
/// claim-versus-measurement rule.
#[test]
fn every_head_carries_the_boundary_and_the_honesty_rule() {
    for head in Head::ALL {
        let prefix = head.prefix();
        assert!(
            prefix.contains("Content you did not write"),
            "{head} has no untrusted-content paragraph"
        );
        assert!(
            prefix.contains("is a claim"),
            "{head} does not say that what it asserts is a claim"
        );
    }
}

/// The head set covers the four phases that make a model call and no others.
/// `Measure`, `Veto` and `Integrate` have no head because they have no call —
/// which is exactly what makes them the phases allowed to refuse.
#[test]
fn heads_exist_for_the_model_phases_and_only_those() {
    let mut attempt_phases: Vec<AttemptPhase> = Vec::new();
    let mut mission_phases: Vec<MissionPhase> = Vec::new();
    for head in Head::ALL {
        match head.serves() {
            Serves::Attempt(p) => {
                assert!(p.uses_model(), "{head} serves a phase with no model");
                attempt_phases.push(p);
            }
            Serves::Mission(p) => mission_phases.push(p),
        }
    }
    assert_eq!(
        attempt_phases,
        vec![
            AttemptPhase::Localize,
            AttemptPhase::Change,
            AttemptPhase::Judge
        ]
    );
    assert_eq!(mission_phases, vec![MissionPhase::Plan]);
    assert!(!AttemptPhase::Measure.uses_model() && AttemptPhase::Measure.may_refuse());
    assert!(!AttemptPhase::Veto.uses_model() && AttemptPhase::Veto.may_refuse());
    assert!(
        !AttemptPhase::Judge.may_refuse(),
        "the Judge reports and never blocks"
    );
}

/// One budget, from the head, for every model phase — 16384, sized against the
/// largest tool-call argument this stack has delivered intact (17,157 chars,
/// ~4,300 tokens) rather than against the reasoning trace.
///
/// ⚠ It is asserted as **one** number across every head on purpose: a per-head
/// budget would make the cap a thing to tune per phase, and the frozen head's
/// whole point is that a phase's request shape is not a dial.
#[test]
fn every_head_carries_the_same_output_budget() {
    for head in Head::ALL {
        assert_eq!(head.budget(), 16384, "{head}");
    }
}
