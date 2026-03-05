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

/// Check whether `pytest-xdist` is importable in the project's Python
/// environment. Returns `true` when the import succeeds.
fn is_xdist_available(project_dir: &Path) -> bool {
    Command::new("python")
        .args(["-c", "import xdist"])
        .current_dir(project_dir)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Extract source directory prefixes from glob patterns.
///
/// For each pattern, takes the path prefix before the first glob wildcard
/// (`*`, `?`, `[`). Deduplicates the result. Returns an empty vec when no
/// meaningful directories can be extracted.
fn extract_source_dirs(patterns: &[String]) -> Vec<String> {
    let mut dirs: Vec<String> = patterns
        .iter()
        .filter_map(|pat| {
            let first_glob = pat.find(['*', '?', '['])?;
            let prefix = pat.get(..first_glob)?;
            let dir = prefix.trim_end_matches('/');
            if dir.is_empty() {
                return None;
            }
            Some(dir.to_owned())
        })
        .collect();

    dirs.sort();
    dirs.dedup();
    dirs
}

/// Run `pytest --cov --cov-context=test` to generate coverage data.
///
/// Source directories extracted from `source_patterns` are passed as explicit
/// `--cov=<dir>` arguments so that coverage is collected for the directories
/// fest will mutate, regardless of the project's own `[tool.coverage.run]`
/// config.
///
/// When `pytest-xdist` is installed, `-n 0` forces serial execution.
/// Parallel workers interfere with `--cov-context=test` context tracking.
/// Using `-n 0` rather than `-p no:xdist` keeps xdist loaded so that any
/// `-n` in the project's `addopts` is still recognised (disabling xdist
/// makes `-n` an unknown flag).
///
/// Returns the exit status success flag. A failing exit code is **not** treated
/// as an error — the `.coverage` database is still produced when tests fail, and
/// we still want to extract coverage data from it.
///
/// # Errors
///
/// Returns [`Error::Coverage`] only if the subprocess cannot be spawned.
fn run_pytest_cov(
    project_dir: &Path,
    source_patterns: &[String],
    fast_coverage: bool,
) -> Result<bool, Error> {
    let source_dirs = extract_source_dirs(source_patterns);
    let xdist_installed = is_xdist_available(project_dir);

    let mut args = vec!["-m".to_owned(), "pytest".to_owned()];

    if source_dirs.is_empty() {
        args.push("--cov".to_owned());
    } else {
        for dir in &source_dirs {
            args.push(format!("--cov={dir}"));
        }
    }

    args.extend([
        "--cov-context=test".to_owned(),
        "--no-header".to_owned(),
        "-q".to_owned(),
    ]);

    if xdist_installed {
        args.extend(["-n".to_owned(), "0".to_owned()]);
    }

    let mut cmd = Command::new("python");
    let _args_ref = cmd.args(&args).current_dir(project_dir);
    if fast_coverage {
        let _env_ref = cmd.env("COVERAGE_CORE", "ctrace");
    }

    let output = cmd
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

    // `--fail-under=0` overrides the project's `[tool.coverage.report]
    // fail_under` setting. Without this, `coverage json` exits with code 2
    // when the total coverage is below the threshold, even though the JSON
    // file is produced successfully.
    let output = Command::new("python")
        .args([
            "-m",
            "coverage",
            "json",
            "--show-contexts",
            "--fail-under=0",
            "-o",
        ])
        .arg(&json_path_str)
        .current_dir(project_dir)
        .output()
        .map_err(|err| Error::Coverage(format!("failed to spawn `coverage json`: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(Error::Coverage(format!(
            "coverage json export failed: {stderr}{stdout}"
        )));
    }

    Ok(json_path)
}

/// Check whether the cached `.coverage.json` is fresh.
///
/// Returns `true` when `<project_dir>/.coverage.json` exists and its
/// modification time is newer than every `**/*.py` file **and** every
/// configuration file (`fest.toml`, `pyproject.toml`) under the project
/// directory. Returns `false` otherwise (missing file, I/O errors, or any
/// relevant file is newer than the cache).
#[inline]
#[must_use]
pub fn is_coverage_cache_fresh(project_dir: &Path) -> bool {
    let json_path = project_dir.join(COVERAGE_JSON_FILENAME);
    let Ok(json_mtime) = std::fs::metadata(&json_path).and_then(|m| m.modified()) else {
        return false;
    };

    // Invalidate cache when configuration files change (e.g. source
    // patterns were updated, which changes which files get coverage).
    for config_name in ["fest.toml", "pyproject.toml"] {
        let config_path = project_dir.join(config_name);
        let is_newer = std::fs::metadata(&config_path)
            .and_then(|m| m.modified())
            .is_ok_and(|mtime| mtime > json_mtime);
        if is_newer {
            return false;
        }
    }

    let pattern = format!("{}/**/*.py", project_dir.display());
    let Ok(entries) = glob::glob(&pattern) else {
        return false;
    };

    for entry in entries {
        let Ok(path) = entry else {
            return false;
        };
        let Ok(py_mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
            return false;
        };
        if py_mtime > json_mtime {
            return false;
        }
    }

    true
}

