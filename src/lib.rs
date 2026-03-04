//! **fest** — a fast mutation-testing tool for Python.
//!
//! This crate provides both a library API and a CLI binary for
//! generating mutants from Python source code, running a test suite
//! against each mutant, and reporting which mutants survived.

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
/// It will be wired up to the full pipeline in a later task.
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
pub fn run() -> Result<(), Error> {
    Ok(())
}
