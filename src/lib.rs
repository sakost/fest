//! **fest** — a fast mutation-testing tool for Python.
//!
//! This crate provides both a library API and a CLI binary for
//! generating mutants from Python source code, running a test suite
//! against each mutant, and reporting which mutants survived.

use std::{
    collections::HashMap,
    io::Write as _,
    path::{Path, PathBuf},
};

pub mod cli;
pub mod config;
pub mod coverage;
pub mod error;
pub mod mutation;
pub mod plugin;
pub mod report;
pub mod runner;

pub use error::Error;
use runner::Runner as _;

/// Run the fest pipeline to completion.
///
/// This is the top-level entry point invoked by the CLI binary.
/// It accepts the parsed [`cli::RunArgs`] from the command line so that
/// CLI overrides can be merged with the loaded configuration.
///
/// The pipeline performs the following stages in order:
/// 1. Load configuration from disk and merge CLI overrides.
/// 2. Build the mutator registry from the enabled mutator flags.
/// 3. Discover and count Python source files, then generate mutants.
/// 4. Collect per-line test coverage from `pytest-cov`.
/// 5. For each mutant, look up coverage and either mark as `NoCoverage` or run the test suite via
///    [`runner::SubprocessRunner`].
/// 6. Build a [`report::MutationReport`] and format it for output.
/// 7. Optionally check the mutation score against a `fail_under` threshold.
///
/// # Errors
///
/// Returns [`Error`] if any stage of the pipeline fails (configuration,
/// mutation, coverage, test-running, or report generation).
#[inline]
#[allow(
    clippy::needless_pass_by_value,
    reason = "RunArgs is consumed; pass-by-value is intentional for the public API"
)]
pub fn run(args: cli::RunArgs) -> Result<(), Error> {
    // 1. Determine the project directory and load configuration.
    let project_dir = resolve_project_dir(&args)?;
    let file_config = config::load(&project_dir)?;
    let config = cli::merge_config(&args, file_config);

    // 2. Build the mutator registry from config.
    let registry = mutation::build_registry(&config.mutators);

    // 3. Discover source files and generate mutants.
    let files = mutation::discover_files(&config.source, &config.exclude, &project_dir)?;
    let files_scanned = files.len();
    let mutants = mutation::generate_mutants_for_files(&files, &registry)?;
    let mutants_generated = mutants.len();

    // 4. Collect coverage.
    let coverage_map = coverage::collect_coverage(&project_dir, &config.source)?;

    // 5. Run each mutant against the test suite.
    let start = std::time::Instant::now();
    let results = run_mutants(&mutants, &coverage_map, &config)?;
    let duration = start.elapsed();

    // 6. Build and format the report.
    let report =
        report::MutationReport::from_results(results, files_scanned, mutants_generated, duration);
    let formatted = report::format_report(&report, &config.output)?;
    write_output(&formatted)?;

    // 7. Check the fail_under threshold.
    if let Some(threshold) = config.fail_under
        && !report.passes_threshold(threshold)
    {
        return Err(Error::Threshold(format!(
            "mutation score {:.1}% is below the required threshold of {:.1}%",
            report.mutation_score(),
            threshold,
        )));
    }

    Ok(())
}

/// Resolve the project directory from CLI arguments.
///
/// When a `--config` path is given, uses its parent directory.
/// Otherwise falls back to the current working directory.
///
/// # Errors
///
/// Returns [`Error::Config`] if the current directory cannot be determined.
fn resolve_project_dir(args: &cli::RunArgs) -> Result<PathBuf, Error> {
    if let Some(config_path) = args.config.as_ref() {
        let parent = config_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        return Ok(parent);
    }

    std::env::current_dir()
        .map_err(|err| Error::Config(format!("failed to determine current directory: {err}")))
}

