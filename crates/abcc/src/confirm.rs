//! 🚨 The model-confirmation check: **which model is actually going to answer.**
//!
//! `abcc-engine`'s provider deliberately does not do this, and the reason it does
//! not is the reason this module exists. Three behaviours of the serving stack,
//! the third measured while writing this file:
//!
//! * **LM Studio serves a request naming a model it does not have, using
//!   whichever model *is* loaded.** So a run configured for the champion and
//!   pointed at a server holding a 4B gets an answer, a `finish_reason`, a token
//!   count and a plausible transcript — from the wrong brain. Nothing in the
//!   response says so.
//! * **The bare `llama-server` names models by GGUF path**, so the id the server
//!   reports is not the id the operator typed, and an equality check on its own
//!   would refuse a correct setup.
//! * 🚨 **F495: LM Studio's `/v1/models` lists everything *downloaded*, not what
//!   is *loaded*.** Measured on this box with the champion resident: the
//!   `OpenAI`-dialect listing returned **27 ids**, of which 26 were on disk and
//!   not in memory. A check against that listing would therefore pass in exactly
//!   the case it exists to catch. **`/api/v0/models` carries `state`**, and in the
//!   same request one id said `loaded` and 26 said `not-loaded` — the positive
//!   control and the negative one in a single answer. So the loaded listing is
//!   asked for first, and the `OpenAI` one is the fallback.
//!
//! Between the first two, no socket settles it: one says a match cannot be
//! assumed and the other says a match cannot be spelled. **What settles it is an
//! operator-configured fingerprint** — a substring the operator asserts must
//! appear in the served id — which is why this is the binary's job and not the
//! provider's.
//!
//! ⚠ There is deliberately **no `--force`**. A check that can be waived by a flag
//! is a check that will be waived, and a run against an unconfirmed model
//! produces numbers that look exactly like good ones. The escape hatch is
//! configuring the fingerprint, which leaves a record of what was asserted.

use std::fmt;
use std::time::Duration;

use serde::Deserialize;

/// The substring the operator asserts must appear in the served model id.
pub const FINGERPRINT_ENV: &str = "ABCC_MODEL_FINGERPRINT";
/// The model to ask for, when `--model` is not given.
pub const MODEL_ENV: &str = "ABCC_MODEL";

/// How long to wait for a server to say what it is holding. This is a local
/// listing endpoint answering from memory; a server that cannot do it in this
/// long is not one a turn is going to survive.
const LISTING_TIMEOUT: Duration = Duration::from_secs(10);

/// Asking the server what it holds did not work.
#[derive(Debug, thiserror::Error)]
pub enum ConfirmError {
    #[error("asking {url} what it is serving: {detail}")]
    Transport { url: String, detail: String },
    #[error("{url} answered HTTP {code}: {body}")]
    Status {
        url: String,
        code: u16,
        body: String,
    },
    #[error("{url} answered something that is not a model listing: {detail}")]
    Malformed { url: String, detail: String },
}

/// What a server was able to tell us, which is **not the same question** from one
/// runtime to the next.
///
/// 🚨 This distinction is the whole of F495. Keeping the two apart in the type is
/// what stops a weaker answer being read as the stronger one — the failure that
/// would make this module decorative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// The server said which models are **resident**, and these are they —
    /// with the context window it has actually allocated for them, when the
    /// runtime reports one. LM Studio does; a bare `llama-server` does not.
    Loaded(Vec<String>, Option<u32>),
    /// The server listed what it *can* serve and did not say what it is holding.
    ///
    /// For a bare `llama-server` that is the same set, because it serves exactly
    /// what it was started with. For LM Studio it is emphatically not, which is
    /// why [`served`] asks for the loaded listing first and only falls back here.
    Available(Vec<String>),
}

impl Listing {
    #[must_use]
    pub fn ids(&self) -> &[String] {
        match self {
            Listing::Loaded(ids, _) | Listing::Available(ids) => ids,
        }
    }

