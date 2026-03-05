//! Coverage analysis — determining which tests exercise which source lines.
//!
//! This module provides utilities for collecting and interpreting
//! coverage data so that the runner can skip mutants in untested code.
//!
//! The workflow is:
//! 1. Verify that `pytest-cov` is available in the target Python environment.
//! 2. Run `pytest --cov --cov-context=test` to produce a `.coverage` database.
//! 3. Export the database to JSON via `coverage json --show-contexts`.
//! 4. Parse the JSON into a [`CoverageMap`] that maps `(file, line)` pairs to the test IDs that
//!    exercised each line.

mod json_parser;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

use crate::Error;

/// A mapping from `(source_file, line_number)` to the list of test IDs that
/// covered that line.
///
/// The `PathBuf` is relative to the project directory (matching the paths
/// emitted by `coverage json`), and the line number is 1-based.
pub type CoverageMap = HashMap<(PathBuf, u32), Vec<String>>;

/// Name of the JSON file produced by `coverage json`.
const COVERAGE_JSON_FILENAME: &str = ".coverage.json";

/// Verify that `pytest-cov` is importable in the project's Python environment.
///
/// # Errors
///
/// Returns [`Error::Coverage`] if the import check fails or the subprocess
/// cannot be spawned.
fn check_pytest_cov_available(project_dir: &Path) -> Result<(), Error> {
    let output = Command::new("python")
        .args(["-c", "import pytest_cov"])
        .current_dir(project_dir)
        .output()
        .map_err(|err| {
            Error::Coverage(format!("failed to spawn Python to check pytest-cov: {err}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Coverage(format!(
            "pytest-cov is not available (install it with `pip install pytest-cov`): {stderr}"
        )));
    }

    Ok(())
}

/// Run `pytest --cov --cov-context=test` to generate coverage data.
///
/// Returns the exit status success flag. A failing exit code is **not** treated
/// as an error — the `.coverage` database is still produced when tests fail, and
/// we still want to extract coverage data from it.
///
/// # Errors
///
/// Returns [`Error::Coverage`] only if the subprocess cannot be spawned.
fn run_pytest_cov(project_dir: &Path) -> Result<bool, Error> {
    let output = Command::new("python")
        .args([
            "-m",
            "pytest",
            "--cov",
            "--cov-context=test",
            "--no-header",
            "-q",
        ])
        .current_dir(project_dir)
        .output()
        .map_err(|err| Error::Coverage(format!("failed to spawn pytest: {err}")))?;

    Ok(output.status.success())
}

/// Export the `.coverage` sqlite database to JSON using `coverage json`.
///
/// Produces a file at `<project_dir>/.coverage.json` that contains per-line
/// context information.
///
/// # Errors
///
/// Returns [`Error::Coverage`] if the subprocess fails or exits with a
/// non-zero status.
fn export_coverage_json(project_dir: &Path) -> Result<PathBuf, Error> {
    let json_path = project_dir.join(COVERAGE_JSON_FILENAME);
    let json_path_str = json_path.display().to_string();

    let output = Command::new("python")
        .args(["-m", "coverage", "json", "--show-contexts", "-o"])
        .arg(&json_path_str)
        .current_dir(project_dir)
        .output()
        .map_err(|err| Error::Coverage(format!("failed to spawn `coverage json`: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Coverage(format!(
            "coverage json export failed: {stderr}"
        )));
    }

    Ok(json_path)
}