/// Run all mutants and collect their results.
///
/// For each mutant, looks up the coverage map to determine which tests
/// exercise the mutated line. Mutants with no covering tests are marked
/// as [`mutation::MutantStatus::NoCoverage`] without reading the source
/// file. Covered mutants have their source read (once per unique file,
/// cached in a [`HashMap`]) and are executed via
/// [`runner::SubprocessRunner`] in a tokio runtime.
///
/// # Errors
///
/// Returns [`Error`] if a source file cannot be read or the tokio runtime
/// cannot be created.
fn run_mutants(
    mutants: &[mutation::Mutant],
    coverage_map: &coverage::CoverageMap,
    config: &config::FestConfig,
) -> Result<Vec<mutation::MutantResult>, Error> {
    // TODO: add runner selection config to choose between SubprocessRunner and PytestPluginRunner.
    let runner = runner::SubprocessRunner::new(config.timeout);
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|err| Error::Runner(format!("failed to create tokio runtime: {err}")))?;

    let mut source_cache: HashMap<PathBuf, String> = HashMap::new();
    let mut results: Vec<mutation::MutantResult> = Vec::with_capacity(mutants.len());

    for mutant in mutants {
        // Look up coverage for this mutant's file and line.
        let coverage_key = (mutant.file_path.clone(), mutant.line);
        let covering_tests = coverage_map.get(&coverage_key);

        let result = match covering_tests {
            Some(tests) if !tests.is_empty() => {
                // Only read the source file when we actually need to run the mutant.
                let source = read_source_cached(&mut source_cache, &mutant.file_path)?;
                runtime.block_on(runner.run_mutant(mutant, source, tests))?
            }
            _ => mutation::MutantResult {
                mutant: mutant.clone(),
                status: mutation::MutantStatus::NoCoverage,
                tests_run: Vec::new(),
                duration: core::time::Duration::from_secs(0_u64),
            },
        };

        results.push(result);
    }

    Ok(results)
}

/// Read a source file, caching its contents for subsequent lookups.
///
/// If the file has already been read, returns a reference to the cached
/// content. Otherwise reads the file, inserts it into `cache`, and
/// returns a reference.
///
/// # Errors
///
/// Returns [`Error::Mutation`] if the file cannot be read.
fn read_source_cached<'cache>(
    cache: &'cache mut HashMap<PathBuf, String>,
    path: &Path,
) -> Result<&'cache str, Error> {
    if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(path.to_path_buf()) {
        let content = std::fs::read_to_string(path).map_err(|err| {
            Error::Mutation(format!(
                "failed to read source file {}: {err}",
                path.display()
            ))
        })?;
        let _inserted = entry.insert(content);
    }
    cache
        .get(path)
        .map(String::as_str)
        .ok_or_else(|| Error::Mutation("source cache lookup failed unexpectedly".to_owned()))
}

