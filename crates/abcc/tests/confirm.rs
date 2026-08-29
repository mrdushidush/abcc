//! The model-confirmation decision, without a socket.
//!
//! 🚨 Two headline cases. [`a_loaded_model_that_is_not_the_one_asked_for_is_refused`]
//! is the one the module exists for: LM Studio answers a request naming a model
//! it does not have by using whichever model *is* loaded, and every number that
//! comes back looks exactly like a good one. And
//! [`a_downloaded_model_is_not_a_loaded_one`] is F495 — the `OpenAI`-dialect
//! listing answers a *different question*, so a check written against it would
//! pass in precisely the case above. Both are pure functions over shapes the two
//! runtimes really produce.

use abcc::confirm::{self, Evidence, How, Listing, Unconfirmed, Verdict};

const CHAMPION: &str = "qwen3.6-35b-a3b-mtp@iq3_s";
/// What the bare `llama-server` reports: a path, not an id.
const BY_PATH: &str = "C:/models/unsloth/qwen3.6-35B-A3B-MTP-IQ3_S.gguf";

fn ids(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| (*s).to_owned()).collect()
}

fn loaded(names: &[&str]) -> Listing {
    Listing::Loaded(ids(names))
}

fn available(names: &[&str]) -> Listing {
    Listing::Available(ids(names))
}

// ---------------------------------------------------------------------------
// F495 — what the server was asked, and what it answered
// ---------------------------------------------------------------------------

#[test]
fn a_downloaded_model_is_not_a_loaded_one() {
    // 🚨 F495. Measured on this box with the champion resident: `/v1/models`
    // returned 27 ids, 26 of them on disk and not in memory, while
    // `/api/v0/models` said `loaded` for one and `not-loaded` for 26. The two
    // answers are different facts and the type keeps them apart, so a run can
    // never read the weaker one as the stronger.
    let body = r#"{"data":[
        {"id":"qwen3.6-35b-a3b-mtp@iq3_s","state":"loaded"},
        {"id":"qwen3.5-4b","state":"not-loaded"},
        {"id":"google/gemma-4-12b","state":"not-loaded"}
    ]}"#;
    assert_eq!(
        confirm::parse_loaded(body).expect("a listing"),
        Some(ids(&[CHAMPION]))
    );

    // An `OpenAI`-dialect listing carries no `state` at all, and that is `None`
    // rather than "nothing is loaded": an unanswerable question must not become a
    // confident refusal.
    let openai = r#"{"data":[{"id":"a","object":"model"},{"id":"b","object":"model"}]}"#;
    assert_eq!(confirm::parse_loaded(openai).expect("a listing"), None);

    // And a server that says it is holding nothing is a third, real answer.
    let empty = r#"{"data":[{"id":"a","state":"not-loaded"}]}"#;
    assert_eq!(
        confirm::parse_loaded(empty).expect("a listing"),
        Some(Vec::new())
    );
}

#[test]
fn the_evidence_behind_a_confirmation_is_carried_and_said_out_loud() {
    let strong = confirm::decide(CHAMPION, None, &loaded(&[CHAMPION]));
    let weak = confirm::decide(CHAMPION, None, &available(&[CHAMPION]));
    assert!(strong.confirmed() && weak.confirmed());
    assert!(matches!(
        strong,
        Verdict::Confirmed {
            evidence: Evidence::Loaded,
            ..
        }
    ));
    assert!(matches!(
        weak,
        Verdict::Confirmed {
            evidence: Evidence::Listed,
            ..
        }
    ));
    // The log has to be able to tell the two runs apart afterwards.
    assert!(
        strong.note(CHAMPION).contains("has loaded"),
        "{}",
        strong.note(CHAMPION)
    );
    assert!(
        weak.note(CHAMPION)
            .contains("without saying what it has loaded"),
        "{}",
        weak.note(CHAMPION)
    );
}

#[test]
fn a_server_holding_nothing_and_a_server_offering_nothing_are_two_sentences() {
    let holding = confirm::decide(CHAMPION, Some("anything"), &Listing::Loaded(Vec::new()));
    let offering = confirm::decide(CHAMPION, Some("anything"), &Listing::Available(Vec::new()));
    assert_eq!(
        holding,
        Verdict::Unconfirmed {
            why: Unconfirmed::NothingLoaded,
            listing: Listing::Loaded(Vec::new()),
        }
    );
    assert_eq!(
        offering,
        Verdict::Unconfirmed {
            why: Unconfirmed::NothingServed,
            listing: Listing::Available(Vec::new()),
        }
    );
    assert!(holding.note(CHAMPION).contains("no model in memory"));
}

// ---------------------------------------------------------------------------
// the decision
// ---------------------------------------------------------------------------

#[test]
fn the_id_the_operator_asked_for_is_confirmed_without_any_configuration() {
    let verdict = confirm::decide(CHAMPION, None, &loaded(&[CHAMPION]));
    assert_eq!(
        verdict,
        Verdict::Confirmed {
            served: CHAMPION.to_owned(),
            how: How::Exact,
            evidence: Evidence::Loaded,
        }
    );
}

