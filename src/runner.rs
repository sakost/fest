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

use core::sync::atomic::{AtomicBool, Ordering};
use std::path::Path;

pub use pytest_plugin::PytestPluginRunner;
pub use subprocess::SubprocessRunner;

use crate::{
    Error,
    config::RunnerBackend,
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
    /// Initialise the runner with the given number of parallel workers.
    ///
    /// Called once before the mutant execution loop begins.  Backends
    /// that maintain persistent worker processes should spawn them here.
    ///
    /// The default implementation is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runner`] if initialisation fails.
    #[inline]
    fn start(
        &self,
        _num_workers: usize,
        _project_dir: &Path,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        async { Ok(()) }
    }

    /// Shut down the runner and release any resources.
    ///
    /// Called once after the mutant execution loop completes.  Backends
    /// that maintain persistent worker processes should terminate them
    /// here.
    ///
    /// The default implementation is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runner`] if shutdown fails.
    #[inline]
    fn stop(&self) -> impl Future<Output = Result<(), Error>> + Send {
        async { Ok(()) }
    }

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

/// Platform-specific `PYTHONPATH` separator.
///
/// Unix uses `:`, Windows uses `;`.
const PYTHON_PATH_SEPARATOR: char = if cfg!(windows) { ';' } else { ':' };

/// Build a `PYTHONPATH` string that prepends `dir` to any existing value.
///
/// Both [`SubprocessRunner`] and [`PytestPluginRunner`] need to set
/// `PYTHONPATH` so that the mutated file (or plugin) takes import
/// priority. This shared helper keeps that logic in one place.
#[inline]
pub(crate) fn build_python_path(dir: &Path) -> String {
    let dir_str = dir.display().to_string();

    match std::env::var("PYTHONPATH") {
        Ok(existing) if !existing.is_empty() => {
            format!("{dir_str}{PYTHON_PATH_SEPARATOR}{existing}")
        }
        _ => dir_str,
    }
}

/// Enum dispatch wrapper for mutant execution backends.
///
/// The [`Runner`] trait uses RPITIT (`-> impl Future`), which makes it
/// not object-safe. This enum provides static dispatch over all
/// available backends.
///
/// The [`Plugin`](Self::Plugin) variant carries a subprocess fallback:
/// on the first [`Error::Runner`] from the plugin, it switches to
/// subprocess for all remaining mutants.
#[derive(Debug)]
pub enum AnyRunner {
    /// Subprocess-based runner (no fallback).
    Subprocess(SubprocessRunner),
    /// Pytest-plugin-based runner with automatic subprocess fallback.
    Plugin {
        /// Primary plugin runner.
        plugin: PytestPluginRunner,
        /// Fallback subprocess runner, used when the plugin fails.
        subprocess: SubprocessRunner,
        /// Set to `true` after the first plugin infrastructure failure.
        plugin_failed: AtomicBool,
    },
}

impl AnyRunner {
    /// Initialise the runner backend.
    ///
    /// For the plugin variant, spawns persistent worker processes.  On
    /// failure, sets the `plugin_failed` flag so that subsequent
    /// [`run_mutant`](Self::run_mutant) calls fall back to subprocess.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runner`] only for the subprocess variant; the
    /// plugin variant absorbs the error and falls back silently.
    #[inline]
    pub async fn start(&self, num_workers: usize, project_dir: &Path) -> Result<(), Error> {
        #[allow(
            clippy::pattern_type_mismatch,
            reason = "matching on &AnyRunner requires pattern_type_mismatch suppression"
        )]
        match self {
            Self::Subprocess(runner) => runner.start(num_workers, project_dir).await,
            Self::Plugin {
                plugin,
                plugin_failed,
                ..
            } => {
                if let Err(_err) = plugin.start(num_workers, project_dir).await {
                    plugin_failed.store(true, Ordering::Relaxed);
                }
                Ok(())
            }
        }
    }

    /// Shut down the runner backend.
    ///
    /// For the plugin variant, terminates persistent worker processes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runner`] if shutdown fails.
    #[inline]
    pub async fn stop(&self) -> Result<(), Error> {
        #[allow(
            clippy::pattern_type_mismatch,
            reason = "matching on &AnyRunner requires pattern_type_mismatch suppression"
        )]
        match self {
            Self::Subprocess(runner) => runner.stop().await,
            Self::Plugin { plugin, .. } => plugin.stop().await,
        }
    }

    /// Run the test suite against a single mutant, dispatching to the
    /// underlying backend.
    ///
    /// When the plugin backend encounters an infrastructure error
    /// ([`Error::Runner`]), it automatically falls back to subprocess
    /// for this and all subsequent mutants.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runner`] if the backend (or its fallback)
    /// encounters an unrecoverable error.
    #[inline]
    pub async fn run_mutant(
        &self,
        mutant: &Mutant,
        source: &str,
        tests: &[String],
    ) -> Result<MutantResult, Error> {
        #[allow(
            clippy::pattern_type_mismatch,
            reason = "matching on &AnyRunner requires pattern_type_mismatch suppression"
        )]
        match self {
            Self::Subprocess(runner) => runner.run_mutant(mutant, source, tests).await,
            Self::Plugin {
                plugin,
                subprocess,
                plugin_failed,
            } => {
                if plugin_failed.load(Ordering::Relaxed) {
                    return subprocess.run_mutant(mutant, source, tests).await;
                }

                match plugin.run_mutant(mutant, source, tests).await {
                    Err(Error::Runner(_msg)) => {
                        plugin_failed.store(true, Ordering::Relaxed);
                        subprocess.run_mutant(mutant, source, tests).await
                    }
                    other => other,
                }
            }
        }
    }

    /// Check whether the plugin backend has failed and fallen back to
    /// subprocess.
    ///
    /// Returns `false` for the subprocess variant.
    #[inline]
    #[must_use]
    pub fn did_plugin_fail(&self) -> bool {
        #[allow(
            clippy::pattern_type_mismatch,
            reason = "matching on &AnyRunner requires pattern_type_mismatch suppression"
        )]
        match self {
            Self::Subprocess(_) => false,
            Self::Plugin { plugin_failed, .. } => plugin_failed.load(Ordering::Relaxed),
        }
    }
}

