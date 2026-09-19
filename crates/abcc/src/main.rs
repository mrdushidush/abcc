//! The entry point, and nothing else.
//!
//! Everything is in the library beside it so that the argument surface, the
//! model-confirmation decision and the operator's endings are testable without a
//! terminal. What is left here is the two things a `main` is actually for: where
//! the arguments come from, and what the process exits with.

use std::io::Write;
use std::process::ExitCode;

use abcc::{AppError, cli};

fn main() -> ExitCode {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match abcc::main_with(std::env::args().skip(1), &mut out) {
        Ok(()) => ExitCode::SUCCESS,
        // `--help` is not a failure, and neither is an empty invocation.
        Err(AppError::Cli(cli::CliError::Help)) => {
            let _ = write!(out, "{}", cli::USAGE);
            ExitCode::SUCCESS
        }
        // `--version` is not a failure either, and it prints the
        // `abcc <version>` shape the usage text opens with.
        Err(AppError::Cli(cli::CliError::Version(version))) => {
            let _ = writeln!(out, "abcc {version}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            let _ = out.flush();
            eprintln!("abcc: {e}");
            // A `u8` because that is what an exit status is; the mapping lives on
            // the error so every caller spells it the same way.
            ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(1))
        }
    }
}