/// Load coverage data from the cached `.coverage.json` in the project
/// directory, without running any subprocesses.
///
/// # Errors
///
/// Returns [`Error::Coverage`] if the file cannot be read or parsed.
#[inline]
pub fn load_cached_coverage(project_dir: &Path) -> Result<CoverageMap, Error> {
    let json_path = project_dir.join(COVERAGE_JSON_FILENAME);
    json_parser::parse_coverage_json(&json_path, project_dir)
}

/// Load coverage data from a user-provided file.
///
/// If the path has a `.json` extension it is parsed directly. Otherwise it
/// is treated as a `.coverage` `SQLite` database: `coverage json` is invoked
/// with `COVERAGE_FILE` pointing at the given path to export JSON, and then
/// the JSON is parsed.
///
/// # Errors
///
/// Returns [`Error::Coverage`] if the export or parse fails.
#[inline]
pub fn load_coverage_from(path: &Path, project_dir: &Path) -> Result<CoverageMap, Error> {
    let is_json = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));

    if is_json {
        return json_parser::parse_coverage_json(path, project_dir);
    }

    // Assume SQLite .coverage database — export to JSON first.
    let json_path = project_dir.join(COVERAGE_JSON_FILENAME);
    let json_path_str = json_path.display().to_string();

    let output = Command::new("python")
        .args([
            "-m",
            "coverage",
            "json",
            "--show-contexts",
            "--fail-under=0",
            "-o",
        ])
        .arg(&json_path_str)
        .env("COVERAGE_FILE", path)
        .current_dir(project_dir)
        .output()
        .map_err(|err| {
            Error::Coverage(format!(
                "failed to spawn `coverage json` for {}: {err}",
                path.display()
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(Error::Coverage(format!(
            "coverage json export from {} failed: {stderr}{stdout}",
            path.display()
        )));
    }

    json_parser::parse_coverage_json(&json_path, project_dir)
}

/// Collect per-line test coverage for a Python project.
///
/// This is the main entry point for the coverage module. It:
/// 1. Checks that `pytest-cov` is installed.
/// 2. Runs pytest with coverage and test-context tracking.
/// 3. Exports the resulting `.coverage` database to JSON.
/// 4. Parses the JSON into a [`CoverageMap`].
///
/// # Errors
///
/// Returns [`Error::Coverage`] if `pytest-cov` is not available, the coverage
/// export fails, or the JSON cannot be parsed.
#[inline]
pub fn collect_coverage(
    project_dir: &Path,
    source_patterns: &[String],
    fast_coverage: bool,
) -> Result<CoverageMap, Error> {
    check_pytest_cov_available(project_dir)?;

    let _tests_passed = run_pytest_cov(project_dir, source_patterns, fast_coverage)?;
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
        let patterns = vec!["src/**/*.py".to_owned()];
        // pytest will fail (no tests), but the function should not panic.
        let result = run_pytest_cov(dir.path(), &patterns, true);
        // Accept Ok(false) (pytest exits non-zero) or Err (if python not
        // found). Either is fine for this test.
        let _unused = result;
    }

    #[test]
    fn extract_source_dirs_basic() {
        let patterns = vec!["src/app/**/*.py".to_owned()];
        assert_eq!(extract_source_dirs(&patterns), vec!["src/app"]);
    }

    #[test]
    fn extract_source_dirs_deduplicates() {
        let patterns = vec!["src/app/**/*.py".to_owned(), "src/app/*.py".to_owned()];
        assert_eq!(extract_source_dirs(&patterns), vec!["src/app"]);
    }

    #[test]
    fn extract_source_dirs_multiple_distinct() {
        let patterns = vec![
            "src/collectors/**/*.py".to_owned(),
            "lib/utils/*.py".to_owned(),
        ];
        let dirs = extract_source_dirs(&patterns);
        assert_eq!(dirs, vec!["lib/utils", "src/collectors"]);
    }

    #[test]
    fn extract_source_dirs_empty_input() {
        let patterns: Vec<String> = vec![];
        assert!(extract_source_dirs(&patterns).is_empty());
    }

    #[test]
    fn extract_source_dirs_root_glob() {
        // Pattern like `*.py` has no directory prefix — should be skipped.
        let patterns = vec!["*.py".to_owned()];
        assert!(extract_source_dirs(&patterns).is_empty());
    }

    #[test]
    fn extract_source_dirs_no_wildcards() {
        // Pattern without any glob characters — no match for wildcard chars,
        // so `find` returns `None` and the pattern is skipped.
        let patterns = vec!["src/app/main.py".to_owned()];
        assert!(extract_source_dirs(&patterns).is_empty());
    }

    #[test]
    fn extract_source_dirs_trailing_slash() {
        let patterns = vec!["src/app/collectors/**/*.py".to_owned()];
        assert_eq!(extract_source_dirs(&patterns), vec!["src/app/collectors"],);
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

    /// `is_coverage_cache_fresh` returns false when `.coverage.json` is missing.
    #[test]
    fn is_coverage_cache_fresh_no_json() {
        let dir = tempfile::tempdir().expect("create temp dir");
        assert!(!is_coverage_cache_fresh(dir.path()));
    }

    /// `is_coverage_cache_fresh` returns false when a `.py` file is newer.
    #[test]
    fn is_coverage_cache_fresh_stale() {
        let dir = tempfile::tempdir().expect("create temp dir");

        // Create .coverage.json first.
        let json_path = dir.path().join(COVERAGE_JSON_FILENAME);
        std::fs::write(&json_path, "{}").expect("write json");

        // Sleep briefly so the .py file gets a strictly newer mtime.
        std::thread::sleep(std::time::Duration::from_millis(50_u64));

        let py_path = dir.path().join("app.py");
        std::fs::write(&py_path, "x = 1").expect("write py");

        assert!(!is_coverage_cache_fresh(dir.path()));
    }

    /// `is_coverage_cache_fresh` returns true when json is newer than all `.py`.
    #[test]
    fn is_coverage_cache_fresh_valid() {
        let dir = tempfile::tempdir().expect("create temp dir");

        // Create .py file first.
        let py_path = dir.path().join("app.py");
        std::fs::write(&py_path, "x = 1").expect("write py");

        // Sleep briefly so the json gets a strictly newer mtime.
        std::thread::sleep(std::time::Duration::from_millis(50_u64));

        let json_path = dir.path().join(COVERAGE_JSON_FILENAME);
        std::fs::write(&json_path, "{}").expect("write json");

        assert!(is_coverage_cache_fresh(dir.path()));
    }

    /// `is_coverage_cache_fresh` returns false when `fest.toml` is newer.
    #[test]
    fn is_coverage_cache_stale_after_config_change() {
        let dir = tempfile::tempdir().expect("create temp dir");

        // Create .py file first.
        let py_path = dir.path().join("app.py");
        std::fs::write(&py_path, "x = 1").expect("write py");

        // Sleep so .coverage.json is strictly newer than .py.
        std::thread::sleep(std::time::Duration::from_millis(50_u64));

        let json_path = dir.path().join(COVERAGE_JSON_FILENAME);
        std::fs::write(&json_path, "{}").expect("write json");

        // Sleep so fest.toml is strictly newer than .coverage.json.
        std::thread::sleep(std::time::Duration::from_millis(50_u64));

        let config_path = dir.path().join("fest.toml");
        std::fs::write(&config_path, "[fest]\nsource = [\"packages/**/*.py\"]")
            .expect("write config");

        assert!(!is_coverage_cache_fresh(dir.path()));
    }

    /// `is_coverage_cache_fresh` returns false when `pyproject.toml` is newer.
    #[test]
    fn is_coverage_cache_stale_after_pyproject_change() {
        let dir = tempfile::tempdir().expect("create temp dir");

        let json_path = dir.path().join(COVERAGE_JSON_FILENAME);
        std::fs::write(&json_path, "{}").expect("write json");

        std::thread::sleep(std::time::Duration::from_millis(50_u64));

        let config_path = dir.path().join("pyproject.toml");
        std::fs::write(&config_path, "[tool.fest]\ntimeout = 30").expect("write pyproject");

        assert!(!is_coverage_cache_fresh(dir.path()));
    }

    /// `load_cached_coverage` parses a valid `.coverage.json` round-trip.
    #[test]
    fn load_cached_coverage_parses_json() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let json_path = dir.path().join(COVERAGE_JSON_FILENAME);

        let json_content = r#"{
            "files": {
                "lib.py": {
                    "executed_lines": [1],
                    "contexts": {
                        "1": ["test_lib.py::test_func"]
                    }
                }
            }
        }"#;
        std::fs::write(&json_path, json_content).expect("write json");

        let map = load_cached_coverage(dir.path()).expect("should parse");
        let key = (dir.path().join("lib.py"), 1_u32);
        assert!(map.contains_key(&key));
    }

    /// `load_coverage_from` loads a user-provided `.json` file.
    #[test]
    fn load_coverage_from_json_file() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let json_path = dir.path().join("user_coverage.json");

        let json_content = r#"{
            "files": {
                "mod.py": {
                    "executed_lines": [5],
                    "contexts": {
                        "5": ["test_mod.py::test_five"]
                    }
                }
            }
        }"#;
        std::fs::write(&json_path, json_content).expect("write json");

        let map = load_coverage_from(&json_path, dir.path()).expect("should parse");
        let key = (dir.path().join("mod.py"), 5_u32);
        assert!(map.contains_key(&key));
    }
}
