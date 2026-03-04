//! Subprocess-based mutant runner.
//!
//! [`SubprocessRunner`] is the simplest (fallback) backend: for each
//! mutant it writes the mutated source to a temporary directory, sets
//! `PYTHONPATH` so that Python imports the mutated file, and spawns
//! `pytest` as a subprocess with a configurable timeout.

use core::time::Duration;
use std::path::Path;

use tokio::process::Command;

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
#[derive(Debug, Clone)]
pub struct SubprocessRunner {
    /// Maximum wall-clock time (in seconds) for a single pytest
    /// invocation before it is considered timed out.
    timeout: Duration,
}

impl SubprocessRunner {
    /// Create a new [`SubprocessRunner`] with the given timeout.
    #[inline]
    #[must_use]
    pub const fn new(timeout_secs: u64) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_secs),
        }
    }
}

impl Default for SubprocessRunner {
    #[inline]
    fn default() -> Self {
        Self::new(DEFAULT_TIMEOUT_SECS)
    }
}

impl Runner for SubprocessRunner {
    /// Run pytest against a single mutant in a subprocess.
    ///
    /// 1. Apply the mutation to the original source.
    /// 2. Write the mutated file into a temp directory, mirroring the original relative path
    ///    structure.
    /// 3. Spawn `python -m pytest <tests> -x --no-header -q` with `PYTHONPATH` prepended so the
    ///    mutated file takes priority.
    /// 4. Interpret the exit code:
    ///    - 0 => [`MutantStatus::Survived`]
    ///    - non-zero => [`MutantStatus::Killed`]
    ///    - timeout => [`MutantStatus::Timeout`]
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

        // 2. Create a temp directory and mirror the file path.
        let temp_dir = tempfile::tempdir()
            .map_err(|err| Error::Runner(format!("failed to create temp dir: {err}")))?;

        let relative_path = relative_or_filename(&mutant.file_path);
        let mutated_path = temp_dir.path().join(relative_path);

        // Ensure parent directories exist.
        if let Some(parent) = mutated_path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                Error::Runner(format!(
                    "failed to create directories for {}: {err}",
                    mutated_path.display()
                ))
            })?;
        }

        std::fs::write(&mutated_path, &mutated_source).map_err(|err| {
            Error::Runner(format!(
                "failed to write mutated source to {}: {err}",
                mutated_path.display()
            ))
        })?;

        // 3. Build PYTHONPATH: prepend the temp dir so imports resolve there first.
        let python_path = build_python_path(temp_dir.path());

        // 4. Spawn pytest.
        let mut cmd = Command::new("python");
        let _cmd_ref = cmd
            .args(["-m", "pytest", "-x", "--no-header", "-q"])
            .args(tests)
            .env("PYTHONPATH", &python_path);

        let outcome = tokio::time::timeout(self.timeout, cmd.output()).await;

        let elapsed = start.elapsed();
        let tests_run: Vec<String> = tests.iter().map(ToString::to_string).collect();

        // 5. Interpret the result.
        let status = match outcome {
            Err(_elapsed) => MutantStatus::Timeout,
            Ok(Err(err)) => MutantStatus::Error(format!("failed to spawn pytest: {err}")),
            Ok(Ok(output)) => interpret_exit_code(output.status.code()),
        };

        // tempfile::TempDir is dropped here, cleaning up automatically.
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

/// Extract a relative path from a potentially absolute path.
///
/// If the path is relative already, returns it as-is. If absolute,
/// returns just the file name component to avoid creating deep
/// directory hierarchies rooted at `/`.
fn relative_or_filename(path: &Path) -> &Path {
    if path.is_relative() {
        return path;
    }
    // For absolute paths, use just the file name to keep the temp dir flat.
    path.file_name().map_or(path, Path::new)
}

/// Build a `PYTHONPATH` string that prepends `dir` to any existing value.
fn build_python_path(dir: &Path) -> String {
    let dir_str = dir.display().to_string();

    match std::env::var("PYTHONPATH") {
        Ok(existing) if !existing.is_empty() => {
            format!("{dir_str}:{existing}")
        }
        _ => dir_str,
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

    /// Mutation is correctly applied and written to the temp file.
    #[test]
    fn mutation_application_and_temp_file_writing() {
        let mutant = make_test_mutant();
        let source = "x = a + b";

        let mutated = mutant.apply_to_source(source);
        assert_eq!(mutated, "x = a - b");

        // Write to a temp dir the same way the runner does.
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let relative = relative_or_filename(&mutant.file_path);
        let mutated_path = temp_dir.path().join(relative);

        if let Some(parent) = mutated_path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }

        std::fs::write(&mutated_path, &mutated).expect("write mutated file");

        let contents = std::fs::read_to_string(&mutated_path).expect("read back");
        assert_eq!(contents, "x = a - b");
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

    /// `relative_or_filename` returns relative paths unchanged.
    #[test]
    fn relative_path_passthrough() {
        let path = Path::new("src/calc.py");
        assert_eq!(relative_or_filename(path), Path::new("src/calc.py"));
    }

    /// `relative_or_filename` extracts filename from absolute paths.
    #[test]
    fn absolute_path_extracts_filename() {
        let path = Path::new("/home/user/project/src/calc.py");
        assert_eq!(relative_or_filename(path), Path::new("calc.py"));
    }

    /// `build_python_path` prepends the directory.
    #[test]
    fn python_path_construction() {
        let dir = Path::new("/tmp/fest_test");
        let result = build_python_path(dir);
        assert!(result.starts_with("/tmp/fest_test"));
    }

    /// `SubprocessRunner::new` sets the timeout correctly.
    #[test]
    fn runner_timeout_configuration() {
        let runner = SubprocessRunner::new(60_u64);
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
        // Create a runner with a tiny timeout (1 millisecond).
        let runner = SubprocessRunner::new(0_u64);

        let mutant = Mutant {
            file_path: PathBuf::from("slow.py"),
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
    }

    /// A mutant that runs against a non-existent test file produces
    /// a Killed status (pytest exits non-zero).
    #[tokio::test]
    async fn nonexistent_test_produces_killed() {
        let runner = SubprocessRunner::new(10_u64);

        let mutant = Mutant {
            file_path: PathBuf::from("simple.py"),
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
        let runner = SubprocessRunner::new(10_u64);
        let mutant = make_test_mutant();
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
        let runner = SubprocessRunner::new(10_u64);
        let mutant = make_test_mutant();
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
        let runner = SubprocessRunner::new(10_u64);
        let mutant = make_test_mutant();
        let source = "x = a + b";
        let tests: Vec<String> = Vec::new();

        let result = runner
            .run_mutant(&mutant, source, &tests)
            .await
            .expect("should not return Err");

        assert_eq!(result.mutant, mutant);
    }
}