    #[must_use]
    pub fn evidence(&self) -> Evidence {
        match self {
            Listing::Loaded(..) => Evidence::Loaded,
            Listing::Available(_) => Evidence::Listed,
        }
    }

    /// The context window the server has **actually allocated**, when it says.
    ///
    /// 🚨 **F818.** This number arrives in the same body [`parse_loaded`] reads,
    /// on an endpoint `served` already calls on every run, and it used to be
    /// thrown away. The only other place abcc could learn it is `n_ctx`, scraped
    /// out of the server's *error* body once a request had already been refused
    /// for exceeding it — a quantity arriving after the event it could have
    /// governed, which is the same shape as F812.
    ///
    /// `None` from a runtime that does not report one: [`Listing::Available`]
    /// never does, and a bare `llama-server` does not either. ⚠ Absent rather
    /// than zero (F494), because an unallocated window and an unreported one are
    /// different answers.
    #[must_use]
    pub fn window(&self) -> Option<u32> {
        match self {
            Listing::Loaded(_, window) => *window,
            Listing::Available(_) => None,
        }
    }
}

/// How good the answer behind a confirmation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// The server said this model is in memory.
    Loaded,
    /// The server listed the id and does not distinguish loaded from available.
    /// The best that runtime can say, and recorded as such rather than as more.
    Listed,
}

/// How a served model was recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum How {
    /// The server reports the id that was asked for.
    Exact,
    /// The operator asserted this substring and a served id carries it.
    Fingerprint(String),
}

/// Why nothing could be confirmed. Four sentences, because they need four
/// different things done about them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unconfirmed {
    /// The server is up and holding nothing at all.
    NothingLoaded,
    /// The server is up and can serve nothing at all.
    NothingServed,
    /// The id asked for is not there, and no fingerprint was configured to say
    /// what would count instead.
    NoFingerprint,
    /// A fingerprint was configured and nothing carries it.
    NoMatch { fingerprint: String },
}

impl fmt::Display for Unconfirmed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unconfirmed::NothingLoaded => f.write_str("the server has no model in memory"),
            Unconfirmed::NothingServed => f.write_str("the server offers no model at all"),
            Unconfirmed::NoFingerprint => write!(
                f,
                "that id is not among them, and no {FINGERPRINT_ENV} was configured to say what \
                 would count instead"
            ),
            Unconfirmed::NoMatch { fingerprint } => {
                write!(f, "nothing there contains {fingerprint:?}")
            }
        }
    }
}

/// What the check decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Confirmed {
        served: String,
        how: How,
        evidence: Evidence,
    },
    Unconfirmed {
        why: Unconfirmed,
        listing: Listing,
    },
}

impl Verdict {
    #[must_use]
    pub fn confirmed(&self) -> bool {
        matches!(self, Verdict::Confirmed { .. })
    }

    /// The sentence that goes on the log, either way.
    ///
    /// It names what was asked, what was there, how the two were matched **and
    /// how good the evidence was**, because `ModelCallStarted` records the
    /// *requested* id and this is the only place in the run that says which brain
    /// answered.
    #[must_use]
    pub fn note(&self, asked: &str) -> String {
        match self {
            Verdict::Confirmed {
                served,
                how,
                evidence,
            } => {
                let matched = match how {
                    How::Exact => "matched exactly".to_owned(),
                    How::Fingerprint(fp) => {
                        format!("matched on the operator's fingerprint {fp:?}")
                    }
                };
                format!(
                    "model confirmed: asked {asked}, {} {served}, {matched}",
                    match evidence {
                        Evidence::Loaded => "the server reports it has loaded",
                        // ⚠ Said out loud. On this runtime a listed id is not a
                        // resident one, and a run whose evidence was weaker
                        // should say so on the log rather than read the same as
                        // one whose evidence was not.
                        Evidence::Listed => "the server lists (without saying what it has loaded)",
                    }
                )
            }
            Verdict::Unconfirmed { why, listing } => format!(
                "model NOT confirmed: asked {asked}, {} [{}] — {why}",
                match listing.evidence() {
                    Evidence::Loaded => "loaded",
                    Evidence::Listed => "listed",
                },
                listing.ids().join(", ")
            ),
        }
    }
}

