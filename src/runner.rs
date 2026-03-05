//! Test runner -- executing the test suite against each mutant.
//!
//! This module defines the [`Runner`] trait for mutant execution backends
//! and provides both a subprocess-based fallback and a pytest-plugin-based
//! primary implementation.
//!
//! The [`Runner`] trait is async, allowing backends to leverage
//! non-blocking I/O and timeouts. The [`SubprocessRunner`] spawns a
//! `pytest` process for each mutant, writing mutated source to a
//! temporary file and interpreting the exit code. The
//! [`PytestPluginRunner`] uses an embedded pytest plugin that
//! communicates over a Unix domain socket for faster in-process
//! module patching.

/// Pytest-plugin-based runner backend.
pub mod pytest_plugin;
/// Subprocess-based runner backend.
pub mod subprocess;

pub use pytest_plugin::PytestPluginRunner;
pub use subprocess::SubprocessRunner;

use crate::{
    Error,
    mutation::{Mutant, MutantResult},
};

/// Trait for mutant execution backends.
///
/// Implementors receive a [`Mutant`], the original source text of the
/// file being mutated, and the list of test IDs to run. They must
/// apply the mutation, execute the tests, and return a [`MutantResult`].
///
/// The trait uses native async fn in trait (stable since Rust 1.75).
pub trait Runner: Send + Sync {
    /// Run the test suite against a single mutant.
    ///
    /// # Parameters
    ///
    /// * `mutant` -- the mutation to apply.
    /// * `source` -- the **original** source text of the file; the mutant knows how to splice
    ///   itself via [`Mutant::apply_to_source`].
    /// * `tests` -- test IDs to execute (e.g. `test_foo.py::test_bar`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runner`] if the backend encounters an
    /// unrecoverable error (e.g. cannot create temp files or spawn a
    /// process).
    fn run_mutant(
        &self,
        mutant: &Mutant,
        source: &str,
        tests: &[String],
    ) -> impl Future<Output = Result<MutantResult, Error>> + Send;
}

// Re-export `Future` so that the trait definition compiles without
// requiring callers to import it themselves.
use core::future::Future;

/// Build a `PYTHONPATH` string that prepends `dir` to any existing value.
///
/// Both [`SubprocessRunner`] and [`PytestPluginRunner`] need to set
/// `PYTHONPATH` so that the mutated file (or plugin) takes import
/// priority. This shared helper keeps that logic in one place.
pub(crate) fn build_python_path(dir: &std::path::Path) -> String {
    let dir_str = dir.display().to_string();

    match std::env::var("PYTHONPATH") {
        Ok(existing) if !existing.is_empty() => {
            format!("{dir_str}:{existing}")
        }
        _ => dir_str,
    }
}
