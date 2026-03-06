//! Subprocess-based mutant runner.
//!
//! [`SubprocessRunner`] is the simplest (fallback) backend: for each
//! mutant it overwrites the original source file in-place, spawns
//! `pytest` as a subprocess, then restores the original source.
//! Mutants for the same file are serialised to avoid races.

extern crate alloc;

use alloc::sync::Arc;
use core::time::Duration;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

use tokio::{process::Command, sync::Mutex as AsyncMutex};

use crate::{
    Error,
    mutation::{Mutant, MutantResult, MutantStatus},
    runner::Runner,
};

/// Default timeout in seconds when none is specified.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Configuration for the subprocess runner.
///
/// Holds tunable parameters such as the per-mutant timeout.
///
/// The runner mutates source files **in-place** (overwrite → test → restore),
/// using per-file locks to prevent concurrent modifications to the same file.
#[derive(Debug)]
pub struct SubprocessRunner {
    /// Maximum wall-clock time (in seconds) for a single pytest
    /// invocation before it is considered timed out.
    timeout: Duration,
    /// Project root directory, used as `current_dir` for pytest.
    project_dir: PathBuf,
    /// Per-file locks ensuring only one mutant modifies a given file at
    /// a time.  Other mutants for different files can still run in
    /// parallel.
    file_locks: Mutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>,
}

impl SubprocessRunner {
    /// Create a new [`SubprocessRunner`] with the given timeout and project directory.
    #[inline]
    #[must_use]
    pub fn new(timeout_secs: u64, project_dir: PathBuf) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_secs),
            project_dir,
            file_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Get (or create) a per-file async lock.
    fn file_lock(&self, path: &Path) -> Arc<AsyncMutex<()>> {
        let mut locks = self
            .file_locks
            .lock()
            .unwrap_or_else(|poisoned: std::sync::PoisonError<_>| poisoned.into_inner());
        Arc::clone(
            locks
                .entry(path.to_path_buf())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        )
    }
}

impl Default for SubprocessRunner {
    #[inline]
    fn default() -> Self {
        Self::new(
            DEFAULT_TIMEOUT_SECS,
            std::env::current_dir().unwrap_or_default(),
        )
    }
}

