//! **fest** — a fast mutation-testing tool for Python.
//!
//! This crate provides both a library API and a CLI binary for
//! generating mutants from Python source code, running a test suite
//! against each mutant, and reporting which mutants survived.

pub mod cli;
pub mod config;
pub mod coverage;
pub mod error;
pub mod mutation;
pub mod report;
pub mod runner;

pub use error::Error;

/// Run the fest pipeline to completion.
///
/// This is the top-level entry point invoked by the CLI binary.
/// It accepts the parsed [`cli::RunArgs`] from the command line so that
/// CLI overrides can be merged with the loaded configuration.
///
/// # Errors
///
/// Returns [`Error`] if any stage of the pipeline fails (configuration,
/// mutation, coverage, test-running, or report generation).
#[inline]
#[allow(
    clippy::missing_const_for_fn,
    reason = "will perform non-const work once wired up"
)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is intentional; will return errors once wired up"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "RunArgs is consumed; pass-by-value is intentional for the final API"
)]
pub fn run(_args: cli::RunArgs) -> Result<(), Error> {
    Ok(())
}