#[test]
fn a_loaded_model_that_is_not_the_one_asked_for_is_refused() {
    // 🚨 The whole point. The server is up, it is holding something, and it would
    // happily answer — with the wrong brain, and with a plausible transcript.
    let verdict = confirm::decide(CHAMPION, None, &loaded(&["qwen3.5-4b"]));
    assert_eq!(
        verdict,
        Verdict::Unconfirmed {
            why: Unconfirmed::NoFingerprint,
            listing: loaded(&["qwen3.5-4b"]),
        }
    );
    assert!(!verdict.confirmed());
}

#[test]
fn the_bare_servers_gguf_path_is_confirmed_only_by_the_operators_fingerprint() {
    // Nothing about a path equals an id, so without the assertion this is a
    // refusal...
    assert!(!confirm::decide(CHAMPION, None, &available(&[BY_PATH])).confirmed());
    // ...and with it, it is the operator saying what would count.
    assert_eq!(
        confirm::decide(CHAMPION, Some("A3B-MTP-IQ3_S"), &available(&[BY_PATH])),
        Verdict::Confirmed {
            served: BY_PATH.to_owned(),
            how: How::Fingerprint("A3B-MTP-IQ3_S".to_owned()),
            evidence: Evidence::Listed,
        }
    );
}

#[test]
fn a_fingerprint_matches_regardless_of_case_because_a_path_is_not_an_id() {
    // Windows hands a path back in whatever case the file has.
    assert!(confirm::decide(CHAMPION, Some("a3b-mtp-iq3_s"), &available(&[BY_PATH])).confirmed());
    assert!(
        confirm::decide(
            CHAMPION,
            Some("A3B-MTP-IQ3_S"),
            &available(&["...a3b-mtp-iq3_s..."])
        )
        .confirmed()
    );
}

#[test]
fn a_fingerprint_that_matches_nothing_is_not_a_pass() {
    assert_eq!(
        confirm::decide(CHAMPION, Some("qwen3.8-27b"), &loaded(&[BY_PATH])),
        Verdict::Unconfirmed {
            why: Unconfirmed::NoMatch {
                fingerprint: "qwen3.8-27b".to_owned()
            },
            listing: loaded(&[BY_PATH]),
        }
    );
}

#[test]
fn a_blank_fingerprint_is_no_fingerprint_rather_than_a_substring_of_everything() {
    // ⚠ `"".contains` is true of every string, so an empty assertion left in the
    // environment would confirm anything at all. It is treated as absent.
    for blank in ["", "   "] {
        assert_eq!(
            confirm::decide(CHAMPION, Some(blank), &loaded(&["qwen3.5-4b"])),
            Verdict::Unconfirmed {
                why: Unconfirmed::NoFingerprint,
                listing: loaded(&["qwen3.5-4b"]),
            },
            "{blank:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// what reaches the log
// ---------------------------------------------------------------------------

#[test]
fn the_note_names_what_was_asked_and_what_was_actually_there() {
    // This sentence is the only place the run says which brain answered:
    // `ModelCallStarted` records the id that was *requested*.
    let confirmed = confirm::decide(CHAMPION, Some("A3B-MTP"), &loaded(&[BY_PATH])).note(CHAMPION);
    assert!(confirmed.contains(CHAMPION), "{confirmed}");
    assert!(confirmed.contains(BY_PATH), "{confirmed}");
    assert!(confirmed.contains("fingerprint"), "{confirmed}");

    let refused = confirm::decide(CHAMPION, None, &loaded(&["qwen3.5-4b"])).note(CHAMPION);
    assert!(refused.contains("NOT confirmed"), "{refused}");
    assert!(refused.contains("qwen3.5-4b"), "{refused}");
    assert!(refused.contains("loaded"), "{refused}");
}

// ---------------------------------------------------------------------------
// the wire
// ---------------------------------------------------------------------------

#[test]
fn both_listing_urls_are_found_from_any_spelling_of_a_base_url() {
    // One `--url` has to work for the provider's completions path and for both
    // listings, because an operator types it once.
    for base in [
        "http://127.0.0.1:1234",
        "http://127.0.0.1:1234/",
        "http://127.0.0.1:1234/v1",
        "http://127.0.0.1:1234/v1/chat/completions",
        "http://127.0.0.1:1234/v1/models",
        "http://127.0.0.1:1234/api/v0/models",
    ] {
        assert_eq!(
            confirm::models_url(base),
            "http://127.0.0.1:1234/v1/models",
            "{base}"
        );
        assert_eq!(
            confirm::loaded_url(base),
            "http://127.0.0.1:1234/api/v0/models",
            "{base}"
        );
    }
}

#[test]
fn a_listing_is_read_as_ids_in_the_order_the_server_gave_them() {
    let body = r#"{"object":"list","data":[{"id":"a","object":"model"},{"id":"b"}]}"#;
    assert_eq!(
        confirm::parse_listing(body).expect("listing"),
        vec!["a".to_owned(), "b".to_owned()]
    );
}

#[test]
fn a_body_that_is_not_a_listing_is_a_sentence_that_quotes_it() {
    // A proxy answering 200 with an error page is the shape this catches, and the
    // operator needs to see the page rather than "malformed".
    let said = confirm::parse_listing("<html>no</html>").expect_err("not a listing");
    assert!(said.contains("<html>"), "{said}");

    let long = format!("{{\"data\":[{}]}}", "x".repeat(500));
    let said = confirm::parse_listing(&long).expect_err("not a listing");
    assert!(said.contains("..."), "a long body is truncated: {said}");
}