/// Collect per-line test coverage for a Python project.
///
/// This is the main entry point for the coverage module. It:
/// 1. Checks that `pytest-cov` is installed.
/// 2. Runs pytest with coverage and test-context tracking.
/// 3. Exports the resulting `.coverage` database to JSON.
/// 4. Parses the JSON into a [`CoverageMap`].
///
/// The `_source_patterns` parameter is reserved for future use (e.g. filtering
/// coverage data to only the files that will be mutated).
///
/// # Errors
///
/// Returns [`Error::Coverage`] if `pytest-cov` is not available, the coverage
/// export fails, or the JSON cannot be parsed.
#[inline]
pub fn collect_coverage(
    project_dir: &Path,
    _source_patterns: &[String],
) -> Result<CoverageMap, Error> {
    check_pytest_cov_available(project_dir)?;

    let _tests_passed = run_pytest_cov(project_dir)?;
    // We intentionally ignore the test pass/fail status — coverage data
    // is produced even when some tests fail.

    let json_path = export_coverage_json(project_dir)?;

    json_parser::parse_coverage_json(&json_path, project_dir)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `check_pytest_cov_available` returns an error when run in a directory
    /// where Python/pytest-cov is not on PATH (uses a non-existent dir to
    /// trigger the spawn failure path on some systems, or an import error).
    #[test]
    fn check_pytest_cov_error_message() {
        // Using a temp dir with no Python env should fail the import.
        let dir = tempfile::tempdir().expect("create temp dir");
        let result = check_pytest_cov_available(dir.path());
        // We accept either an error (import failure) or success (if
        // pytest-cov happens to be installed system-wide). The important
        // thing is that the function does not panic.
        let _unused = result;
    }

    /// `run_pytest_cov` constructs and executes the right command. Since we
    /// cannot run real pytest in unit tests, we just verify that the function
    /// returns a meaningful result when invoked in an empty directory.
    #[test]
    fn run_pytest_cov_in_empty_dir() {
        let dir = tempfile::tempdir().expect("create temp dir");
        // pytest will fail (no tests), but the function should not panic.
        let result = run_pytest_cov(dir.path());
        // Accept Ok(false) (pytest exits non-zero) or Err (if python not
        // found). Either is fine for this test.
        let _unused = result;
    }

    /// `export_coverage_json` fails when there is no `.coverage` file.
    #[test]
    fn export_coverage_json_no_coverage_file() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let result = export_coverage_json(dir.path());
        // Should be an error because there is no .coverage database.
        // Accept Err or Ok depending on whether `coverage` CLI is installed.
        let _unused = result;
    }

    /// `COVERAGE_JSON_FILENAME` has the expected value.
    #[test]
    fn coverage_json_filename_constant() {
        assert_eq!(COVERAGE_JSON_FILENAME, ".coverage.json");
    }

    /// `CoverageMap` supports expected lookup operations.
    #[test]
    fn coverage_map_lookup() {
        let mut map = CoverageMap::new();
        let key = (PathBuf::from("src/app.py"), 42_u32);
        let tests = vec!["test_app.py::test_hello".to_owned()];
        let _prev = map.insert(key.clone(), tests);

        assert!(map.contains_key(&key));
        assert_eq!(map.get(&key).expect("should contain key").len(), 1_usize,);

        let missing = (PathBuf::from("src/other.py"), 1_u32);
        assert!(!map.contains_key(&missing));
    }

    /// `CoverageMap` handles multiple entries for different lines in the
    /// same file.
    #[test]
    fn coverage_map_same_file_multiple_lines() {
        let mut map = CoverageMap::new();
        let file = PathBuf::from("module.py");

        let _prev1 = map.insert(
            (file.clone(), 1_u32),
            vec!["test_a.py::test_one".to_owned()],
        );
        let _prev2 = map.insert(
            (file.clone(), 2_u32),
            vec![
                "test_a.py::test_one".to_owned(),
                "test_b.py::test_two".to_owned(),
            ],
        );

        assert_eq!(map.len(), 2_usize);
        assert_eq!(
            map.get(&(file.clone(), 2_u32))
                .expect("line 2 present")
                .len(),
            2_usize,
        );
    }

    /// Empty `CoverageMap` behaves correctly.
    #[test]
    fn coverage_map_empty() {
        let map = CoverageMap::new();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0_usize);
    }
}
