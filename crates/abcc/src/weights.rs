//! 🚨 **The model weights: the one ungated input, and the one control no donor
//! in the family has.**
//!
//! ADR-0014 §6, W7's F420. Every other dependency this binary has passes a
//! supply-chain gate — `cargo deny` over 297 crates, a license allow-list, a
//! source allow-list, a yanked-crate refusal. The model does not. It is
//! downloaded once by a separate program, it is the largest input by four orders
//! of magnitude, and until this module nothing in the workspace had ever looked
//! at it. The control is ADR-0014's: **record the digest at first pull and
//! compare it on every start, surfacing a mismatch as a run-visible event.**
//!
//! ## What was measured before any of this was designed, 2026-09-11
//!
//! 🚨 **The serving stack does not offer a digest.** Both listings were probed
//! against LM Studio with the service up: `/v1/models` returns `id`, `object`
//! and `owned_by`; `/api/v0/models` adds `type`, `publisher`, `arch`,
//! `quantization`, `state`, `max_context_length` and `capabilities`. **Neither
//! carries a digest, a length or a path** — the API identifies a model by the
//! name, which is the thing under suspicion. ADR-0014 says *manifest digest*
//! because ollama has one; this stack does not, so the digest has to be of the
//! file.
//!
//! 🚨 **A full SHA-256 of the champion is 51.9 s** — 12.67 GiB at ~250 MB/s,
//! measured on this box. That single number is the whole design: a check costing
//! most of a minute on every `abcc run` is a check that gets turned off, so a
//! start compares what a `stat` can see and [`WeightsOutcome`] keeps that
//! answer from reading like the expensive one.
//!
//! ## What it does and does not buy
//!
//! ✅ It catches the threat the row names — *a substituted model at first pull* —
//! and it catches a silent re-download, a quantization swapped underneath the
//! same name, and a truncated file.
//!
//! ⚠ It does **not** catch an adversary who rewrites the file preserving both
//! its length and its modification time, unless `abcc weights --verify` is run.
//! ⚠ A first pin **trusts what is there**: it records the bytes, it cannot
//! vouch for them. Both admissions are in `SECURITY.md` rather than only here.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use abcc_core::event::WeightsOutcome;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Points at the file behind the configured model, when the layout cannot be
/// guessed. ⚠ **The operator's assertion**, exactly like `ABCC_MODEL_FINGERPRINT`
/// — and for the same reason: no socket settles which bytes a name refers to.
pub const WEIGHTS_ENV: &str = "ABCC_MODEL_WEIGHTS";

/// Where a runtime keeps its models, when it is not the platform default.
pub const MODELS_DIR_ENV: &str = "ABCC_MODELS_DIR";

/// How much is read at a time. 8 MiB rather than a page: the measured rate is
/// 250 MB/s and this is a sequential read of a file larger than any cache, so
/// the syscall count is the only part worth tuning.
const CHUNK: usize = 8 * 1024 * 1024;

/// Working with the pin file itself failed. ⚠ A failure here is **not** a
/// mismatch and must never be reported as one — the difference between *the
/// weights changed* and *the pin could not be read* is the difference between an
/// alarm and a chore.
#[derive(Debug, thiserror::Error)]
pub enum WeightsError {
    #[error("reading the weights pin at {path}: {detail}")]
    Read { path: String, detail: String },
    #[error("writing the weights pin at {path}: {detail}")]
    Write { path: String, detail: String },
}

/// The cheap half: what a directory entry says about the file.
///
/// Length and modification time, and deliberately nothing derived from the
/// contents. It is what a start can afford.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub len: u64,
    /// Unix milliseconds. `None` on a filesystem that does not report one, which
    /// makes every start recompute rather than assume — the safe direction.
    pub modified_ms: Option<i64>,
}

impl Identity {
    /// # Errors
    ///
    /// Fails if the file cannot be stat-ed.
    pub fn of(path: &Path) -> io::Result<Identity> {
        let meta = fs::metadata(path)?;
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok());
        Ok(Identity {
            len: meta.len(),
            modified_ms,
        })
    }

    /// Whether this is, as far as a `stat` can tell, the same file.
    ///
    /// ⚠ **A missing timestamp is never a match.** Two files of equal length on
    /// a filesystem that reports no `mtime` would otherwise compare equal, and
    /// the whole point of the cheap half is that it errs towards doing the
    /// expensive one.
    #[must_use]
    pub fn looks_unchanged(&self, pinned: &Identity) -> bool {
        match (self.modified_ms, pinned.modified_ms) {
            (Some(a), Some(b)) => self.len == pinned.len && a == b,
            _ => false,
        }
    }
}

/// What was recorded for one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    /// The file the digest was taken of, so a pin that starts failing says which
    /// path it is talking about.
    pub path: String,
    pub identity: Identity,
    /// Lowercase hex SHA-256 of the whole file.
    pub digest: String,
    pub pinned_at_ms: i64,
}