impl Runner for SubprocessRunner {
    /// Run pytest against a single mutant in a subprocess.
    ///
    /// 1. Apply the mutation to the original source.
    /// 2. Overwrite the source file with the mutated version.
    /// 3. Spawn `python -m pytest <tests> -x --no-header -q`.
    /// 4. Restore the original source file.
    /// 5. Interpret the exit code.
    #[inline]
    async fn run_mutant(
        &self,
        mutant: &Mutant,
        source: &str,
        tests: &[String],
    ) -> Result<MutantResult, Error> {
        let start = tokio::time::Instant::now();

        // 1. Apply the mutation.
        let mutated_source = mutant.apply_to_source(source);

        // 2. Acquire per-file lock to prevent concurrent modifications.
        let lock = self.file_lock(&mutant.file_path);
        let guard = lock.lock().await;

        // 3. Overwrite the original file in-place.
        let file_path = &mutant.file_path;
        std::fs::write(file_path, &mutated_source).map_err(|err| {
            Error::Runner(format!(
                "failed to write mutated source to {}: {err}",
                file_path.display()
            ))
        })?;

        // 4. Spawn pytest with timeout.
        let mut cmd = Command::new("python");
        let _cmd_ref = cmd
            .args(["-m", "pytest", "-x", "--no-header", "-q"])
            .args(tests)
            .current_dir(&self.project_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        let outcome = tokio::time::timeout(self.timeout, cmd.output()).await;

        // 5. Restore the original source immediately (before releasing lock).
        let _restore = std::fs::write(file_path, source);

        // Lock released here when guard drops.
        drop(guard);

        let elapsed = start.elapsed();
        let tests_run: Vec<String> = tests.iter().map(ToString::to_string).collect();

        // 6. Interpret the result.
        let status = match outcome {
            Err(_elapsed) => MutantStatus::Timeout,
            Ok(Err(err)) => MutantStatus::Error(format!("failed to spawn pytest: {err}")),
            Ok(Ok(output)) => interpret_exit_code(output.status.code()),
        };

        Ok(MutantResult {
            mutant: mutant.clone(),
            status,
            tests_run,
            duration: elapsed,
        })
    }
}

/// Interpret the pytest exit code into a [`MutantStatus`].
///
/// - exit 0 means all tests passed => mutant survived
/// - any other exit code means at least one test failed => mutant killed
/// - `None` means the process was killed by a signal => treat as killed
const fn interpret_exit_code(code: Option<i32>) -> MutantStatus {
    match code {
        Some(0_i32) => MutantStatus::Survived,
        Some(_) | None => MutantStatus::Killed,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// Helper to create a simple test mutant.
    fn make_test_mutant() -> Mutant {
        Mutant {
            file_path: PathBuf::from("src/calc.py"),
            line: 1_u32,
            column: 7_u32,
            byte_offset: 6_usize,
            byte_length: 1_usize,
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            mutator_name: "arithmetic_op".to_owned(),
        }
    }

    /// Mutation is correctly applied.
    #[test]
    fn mutation_application() {
        let mutant = make_test_mutant();
        let source = "x = a + b";

        let mutated = mutant.apply_to_source(source);
        assert_eq!(mutated, "x = a - b");
    }

    /// `interpret_exit_code` maps exit codes to statuses correctly.
    #[test]
    fn exit_code_interpretation() {
        assert_eq!(interpret_exit_code(Some(0_i32)), MutantStatus::Survived);
        assert_eq!(interpret_exit_code(Some(1_i32)), MutantStatus::Killed);
        assert_eq!(interpret_exit_code(Some(2_i32)), MutantStatus::Killed);
        assert_eq!(interpret_exit_code(Some(127_i32)), MutantStatus::Killed);
        // Signal-killed process (no exit code)
        assert_eq!(interpret_exit_code(None), MutantStatus::Killed);
    }

    /// `build_python_path` prepends the directory.
    #[test]
    fn python_path_construction() {
        let dir = Path::new("/tmp/fest_test");
        let result = super::super::build_python_path(dir);
        assert!(result.starts_with("/tmp/fest_test"));
    }

    /// `SubprocessRunner::new` sets the timeout correctly.
    #[test]
    fn runner_timeout_configuration() {
        let runner = SubprocessRunner::new(60_u64, PathBuf::from("/project"));
        assert_eq!(runner.timeout, Duration::from_secs(60_u64));
    }

    /// `SubprocessRunner::default` uses the default timeout.
    #[test]
    fn runner_default_timeout() {
        let runner = SubprocessRunner::default();
        assert_eq!(runner.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    }

    /// Timeout handling: a very short timeout causes a `Timeout` status.
    #[tokio::test]
    async fn timeout_produces_timeout_status() {
        // Use a temp file so the in-place write succeeds.
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        std::fs::write(tmp.path(), "pass").expect("write source");

        let runner = SubprocessRunner::new(0_u64, PathBuf::from("."));

        let mutant = Mutant {
            file_path: tmp.path().to_path_buf(),
            line: 1_u32,
            column: 1_u32,
            byte_offset: 0_usize,
            byte_length: 4_usize,
            original_text: "pass".to_owned(),
            mutated_text: "pass".to_owned(),
            mutator_name: "noop".to_owned(),
        };

        let source = "pass";
        let tests = vec!["test_slow.py::test_hang".to_owned()];

        let result = runner
            .run_mutant(&mutant, source, &tests)
            .await
            .expect("should not return Err");

        // With a 0-second timeout, the process should be timed out.
        // However, if the system is extremely fast, pytest might not
        // even be spawned before the timeout. Either Timeout or Error
        // is acceptable.
        assert!(
            result.status == MutantStatus::Timeout
                || matches!(result.status, MutantStatus::Error(_)),
            "expected Timeout or Error, got {:?}",
            result.status,
        );

        // Verify original source was restored.
        let restored = std::fs::read_to_string(tmp.path()).expect("read restored");
        assert_eq!(restored, "pass");
    }

    /// A mutant that runs against a non-existent test file produces
    /// a Killed status (pytest exits non-zero).
    #[tokio::test]
    async fn nonexistent_test_produces_killed() {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        std::fs::write(tmp.path(), "1").expect("write source");

        let runner = SubprocessRunner::new(10_u64, PathBuf::from("."));

        let mutant = Mutant {
            file_path: tmp.path().to_path_buf(),
            line: 1_u32,
            column: 1_u32,
            byte_offset: 0_usize,
            byte_length: 1_usize,
            original_text: "1".to_owned(),
            mutated_text: "2".to_owned(),
            mutator_name: "constant_replace".to_owned(),
        };

        let source = "1";
        let tests = vec!["nonexistent_test_file.py::test_nothing".to_owned()];

        let result = runner
            .run_mutant(&mutant, source, &tests)
            .await
            .expect("should not return Err");

        // pytest will exit non-zero for a nonexistent test => Killed,
        // or Error if python is not found.
        assert!(
            result.status == MutantStatus::Killed
                || matches!(result.status, MutantStatus::Error(_)),
            "expected Killed or Error, got {:?}",
            result.status,
        );
    }

    /// The result includes the correct tests_run list.
    #[tokio::test]
    async fn result_contains_tests_run() {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        std::fs::write(tmp.path(), "x = a + b").expect("write source");

        let runner = SubprocessRunner::new(10_u64, PathBuf::from("."));
        let mut mutant = make_test_mutant();
        mutant.file_path = tmp.path().to_path_buf();
        let source = "x = a + b";
        let tests = vec![
            "test_a.py::test_add".to_owned(),
            "test_b.py::test_sub".to_owned(),
        ];

        let result = runner
            .run_mutant(&mutant, source, &tests)
            .await
            .expect("should not return Err");

        assert_eq!(result.tests_run.len(), 2_usize);
        assert_eq!(result.tests_run[0_usize], "test_a.py::test_add");
        assert_eq!(result.tests_run[1_usize], "test_b.py::test_sub");
    }

    /// The result duration is non-negative (sanity check).
    #[tokio::test]
    async fn result_has_duration() {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        std::fs::write(tmp.path(), "x = a + b").expect("write source");

        let runner = SubprocessRunner::new(10_u64, PathBuf::from("."));
        let mut mutant = make_test_mutant();
        mutant.file_path = tmp.path().to_path_buf();
        let source = "x = a + b";
        let tests: Vec<String> = Vec::new();

        let result = runner
            .run_mutant(&mutant, source, &tests)
            .await
            .expect("should not return Err");

        // Duration should be at least zero (always true for Duration).
        assert!(result.duration >= Duration::from_secs(0_u64));
    }

    /// The mutant in the result matches the input mutant.
    #[tokio::test]
    async fn result_mutant_matches_input() {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        std::fs::write(tmp.path(), "x = a + b").expect("write source");

        let runner = SubprocessRunner::new(10_u64, PathBuf::from("."));
        let mut mutant = make_test_mutant();
        mutant.file_path = tmp.path().to_path_buf();
        let source = "x = a + b";
        let tests: Vec<String> = Vec::new();

        let result = runner
            .run_mutant(&mutant, source, &tests)
            .await
            .expect("should not return Err");

        assert_eq!(result.mutant, mutant);
    }
}
