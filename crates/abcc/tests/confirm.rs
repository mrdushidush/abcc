//! The model-confirmation decision, without a socket.
//!
//! 🚨 The headline case is [`a_loaded_model_that_is_not_the_one_asked_for_is_refused`]:
//! LM Studio answers a request naming a model it does not have by using whichever
//! model *is* loaded, and every number that comes back looks exactly like a good
//! one. This is the check that stops a whole run being about the wrong brain, so
//! its decision is a pure function and every shape below is one of the two
//! runtimes' real output.

use abcc::confirm::{self, How, Unconfirmed, Verdict};

const CHAMPION: &str = "qwen3.6-35b-a3b-mtp@iq3_s";
/// What the bare `llama-server` reports: a path, not an id.
const BY_PATH: &str = "C:/models/unsloth/qwen3.6-35B-A3B-MTP-IQ3_S.gguf";

fn served(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| (*s).to_owned()).collect()
}

// ---------------------------------------------------------------------------
// the decision
// ---------------------------------------------------------------------------

#[test]
fn the_id_the_operator_asked_for_is_confirmed_without_any_configuration() {
    let verdict = confirm::decide(CHAMPION, None, &served(&["qwen3-4b", CHAMPION]));
    assert_eq!(
        verdict,
        Verdict::Confirmed {
            served: CHAMPION.to_owned(),
            how: How::Exact
        }
    );
    assert!(verdict.confirmed());
}

#[test]
fn a_loaded_model_that_is_not_the_one_asked_for_is_refused() {
    // 🚨 The whole point. The server is up, it is holding something, and it would
    // happily answer — with the wrong brain, and with a plausible transcript.
    let verdict = confirm::decide(CHAMPION, None, &served(&["qwen3-4b"]));
    assert_eq!(
        verdict,
        Verdict::Unconfirmed {
            why: Unconfirmed::NoFingerprint,
            served: served(&["qwen3-4b"]),
        }
    );
    assert!(!verdict.confirmed());
}

#[test]
fn the_bare_servers_gguf_path_is_confirmed_only_by_the_operators_fingerprint() {
    // Nothing about a path equals an id, so without the assertion this is a
    // refusal...
    assert!(!confirm::decide(CHAMPION, None, &served(&[BY_PATH])).confirmed());
    // ...and with it, it is the operator saying what would count.
    assert_eq!(
        confirm::decide(CHAMPION, Some("A3B-MTP-IQ3_S"), &served(&[BY_PATH])),
        Verdict::Confirmed {
            served: BY_PATH.to_owned(),
            how: How::Fingerprint("A3B-MTP-IQ3_S".to_owned()),
        }
    );
}

#[test]
fn a_fingerprint_matches_regardless_of_case_because_a_path_is_not_an_id() {
    // Windows hands a path back in whatever case the file has.
    assert!(confirm::decide(CHAMPION, Some("a3b-mtp-iq3_s"), &served(&[BY_PATH])).confirmed());
    assert!(
        confirm::decide(
            CHAMPION,
            Some("A3B-MTP-IQ3_S"),
            &served(&["...a3b-mtp-iq3_s..."])
        )
        .confirmed()
    );
}

#[test]
fn a_fingerprint_that_matches_nothing_is_not_a_pass() {
    let verdict = confirm::decide(CHAMPION, Some("qwen3.8-27b"), &served(&[BY_PATH]));
    assert_eq!(
        verdict,
        Verdict::Unconfirmed {
            why: Unconfirmed::NoMatch {
                fingerprint: "qwen3.8-27b".to_owned()
            },
            served: served(&[BY_PATH]),
        }
    );
}

#[test]
fn a_blank_fingerprint_is_no_fingerprint_rather_than_a_substring_of_everything() {
    // ⚠ `"".contains` is true of every string, so an empty assertion left in the
    // environment would confirm anything at all. It is treated as absent.
    for blank in ["", "   "] {
        assert_eq!(
            confirm::decide(CHAMPION, Some(blank), &served(&["qwen3-4b"])),
            Verdict::Unconfirmed {
                why: Unconfirmed::NoFingerprint,
                served: served(&["qwen3-4b"]),
            },
            "{blank:?}"
        );
    }
}

#[test]
fn a_server_holding_nothing_is_its_own_sentence() {
    let verdict = confirm::decide(CHAMPION, Some("anything"), &[]);
    assert_eq!(
        verdict,
        Verdict::Unconfirmed {
            why: Unconfirmed::NothingServed,
            served: Vec::new(),
        }
    );
    assert!(verdict.note(CHAMPION).contains("no model at all"));
}

// ---------------------------------------------------------------------------
// what reaches the log
// ---------------------------------------------------------------------------

#[test]
fn the_note_names_what_was_asked_and_what_was_actually_served() {
    // This sentence is the only place the run says which brain answered:
    // `ModelCallStarted` records the id that was *requested*.
    let confirmed = confirm::decide(CHAMPION, Some("A3B-MTP"), &served(&[BY_PATH])).note(CHAMPION);
    assert!(confirmed.contains(CHAMPION), "{confirmed}");
    assert!(confirmed.contains(BY_PATH), "{confirmed}");
    assert!(confirmed.contains("fingerprint"), "{confirmed}");

    let refused = confirm::decide(CHAMPION, None, &served(&["qwen3-4b"])).note(CHAMPION);
    assert!(refused.contains("NOT confirmed"), "{refused}");
    assert!(refused.contains("qwen3-4b"), "{refused}");
}

// ---------------------------------------------------------------------------
// the wire
// ---------------------------------------------------------------------------

#[test]
fn the_listing_url_tolerates_the_three_spellings_of_a_base_url() {
    // The same three the provider's completions URL accepts, so one `--url`
    // works for both.
    for base in [
        "http://127.0.0.1:1234",
        "http://127.0.0.1:1234/",
        "http://127.0.0.1:1234/v1",
        "http://127.0.0.1:1234/v1/chat/completions",
        "http://127.0.0.1:1234/v1/models",
    ] {
        assert_eq!(
            confirm::models_url(base),
            "http://127.0.0.1:1234/v1/models",
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