/// Every pin on this machine, keyed by model id.
///
/// 🚨 **Per machine, not per repository.** `Home` is per-repository because a log
/// is about one repository's work; the weights are one file serving every
/// repository on the box, and a per-repository pin would mean each new checkout
/// silently pins whatever is there and the second one never learns what the
/// first knew.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pins {
    #[serde(default)]
    models: std::collections::BTreeMap<String, Pin>,
}

impl Pins {
    /// Read the pin file, or an empty set if there is not one yet.
    ///
    /// # Errors
    ///
    /// Fails if the file exists and cannot be read or parsed. ⚠ It does not fall
    /// back to empty on a parse error: a corrupt pin file silently becoming *no
    /// pins* would re-pin the next start and report `Pinned`, which is the one
    /// outcome that looks like success.
    pub fn load(path: &Path) -> Result<Pins, WeightsError> {
        match fs::read_to_string(path) {
            Ok(body) => serde_json::from_str(&body).map_err(|e| WeightsError::Read {
                path: path.display().to_string(),
                detail: e.to_string(),
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Pins::default()),
            Err(e) => Err(WeightsError::Read {
                path: path.display().to_string(),
                detail: e.to_string(),
            }),
        }
    }

    /// # Errors
    ///
    /// Fails if the directory cannot be created or the file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), WeightsError> {
        let write = |detail: String| WeightsError::Write {
            path: path.display().to_string(),
            detail,
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| write(e.to_string()))?;
        }
        let body = serde_json::to_string_pretty(self).map_err(|e| write(e.to_string()))?;
        fs::write(path, body).map_err(|e| write(e.to_string()))
    }

    #[must_use]
    pub fn get(&self, model: &str) -> Option<&Pin> {
        self.models.get(model)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.models.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    pub fn set(&mut self, model: impl Into<String>, pin: Pin) {
        self.models.insert(model.into(), pin);
    }

    /// Drop this model's pin, so the next check is a first sight.
    ///
    /// ⚠ The only way to re-pin, and it is deliberately not *overwrite the
    /// digest*: going back through [`check`] means one recipe decides what a pin
    /// contains (F330), and the operator's log line says `pinned` rather than a
    /// second word nobody else writes.
    pub fn forget(&mut self, model: &str) {
        self.models.remove(model);
    }
}

/// What one check found, beside the event it becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub outcome: WeightsOutcome,
    pub digest: Option<String>,
    /// Set when the caller must write the pin file back.
    pub pin: Option<Pin>,
}

impl Checked {
    /// Whether an operator has to do something about this.
    #[must_use]
    pub fn alarming(&self) -> bool {
        matches!(self.outcome, WeightsOutcome::Changed { .. })
    }
}

/// How hard to look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    /// Compare the directory entry; read the file only when it has moved. What a
    /// run start can afford.
    Cheap,
    /// Read the whole file. 51.9 s on the champion, and the only thing that
    /// answers the question the row actually asks.
    Full,
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Effort::Cheap => "cheap",
            Effort::Full => "full",
        })
    }
}

/// Whether [`check`] is about to read the whole file — **51.9 s on the
/// champion**, measured; 54 s through this module's own digest, within 4% of
/// `sha256sum` over the same 12.67 GiB.
///
/// It exists so a caller can put a sentence on the screen *before* the pause
/// rather than explaining it afterwards. ⚠ **A first sight reads the file too**,
/// which is the case that is easy to forget: `Effort::Cheap` is cheap only once
/// there is something to be cheap against.
///
/// 🚨 It re-derives rather than being told, so `tests/weights.rs` asserts it
/// agrees with what `check` actually did on every arm. Two functions answering
/// one question is F392's shape, and the defence is that the test fails when
/// they disagree rather than that nobody expects them to.
#[must_use]
pub fn reads_the_file(pins: &Pins, model: &str, effort: Effort) -> bool {
    // 🚨 **Locating comes FIRST, and the agreement test is what said so.** With
    // the effort and the pin checked ahead of it, an unlocatable model was
    // predicted to read — so `abcc weights` announced a 54-second pause and then
    // reported `unchecked` immediately. `check` reads nothing it cannot find,
    // and neither may this.
    let Ok(path) = locate(model) else {
        return false;
    };
    let Ok(identity) = Identity::of(&path) else {
        return false;
    };
    if effort == Effort::Full {
        return true;
    }
    let Some(pin) = pins.get(model) else {
        // No pin: the cheap path has nothing to be cheap against and falls
        // through to the digest.
        return true;
    };
    !identity.looks_unchanged(&pin.identity)
}

