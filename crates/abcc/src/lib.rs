//! `abcc` — the binary. Where the six crates meet a person.
//!
//! Every other crate in this workspace holds one thing and refuses to hold a
//! second: `abcc-core` has the words, `abcc-store` writes them down, `abcc-vcs`
//! gives an attempt somewhere to work, `abcc-engine` drives a phase against a
//! model, `abcc-drive` composes those four into one attempt, and `abcc-tui`
//! reads the log back. **This crate owns the things that are only true of a
//! process**: where the log lives, which model is really loaded, what an operator
//! typed, and the fact that a run started at all.
//!
//! Four of those are not glue, and each closes a gap the library crates left
//! open on purpose:
//!
//! 1. 🚨 [`confirm`] — **the model-confirmation check.** `abcc-engine` does not
//!    verify that the server loaded the model it was asked for, because no socket
//!    can settle it; what settles it is an operator-configured fingerprint, which
//!    is configuration and therefore the binary's.
//! 2. [`feed::StoreFeed`] — the durable [`abcc_tui::Feed`]. `abcc-tui` will not
//!    depend on `abcc-store`, so the adapter belongs here.
//! 3. [`desk`] — the console edge that owns the `ControlHandle`, and the only
//!    thing in the binary that writes `ControlRequested`.
//! 4. [`Event::RunStarted`] — `PLAN.md` §5 requires the run's mode as its first
//!    event, and until this crate existed **nothing in the workspace wrote one**.
//!
//! The other measurement §5 requires from the first milestone is
//! `ReviewRecorded`, and it is the same story: `abcc-tui` already *shows* review
//! minutes, and until `abcc review` there was nobody to write one. W13's ladder
//! is measured in human review minutes per merged change, and a ladder whose
//! baseline starts at Self-Host is unfalsifiable.

use std::io::Write;

pub mod breaker;
pub mod cli;
pub mod confirm;
pub mod desk;
pub mod feed;
pub mod fleet;
pub mod fun;
pub mod home;
pub mod ops;
mod paint;
pub mod pulse;
pub mod run;

pub use cli::{Command, Invocation};
pub use home::Home;

/// Who a run and an operator verb are attributed to.
pub const OPERATOR_ENV: &str = "ABCC_OPERATOR";

/// Anything that stops a subcommand.
///
/// [`AppError::Refused`] is the one that is not a fault: the operator asked for
/// something the state of the world does not permit — an unconfirmed model, a
/// task that is not there — and the right response is a sentence, not a
/// backtrace.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Cli(#[from] cli::CliError),
    #[error("the log: {0}")]
    Store(#[from] abcc_store::StoreError),
    #[error("git: {0}")]
    Vcs(#[from] abcc_vcs::VcsError),
    #[error("{0}")]
    Drive(#[from] abcc_drive::DriveError),
    #[error("{0}")]
    Fleet(#[from] abcc_fleet::FleetError),
    #[error("the model server: {0}")]
    Provider(#[from] abcc_engine::provider::ProviderError),
    #[error("{0}")]
    Confirm(#[from] confirm::ConfirmError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Refused(String),
}

impl AppError {
    /// What the process exits with. `2` for a usage error, the way every other
    /// command-line tool spells it; `1` for everything else; `0` for `--help`,
    /// which is not a failure.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            AppError::Cli(cli::CliError::Help) => 0,
            AppError::Cli(cli::CliError::Usage(_)) => 2,
            _ => 1,
        }
    }
}

/// Parse, dispatch, and write everything an operator sees to `out`.
///
/// # Errors
///
/// [`AppError`] — including [`cli::CliError::Help`], which the caller prints and
/// exits zero on.
pub fn main_with<I: IntoIterator<Item = String>>(
    args: I,
    out: &mut impl Write,
) -> Result<(), AppError> {
    dispatch(&cli::parse(args)?, out)
}

/// Do what an invocation says.
///
/// Separate from [`main_with`] so a test can drive a subcommand without going
/// through a shell's quoting rules, and so the parse can be tested on its own.
///
/// # Errors
///
/// [`AppError`], whatever the subcommand could not do.
pub fn dispatch(invocation: &Invocation, out: &mut impl Write) -> Result<(), AppError> {
    match &invocation.command {
        Command::Where => ops::show_where(invocation, out),
        Command::Task { prompt, title } => ops::task(invocation, prompt, title.as_deref(), out),
        Command::Board => ops::board(invocation, out),
        Command::Fun => fun::report(invocation, out),
        Command::Run(run) => run::attempt(invocation, run, out),
        Command::Fleet(args) => fleet::sortie(invocation, args, out),
        Command::Breaker { model, base_url } => {
            breaker::report(invocation, model.as_deref(), base_url.as_deref(), out)
        }
        Command::Watch { theme } => ops::watch(invocation, *theme),
        Command::Paint {
            sprites,
            px,
            size,
            corpus,
        } => paint::paint(invocation, sprites.as_deref(), *px, *size, *corpus, out),
        Command::Check {
            model,
            base_url,
            fingerprint,
        } => ops::check(
            model.as_deref(),
            base_url.as_deref(),
            fingerprint.as_deref(),
            out,
        ),
        Command::Review {
            change,
            seconds,
            by,
            crossed_boundary,
        } => ops::review(
            invocation,
            change,
            *seconds,
            by.as_deref(),
            *crossed_boundary,
            out,
        ),
        Command::Accept { task, note } => {
            ops::finish(invocation, *task, note.as_deref(), ops::Finish::Accept, out)
        }
        Command::Reject { task, note } => {
            ops::finish(invocation, *task, note.as_deref(), ops::Finish::Reject, out)
        }
    }
}

/// The operator's name, for the records that are about a person.
#[must_use]
pub fn operator() -> String {
    std::env::var(OPERATOR_ENV)
        .ok()
        .filter(|w| !w.trim().is_empty())
        .unwrap_or_else(|| "operator".to_owned())
}