/// The decision, as a function of what was asked, what the operator asserted, and
/// what the server was able to say.
///
/// Pure, so the half that matters is testable without a server: every case below
/// is a shape one of the two runtimes actually produces.
#[must_use]
pub fn decide(asked: &str, fingerprint: Option<&str>, listing: &Listing) -> Verdict {
    let ids = listing.ids();
    let evidence = listing.evidence();
    if ids.is_empty() {
        return Verdict::Unconfirmed {
            why: match evidence {
                Evidence::Loaded => Unconfirmed::NothingLoaded,
                Evidence::Listed => Unconfirmed::NothingServed,
            },
            listing: listing.clone(),
        };
    }
    if let Some(hit) = ids.iter().find(|id| id.eq_ignore_ascii_case(asked)) {
        return Verdict::Confirmed {
            served: hit.clone(),
            how: How::Exact,
            evidence,
        };
    }
    let Some(fp) = fingerprint.map(str::trim).filter(|f| !f.is_empty()) else {
        return Verdict::Unconfirmed {
            why: Unconfirmed::NoFingerprint,
            listing: listing.clone(),
        };
    };
    // ⚠ Case-insensitive `contains`, because what is being matched is usually a
    // path fragment and Windows hands those back in whatever case the file has.
    let needle = fp.to_lowercase();
    ids.iter()
        .find(|id| id.to_lowercase().contains(&needle))
        .map_or_else(
            || Verdict::Unconfirmed {
                why: Unconfirmed::NoMatch {
                    fingerprint: fp.to_owned(),
                },
                listing: listing.clone(),
            },
            |hit| Verdict::Confirmed {
                served: hit.clone(),
                how: How::Fingerprint(fp.to_owned()),
                evidence,
            },
        )
}

/// Ask a server what it is holding, preferring the endpoint that knows.
///
/// 🚨 The loaded listing is tried first and the `OpenAI` one is the fallback, in
/// that order, because of F495: on LM Studio the second answers a different
/// question and answering it confidently is the failure this whole module exists
/// to prevent. A server without the first endpoint gets [`Listing::Available`],
/// which the verdict then says out loud.
///
/// # Errors
///
/// [`ConfirmError`] — the fallback did not answer, answered an error, or answered
/// something that is not a listing. A missing *loaded* endpoint is not an error;
/// it is the fallback's whole reason for existing.
pub fn served(base_url: &str, api_key: Option<&str>) -> Result<Listing, ConfirmError> {
    if let Ok(body) = fetch(&loaded_url(base_url), api_key)
        && let Ok(Some(loaded)) = parse_loaded(&body)
    {
        return Ok(Listing::Loaded(loaded, parse_window(&body)));
    }
    let url = models_url(base_url);
    let body = fetch(&url, api_key)?;
    parse_listing(&body)
        .map(Listing::Available)
        .map_err(|detail| ConfirmError::Malformed { url, detail })
}

fn fetch(url: &str, api_key: Option<&str>) -> Result<String, ConfirmError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(LISTING_TIMEOUT)
        .build()
        .map_err(|e| ConfirmError::Transport {
            url: url.to_owned(),
            detail: e.to_string(),
        })?;
    let mut request = client.get(url);
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }
    let response = request.send().map_err(|e| ConfirmError::Transport {
        url: url.to_owned(),
        detail: e.to_string(),
    })?;
    let code = response.status().as_u16();
    let body = response.text().map_err(|e| ConfirmError::Transport {
        url: url.to_owned(),
        detail: e.to_string(),
    })?;
    if (200..300).contains(&code) {
        Ok(body)
    } else {
        Err(ConfirmError::Status {
            url: url.to_owned(),
            code,
            body: truncate(&body),
        })
    }
}

