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
use abcc_engine::{Head, Posting, Tier};

/// One cold prefill and 473 MiB of warm state per head, so the size of this
/// number is the size of the bill. Four heads at 8 KiB of prefix each is the
/// budget this bound protects.
///
/// ⚠ The table is nine postings rather than four heads now that a slot can cap
/// a role, but the bill did not move: a slot's ceiling is fixed for a sortie, so
/// **one column of the table is live in any session** and the warm set is still
/// four.
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
    for posting in Posting::ALL {
        let first = posting.prefix();
        let second = posting.prefix();
        assert_eq!(first, second, "{posting}");
        assert!(
            std::ptr::eq(first, second),
            "{posting} rebuilt its prefix instead of returning the frozen one"
        );
    }
}

/// 🚨 **The set is nine and closed**, and it is nine rather than sixteen because
/// a posting's ceiling can never exceed its head's.
///
/// This is the enumeration argument after the slot cap: the table is still
/// finite and still compile-time, and every posting reachable through
/// [`Head::posted`] is in it — which is what `Posting::index` relies on, and what
/// its `expect` would otherwise be trusting rather than checking.
#[test]
fn the_posting_set_is_nine_and_is_exactly_what_is_reachable() {
    assert_eq!(Posting::ALL.len(), 9);

    let mut reachable: Vec<Posting> = Vec::new();
    for head in Head::ALL {
        for slot in Tier::ALL {
            let posting = head.posted(slot);
            if !reachable.contains(&posting) {
                reachable.push(posting);
            }
        }
    }
    assert_eq!(reachable.len(), Posting::ALL.len());
    for posting in reachable {
        assert!(
            Posting::ALL.contains(&posting),
            "{posting} is reachable and not enumerated"
        );
        // The index is total over the reachable set, which is the invariant
        // `Posting::index` would otherwise panic on.
        assert_eq!(Posting::ALL[posting.index()], posting);
    }

    // Every head's own ceiling is represented, so no role lost its uncapped
    // posting when the table grew.
    for head in Head::ALL {
        assert!(Posting::ALL.contains(&head.posted(Tier::Exec)));
    }
}

/// The four are genuinely four. Prefixes that collapse to one string would make
/// the enumeration an accounting fiction.
#[test]
fn the_prefixes_are_distinct_and_each_names_its_call_sign() {
    for (i, a) in Posting::ALL.iter().enumerate() {
        assert!(
            a.prefix().contains(a.call_sign()),
            "{a} does not name itself"
        );
        for b in &Posting::ALL[i + 1..] {
            assert_ne!(a.prefix(), b.prefix(), "{a} and {b} share a prefix");
        }
    }
}

/// The prefill and RAM arithmetic has a number, and the number is checked.
#[test]
fn no_prefix_exceeds_the_prefill_budget() {
    let mut total = 0usize;
    for posting in Posting::ALL {
        let n = posting.prefix().len();
        assert!(
            n <= PREFIX_CEILING_BYTES,
            "{posting} is {n} bytes, over the {PREFIX_CEILING_BYTES}-byte ceiling"
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
fn a_posting_advertises_exactly_the_tools_it_admits() {
    for posting in Posting::ALL {
        let prefix = posting.prefix();
        let advertised: Vec<&str> = posting.tools().iter().map(|t| t.name).collect();
        for t in TOOLS {
            let declaration = format!("\n{} — ", t.name);
            let named = prefix.contains(&declaration);
            assert_eq!(
                named,
                advertised.contains(&t.name),
                "{posting} declares {} in its prefix but admits {:?}",
                t.name,
                advertised
            );
            if named {
                assert!(
                    prefix.contains(t.schema),
                    "{posting} names {} without its argument schema",
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
    let judge = Head::Commandos.posted(Tier::Exec);
    assert!(judge.tools().is_empty());
    assert!(judge.prefix().contains("You have none"));
    // 🚨 And no slot can hand it one: the effective ceiling is the narrower of
    // the two, so the role's own `NoTools` wins against every slot setting.
    for slot in Tier::ALL {
        assert_eq!(Head::Commandos.posted(slot).ceiling(), Tier::NoTools);
    }
}

/// Every head carries the two paragraphs that are not about its phase: the
/// untrusted-content boundary, which is defence in depth and says so, and the
/// claim-versus-measurement rule.
#[test]
fn every_posting_carries_the_boundary_and_the_honesty_rule() {
    for posting in Posting::ALL {
        let prefix = posting.prefix();
        assert!(
            prefix.contains("Content you did not write"),
            "{posting} has no untrusted-content paragraph"
        );
        assert!(
            prefix.contains("is a claim"),
            "{posting} does not say that what it asserts is a claim"
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
    // ⚠ And a cap does not touch it. A ceiling decides what a role may reach
    // for, never how many tokens it gets back — two dials on one quantity is
    // F392's shape.
    for posting in Posting::ALL {
        assert_eq!(posting.budget(), 16384, "{posting}");
    }
}