/// Compare the weights behind `model` against their pin.
///
/// 🚨 **An absent file is [`WeightsOutcome::Unlocated`] and never a pass.** A
/// control that cannot find its subject has to say so: the alternative is a run
/// whose log carries no weights line at all, which reads exactly like a run on a
/// build that predates this module.
pub fn check(pins: &mut Pins, model: &str, effort: Effort, now_ms: i64) -> Checked {
    let path = match locate(model) {
        Ok(path) => path,
        Err(why) => {
            return Checked {
                outcome: WeightsOutcome::Unlocated { why },
                digest: None,
                pin: None,
            };
        }
    };

    let identity = match Identity::of(&path) {
        Ok(identity) => identity,
        Err(e) => {
            return Checked {
                outcome: WeightsOutcome::Unlocated {
                    why: format!("{}: {e}", path.display()),
                },
                digest: None,
                pin: None,
            };
        }
    };

    let pinned = pins.get(model).cloned();

    // The cheap exit, and the only one that reports without reading the bytes.
    if let (Effort::Cheap, Some(pin)) = (effort, pinned.as_ref())
        && identity.looks_unchanged(&pin.identity)
    {
        return Checked {
            outcome: WeightsOutcome::Unchanged,
            digest: Some(pin.digest.clone()),
            pin: None,
        };
    }

    let digest = match digest_of(&path) {
        Ok(digest) => digest,
        Err(e) => {
            return Checked {
                outcome: WeightsOutcome::Unlocated {
                    why: format!("reading {}: {e}", path.display()),
                },
                digest: None,
                pin: None,
            };
        }
    };

    let fresh = Pin {
        path: path.display().to_string(),
        identity,
        digest: digest.clone(),
        pinned_at_ms: now_ms,
    };

    match pinned {
        None => {
            pins.set(model, fresh.clone());
            Checked {
                outcome: WeightsOutcome::Pinned,
                digest: Some(digest),
                pin: Some(fresh),
            }
        }
        Some(pin) if pin.digest == digest => {
            // ⚠ The pin is rewritten even on a match, because a file whose
            // timestamp moved without its contents changing — a re-download of
            // the same bytes, a backup restore — would otherwise pay 51.9 s on
            // every start forever.
            pins.set(model, fresh.clone());
            Checked {
                outcome: WeightsOutcome::Verified,
                digest: Some(digest),
                pin: Some(fresh),
            }
        }
        Some(pin) => Checked {
            // 🚨 **The pin is NOT updated.** Overwriting it here would make the
            // alarm fire exactly once and then describe the substitute as the
            // reference. Re-pinning is an operator's deliberate act.
            outcome: WeightsOutcome::Changed { was: pin.digest },
            digest: Some(digest),
            pin: None,
        },
    }
}

/// Lowercase hex SHA-256 of a whole file.
///
/// # Errors
///
/// Fails if the file cannot be opened or read.
pub fn digest_of(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Find the file behind a model id.
///
/// Three sources, in the order of how much they are asserting:
///
/// 1. [`WEIGHTS_ENV`] names the file outright — the operator's assertion, and
///    the only one that works for a bare `llama-server` naming models by path.
/// 2. The id is already a path that exists.
/// 3. The id is a relative path under the models directory, which is what LM
///    Studio's ids are: `publisher/repo/file.gguf`.
///
/// ⚠ **It returns why it failed rather than `None`.** *No `ABCC_MODEL_WEIGHTS`
/// and nothing under the models directory* and *the models directory does not
/// exist* want different things done about them, and an operator reading a log
/// line that says only `unchecked` will do neither.
fn locate(model: &str) -> Result<PathBuf, String> {
    if let Some(explicit) = std::env::var_os(WEIGHTS_ENV) {
        let path = PathBuf::from(explicit);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "{WEIGHTS_ENV} is set to {} and that is not a file",
                path.display()
            ))
        };
    }

    let direct = Path::new(model);
    if direct.is_file() {
        return Ok(direct.to_path_buf());
    }

    let dir = models_dir().ok_or_else(|| {
        format!("no {WEIGHTS_ENV} and no models directory to search; set one of them")
    })?;
    if !dir.is_dir() {
        return Err(format!(
            "the models directory {} does not exist; set {WEIGHTS_ENV}",
            dir.display()
        ));
    }
    let candidate = dir.join(model);
    if candidate.is_file() {
        return Ok(candidate);
    }
    Err(format!(
        "nothing at {} and no {WEIGHTS_ENV}; the id may not be a path under the models directory",
        candidate.display()
    ))
}

/// Where a runtime keeps its models.
///
/// [`MODELS_DIR_ENV`] first, then LM Studio's default. ⚠ Only LM Studio's,
/// because that is the runtime this project has measured on — guessing at four
/// other layouts would be four claims nobody has tested, and the env var is the
/// answer for every one of them.
fn models_dir() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(MODELS_DIR_ENV) {
        return Some(PathBuf::from(explicit));
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".lmstudio").join("models"))
}

/// The host clock, in the unit the log uses.
///
/// ⚠ `check` takes the time as an argument rather than reading it, so a test can
/// assert what was pinned and when without owning the clock — the store keeps
/// its own copy of this for the same reason.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Where the pins live: beside the per-repository log directories, not inside
/// one. See [`Pins`].
#[must_use]
pub fn pin_path(data_root: &Path) -> PathBuf {
    data_root.join("weights.json")
}
