//! 🚨 The model-confirmation check: **which model actually answered.**
//!
//! `abcc-engine`'s provider deliberately does not do this, and the reason it does
//! not is the reason this module exists. Two behaviours of the serving stack,
//! both found during Phase 1:
//!
//! * **LM Studio serves a request naming a model it does not have, using
//!   whichever model *is* loaded.** So a run configured for the champion and
//!   pointed at a server holding a 4B gets an answer, a `finish_reason`, a token
//!   count and a plausible transcript — from the wrong brain. Nothing in the
//!   response says so.
//! * **The bare `llama-server` names models by GGUF path**, so the id the server
//!   reports is not the id the operator typed, and an equality check on its own
//!   would refuse a correct setup.
//!
//! Between those two, no socket settles it: the first says a match cannot be
//! assumed and the second says a match cannot be spelled. **What settles it is an
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

/// How a served model was recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum How {
    /// The server reports the id that was asked for.
    Exact,
    /// The operator asserted this substring and a served id carries it.
    Fingerprint(String),
}

/// Why nothing could be confirmed. Three sentences, because they need three
/// different things done about them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unconfirmed {
    /// The server is up and holding nothing.
    NothingServed,
    /// The id asked for is not being served, and no fingerprint was configured to
    /// say what would count instead.
    NoFingerprint,
    /// A fingerprint was configured and no served id carries it.
    NoMatch { fingerprint: String },
}

impl fmt::Display for Unconfirmed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unconfirmed::NothingServed => f.write_str("the server is holding no model at all"),
            Unconfirmed::NoFingerprint => write!(
                f,
                "the server is not holding that id, and no {FINGERPRINT_ENV} was configured to \
                 say what would count instead"
            ),
            Unconfirmed::NoMatch { fingerprint } => {
                write!(f, "no served id contains {fingerprint:?}")
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
    },
    Unconfirmed {
        why: Unconfirmed,
        served: Vec<String>,
    },
}

impl Verdict {
    #[must_use]
    pub fn confirmed(&self) -> bool {
        matches!(self, Verdict::Confirmed { .. })
    }

    /// The sentence that goes on the log, either way.
    ///
    /// It names what was asked, what was served and how the two were matched,
    /// because `ModelCallStarted` records the *requested* id and this is the only
    /// place in the run that says which brain actually answered.
    #[must_use]
    pub fn note(&self, asked: &str) -> String {
        match self {
            Verdict::Confirmed {
                served,
                how: How::Exact,
            } => format!("model confirmed: asked {asked}, served {served}, matched exactly"),
            Verdict::Confirmed {
                served,
                how: How::Fingerprint(fp),
            } => format!(
                "model confirmed: asked {asked}, served {served}, matched on the operator's \
                 fingerprint {fp:?}"
            ),
            Verdict::Unconfirmed { why, served } => format!(
                "model NOT confirmed: asked {asked}, served [{}] — {why}",
                served.join(", ")
            ),
        }
    }
}

/// The decision, as a function of what was asked, what the operator asserted, and
/// what the server says it is holding.
///
/// Pure, so the half that matters is testable without a server: every case below
/// is a shape one of the two runtimes actually produces.
#[must_use]
pub fn decide(asked: &str, fingerprint: Option<&str>, served: &[String]) -> Verdict {
    if served.is_empty() {
        return Verdict::Unconfirmed {
            why: Unconfirmed::NothingServed,
            served: Vec::new(),
        };
    }
    if let Some(hit) = served.iter().find(|id| id.eq_ignore_ascii_case(asked)) {
        return Verdict::Confirmed {
            served: hit.clone(),
            how: How::Exact,
        };
    }
    let Some(fp) = fingerprint.map(str::trim).filter(|f| !f.is_empty()) else {
        return Verdict::Unconfirmed {
            why: Unconfirmed::NoFingerprint,
            served: served.to_vec(),
        };
    };
    // ⚠ Case-insensitive `contains`, because what is being matched is usually a
    // path fragment and Windows hands those back in whatever case the file has.
    let needle = fp.to_lowercase();
    served
        .iter()
        .find(|id| id.to_lowercase().contains(&needle))
        .map_or_else(
            || Verdict::Unconfirmed {
                why: Unconfirmed::NoMatch {
                    fingerprint: fp.to_owned(),
                },
                served: served.to_vec(),
            },
            |hit| Verdict::Confirmed {
                served: hit.clone(),
                how: How::Fingerprint(fp.to_owned()),
            },
        )
}

/// Ask a server what it is holding.
///
/// # Errors
///
/// [`ConfirmError`] — the server did not answer, answered an error, or answered
/// something that is not a listing.
pub fn served_models(base_url: &str, api_key: Option<&str>) -> Result<Vec<String>, ConfirmError> {
    let url = models_url(base_url);
    let client = reqwest::blocking::Client::builder()
        .timeout(LISTING_TIMEOUT)
        .build()
        .map_err(|e| ConfirmError::Transport {
            url: url.clone(),
            detail: e.to_string(),
        })?;
    let mut request = client.get(&url);
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }
    let response = request.send().map_err(|e| ConfirmError::Transport {
        url: url.clone(),
        detail: e.to_string(),
    })?;
    let code = response.status().as_u16();
    let body = response.text().map_err(|e| ConfirmError::Transport {
        url: url.clone(),
        detail: e.to_string(),
    })?;
    if !(200..300).contains(&code) {
        return Err(ConfirmError::Status {
            url,
            code,
            body: truncate(&body),
        });
    }
    parse_listing(&body).map_err(|detail| ConfirmError::Malformed { url, detail })
}

/// The ids in an `OpenAI`-dialect model listing.
///
/// # Errors
///
/// The reason the body could not be read as one, as a sentence.
pub fn parse_listing(body: &str) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct Listing {
        data: Vec<Model>,
    }
    #[derive(Deserialize)]
    struct Model {
        id: String,
    }
    let listing: Listing =
        serde_json::from_str(body).map_err(|e| format!("{e} (body was {})", truncate(body)))?;
    Ok(listing.data.into_iter().map(|m| m.id).collect())
}

/// The listing endpoint, tolerating the same three spellings of a base URL that
/// the provider's completions URL does.
#[must_use]
pub fn models_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if let Some(root) = base.strip_suffix("/chat/completions") {
        format!("{root}/models")
    } else if base.ends_with("/models") {
        base.to_owned()
    } else if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    }
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