/// The ids a listing reports as resident, or `None` if this body does not answer
/// that question at all.
///
/// ⚠ `None` and `Some(vec![])` are different answers and are kept apart: the
/// first is *this server does not say*, the second is *this server says nothing
/// is loaded*. Folding them together would turn an unanswerable question into a
/// confident refusal.
///
/// # Errors
///
/// The reason the body could not be read, as a sentence.
pub fn parse_loaded(body: &str) -> Result<Option<Vec<String>>, String> {
    #[derive(Deserialize)]
    struct Listed {
        data: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        id: String,
        state: Option<String>,
    }
    let listed: Listed =
        serde_json::from_str(body).map_err(|e| format!("{e} (body was {})", truncate(body)))?;
    if listed.data.iter().all(|e| e.state.is_none()) {
        return Ok(None);
    }
    Ok(Some(
        listed
            .data
            .into_iter()
            .filter(|e| e.state.as_deref() == Some("loaded"))
            .map(|e| e.id)
            .collect(),
    ))
}

/// The context window the server has allocated for what it is holding, read from
/// the same body [`parse_loaded`] takes the ids out of.
///
/// 🚨 **⚠ `loaded_context_length`, never `max_context_length`.** They sit beside
/// each other in every entry, and on the champion they read **40960** and
/// **262144**: the second is the *model's* ceiling, **6.4×** the window actually
/// served. Budgeting against it would be wrong in the dangerous direction and
/// would look right on any box whose operator happened to load a large context.
/// This is the same join hazard Sweep B recorded — join on the server's
/// `loaded_context_length`, never on what the model says it could hold.
///
/// ⚠ Only an entry the server calls `loaded` is consulted. A listing that does
/// not distinguish loaded from available cannot answer this question, and gets
/// `None` rather than the first number in the file.
#[must_use]
pub fn parse_window(body: &str) -> Option<u32> {
    #[derive(Deserialize)]
    struct Listed {
        data: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        state: Option<String>,
        loaded_context_length: Option<u32>,
    }
    let listed: Listed = serde_json::from_str(body).ok()?;
    listed
        .data
        .into_iter()
        .filter(|e| e.state.as_deref() == Some("loaded"))
        .find_map(|e| e.loaded_context_length)
}

/// The ids in an `OpenAI`-dialect model listing.
///
/// # Errors
///
/// The reason the body could not be read as one, as a sentence.
pub fn parse_listing(body: &str) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct Listed {
        data: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        id: String,
    }
    let listed: Listed =
        serde_json::from_str(body).map_err(|e| format!("{e} (body was {})", truncate(body)))?;
    Ok(listed.data.into_iter().map(|e| e.id).collect())
}

/// The `OpenAI`-dialect listing endpoint, tolerating the same spellings of a base
/// URL that the provider's completions URL does.
#[must_use]
pub fn models_url(base: &str) -> String {
    format!("{}/v1/models", root(base))
}

/// LM Studio's own listing, which is the one that carries `state`.
#[must_use]
pub fn loaded_url(base: &str) -> String {
    format!("{}/api/v0/models", root(base))
}

/// The server root, whichever of the four spellings of it the operator gave.
fn root(base: &str) -> &str {
    let base = base.trim_end_matches('/');
    for tail in [
        "/v1/chat/completions",
        "/api/v0/models",
        "/v1/models",
        "/v1",
    ] {
        if let Some(root) = base.strip_suffix(tail) {
            return root;
        }
    }
    base
}

fn truncate(body: &str) -> String {
    const CAP: usize = 200;
    if body.len() <= CAP {
        return body.to_owned();
    }
    let mut end = CAP;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &body[..end])
}