/// Write the formatted report to stdout.
///
/// # Errors
///
/// Returns [`Error::Io`] if writing to stdout fails.
fn write_output(output: &str) -> Result<(), Error> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{output}")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `resolve_project_dir` returns the parent of a config path.
    #[test]
    fn resolve_project_dir_from_config_path() {
        let args = cli::RunArgs {
            source: None,
            exclude: None,
            workers: None,
            workers_cpu_ratio: None,
            timeout: None,
            fail_under: None,
            output: None,
            config: Some(PathBuf::from("/home/user/project/fest.toml")),
        };

        let dir = resolve_project_dir(&args).expect("should resolve");
        assert_eq!(dir, PathBuf::from("/home/user/project"));
    }

    /// `resolve_project_dir` falls back to the current working directory.
    #[test]
    fn resolve_project_dir_cwd_fallback() {
        let args = cli::RunArgs {
            source: None,
            exclude: None,
            workers: None,
            workers_cpu_ratio: None,
            timeout: None,
            fail_under: None,
            output: None,
            config: None,
        };

        let dir = resolve_project_dir(&args).expect("should resolve");
        let cwd = std::env::current_dir().expect("should get cwd");
        assert_eq!(dir, cwd);
    }

    /// `resolve_project_dir` handles a config path with no parent
    /// (bare filename) by returning `"."`.
    #[test]
    fn resolve_project_dir_bare_filename() {
        let args = cli::RunArgs {
            source: None,
            exclude: None,
            workers: None,
            workers_cpu_ratio: None,
            timeout: None,
            fail_under: None,
            output: None,
            config: Some(PathBuf::from("fest.toml")),
        };

        let dir = resolve_project_dir(&args).expect("should resolve");
        // Parent of "fest.toml" is "" (empty), which is normalized to "."
        assert_eq!(dir, PathBuf::from("."));
    }

    /// `write_output` writes to stdout without errors.
    #[test]
    fn write_output_succeeds() {
        let result = write_output("test output");
        assert!(result.is_ok());
    }

    /// `read_source_cached` reads a file and caches it.
    #[test]
    fn read_source_cached_reads_and_caches() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let py_file = dir.path().join("cached.py");
        std::fs::write(&py_file, "x = 1 + 2\n").expect("write file");

        let mut cache: HashMap<PathBuf, String> = HashMap::new();

        let content = read_source_cached(&mut cache, &py_file).expect("should read");
        assert_eq!(content, "x = 1 + 2\n");

        // Should be cached now.
        assert!(cache.contains_key(&py_file));

        // Second read should return the same content from cache.
        let content2 = read_source_cached(&mut cache, &py_file).expect("should read from cache");
        assert_eq!(content2, "x = 1 + 2\n");
    }

    /// `read_source_cached` returns an error for a non-existent file.
    #[test]
    fn read_source_cached_missing_file_errors() {
        let mut cache: HashMap<PathBuf, String> = HashMap::new();
        let path = PathBuf::from("/nonexistent/path/missing.py");
        let result = read_source_cached(&mut cache, &path);
        assert!(result.is_err());
    }

    /// `run_mutants` marks mutants as `NoCoverage` when the coverage map
    /// is empty.
    #[test]
    fn run_mutants_no_coverage_when_map_empty() {
        let mutant = mutation::Mutant {
            file_path: PathBuf::from("src/app.py"),
            line: 1_u32,
            column: 1_u32,
            byte_offset: 0_usize,
            byte_length: 1_usize,
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            mutator_name: "arithmetic_op".to_owned(),
        };

        let coverage_map = coverage::CoverageMap::new();
        let config = config::FestConfig::default();

        let results = run_mutants(&[mutant], &coverage_map, &config).expect("should succeed");

        assert_eq!(results.len(), 1_usize);
        assert_eq!(results[0_usize].status, mutation::MutantStatus::NoCoverage);
        assert!(results[0_usize].tests_run.is_empty());
    }

    /// `run_mutants` marks mutants as `NoCoverage` when the coverage map
    /// has entries but none for the mutant's file and line.
    #[test]
    fn run_mutants_no_coverage_when_line_not_covered() {
        let mutant = mutation::Mutant {
            file_path: PathBuf::from("src/app.py"),
            line: 10_u32,
            column: 1_u32,
            byte_offset: 50_usize,
            byte_length: 1_usize,
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            mutator_name: "arithmetic_op".to_owned(),
        };

        let mut coverage_map = coverage::CoverageMap::new();
        // Coverage for a different line.
        let _prev = coverage_map.insert(
            (PathBuf::from("src/app.py"), 5_u32),
            vec!["test_app.py::test_something".to_owned()],
        );

        let config = config::FestConfig::default();

        let results = run_mutants(&[mutant], &coverage_map, &config).expect("should succeed");

        assert_eq!(results.len(), 1_usize);
        assert_eq!(results[0_usize].status, mutation::MutantStatus::NoCoverage);
    }

    /// `run_mutants` marks mutants as `NoCoverage` when the covering test
    /// list is empty.
    #[test]
    fn run_mutants_no_coverage_when_test_list_empty() {
        let mutant = mutation::Mutant {
            file_path: PathBuf::from("src/app.py"),
            line: 1_u32,
            column: 1_u32,
            byte_offset: 0_usize,
            byte_length: 1_usize,
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            mutator_name: "arithmetic_op".to_owned(),
        };

        let mut coverage_map = coverage::CoverageMap::new();
        // Entry exists but with empty test list.
        let _prev = coverage_map.insert((PathBuf::from("src/app.py"), 1_u32), Vec::new());

        let config = config::FestConfig::default();

        let results = run_mutants(&[mutant], &coverage_map, &config).expect("should succeed");

        assert_eq!(results.len(), 1_usize);
        assert_eq!(results[0_usize].status, mutation::MutantStatus::NoCoverage);
    }

    /// `run_mutants` with an empty mutant slice returns an empty result.
    #[test]
    fn run_mutants_empty_slice() {
        let coverage_map = coverage::CoverageMap::new();
        let config = config::FestConfig::default();

        let results = run_mutants(&[], &coverage_map, &config).expect("should succeed");

        assert!(results.is_empty());
    }
}