/// Build the runner backend selected by the configuration.
///
/// The [`Plugin`](RunnerBackend::Plugin) variant always includes a
/// subprocess fallback runner alongside the plugin runner.
#[inline]
#[must_use]
pub fn build_runner(
    backend: &RunnerBackend,
    timeout: u64,
    project_dir: std::path::PathBuf,
) -> AnyRunner {
    match *backend {
        RunnerBackend::Subprocess => {
            AnyRunner::Subprocess(SubprocessRunner::new(timeout, project_dir))
        }
        RunnerBackend::Plugin => {
            let subprocess = SubprocessRunner::new(timeout, project_dir);
            AnyRunner::Plugin {
                plugin: PytestPluginRunner::new(timeout),
                subprocess,
                plugin_failed: AtomicBool::new(false),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// `build_runner` returns `AnyRunner::Subprocess` for the subprocess backend.
    #[test]
    fn build_runner_subprocess() {
        let runner = build_runner(
            &RunnerBackend::Subprocess,
            10_u64,
            PathBuf::from("/project"),
        );
        assert!(matches!(runner, AnyRunner::Subprocess(_)));
    }

    /// `build_runner` returns `AnyRunner::Plugin` for the plugin backend.
    #[test]
    fn build_runner_plugin() {
        let runner = build_runner(&RunnerBackend::Plugin, 10_u64, PathBuf::from("/project"));
        assert!(matches!(runner, AnyRunner::Plugin { .. }));
    }

    /// Plugin variant includes a fresh `plugin_failed` flag.
    #[test]
    fn build_runner_plugin_not_failed() {
        let runner = build_runner(&RunnerBackend::Plugin, 10_u64, PathBuf::from("/project"));
        if let AnyRunner::Plugin { plugin_failed, .. } = &runner {
            assert!(!plugin_failed.load(Ordering::Relaxed));
        } else {
            panic!("expected AnyRunner::Plugin");
        }
    }

    /// `AnyRunner::start` on subprocess variant is a no-op that succeeds.
    #[tokio::test]
    async fn any_runner_start_subprocess() {
        let runner = build_runner(
            &RunnerBackend::Subprocess,
            10_u64,
            PathBuf::from("/project"),
        );
        let result = runner.start(4_usize, Path::new(".")).await;
        assert!(result.is_ok());
    }

    /// `AnyRunner::stop` on subprocess variant is a no-op that succeeds.
    #[tokio::test]
    async fn any_runner_stop_subprocess() {
        let runner = build_runner(
            &RunnerBackend::Subprocess,
            10_u64,
            PathBuf::from("/project"),
        );
        let result = runner.stop().await;
        assert!(result.is_ok());
    }

    /// `AnyRunner::start` on plugin variant absorbs errors and sets
    /// `plugin_failed`.
    #[tokio::test]
    async fn any_runner_start_plugin_absorbs_error() {
        // Using timeout=0 so the worker spawn will likely fail/timeout.
        let runner = build_runner(&RunnerBackend::Plugin, 0_u64, PathBuf::from("/project"));
        // start should not return Err -- it absorbs plugin errors.
        let result = runner.start(1_usize, Path::new(".")).await;
        assert!(result.is_ok());
    }

    /// `AnyRunner::stop` on plugin variant succeeds even without start.
    #[tokio::test]
    async fn any_runner_stop_plugin_without_start() {
        let runner = build_runner(&RunnerBackend::Plugin, 10_u64, PathBuf::from("/project"));
        let result = runner.stop().await;
        assert!(result.is_ok());
    }
}
