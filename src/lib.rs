//! **fest** — a fast mutation-testing tool for Python.
//!
//! This crate provides both a library API and a CLI binary for
//! generating mutants from Python source code, running a test suite
//! against each mutant, and reporting which mutants survived.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::{
    collections::HashMap,
    io::{IsTerminal as _, Write as _},
    path::{Path, PathBuf},
};

use rayon::iter::{IntoParallelRefIterator as _, ParallelIterator as _};

pub mod cli;
pub mod config;
pub mod coverage;
pub mod error;
pub mod init;
pub mod mutation;
pub mod plugin;
pub mod progress;
pub mod python;
pub mod report;
pub mod runner;
pub mod session;
pub mod signal;

pub use error::Error;

/// Shared context passed into the mutant execution loop.
///
/// Bundles the tokio runtime, progress reporter, and cancellation state
/// so that [`run_mutants`] stays within the 5-argument limit.
struct RunContext<'ctx> {
    /// Tokio async runtime for running test processes.
    runtime: &'ctx tokio::runtime::Runtime,
    /// Progress reporter for user feedback.
    progress: progress::ProgressReporter,
    /// Cancellation flag set by signal handlers.
    cancel: &'ctx signal::CancellationState,
}

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
///    the configured runner backend.
/// 6. Build a [`report::MutationReport`] and format it for output.
/// 7. Optionally check the mutation score against a `fail_under` threshold.
///
/// # Errors
///
/// Returns [`Error`] if any stage of the pipeline fails (configuration,
/// mutation, coverage, test-running, or report generation), or
/// [`Error::Cancelled`] if interrupted by a signal.
#[inline]
#[allow(
    clippy::needless_pass_by_value,
    reason = "RunArgs is consumed; pass-by-value is intentional for the public API"
)]
pub fn run(args: cli::RunArgs) -> Result<(), Error> {
    // Create the tokio runtime early so signal handlers can be installed.
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|err| Error::Runner(format!("failed to create tokio runtime: {err}")))?;

    // Resolve the render mode and spawn the render task.
    let mode = progress::resolve_render_mode(args.verbose, args.progress);
    let render = progress::RenderHandle::new(&runtime, mode);
    let reporter = render.reporter();

    // Install signal handlers for graceful cancellation.
    let cancel = signal::CancellationState::new();
    signal::install_signal_handlers(&runtime, &cancel)?;

    // Run preparation phases (config, discovery, coverage).
    let (config, mutants, coverage_map, files_scanned, project_dir) =
        run_preparation_phases(&args, &reporter)?;

    // Open session if configured, applying --reset and --incremental.
    let session_db = prepare_session(&config, &args, &reporter)?;

    // Determine which mutants to run: all, or only pending from session.
    let (mutants_to_run, prior_results) = resolve_session_mutants(session_db.as_ref(), &mutants)?;

    // Run mutants against the test suite.
    let total_u64 = u64::try_from(mutants_to_run.len()).unwrap_or(u64::MAX);
    reporter.phase_start("Running mutants");
    reporter.start_mutants(total_u64);

    let ctx = RunContext {
        runtime: &runtime,
        progress: reporter.clone(),
        cancel: &cancel,
    };
    let mutants_generated = mutants.len();
    let start = std::time::Instant::now();
    let (new_results, was_cancelled) =
        run_mutants(&mutants_to_run, &coverage_map, &config, &ctx, &project_dir)?;

    // Persist results to session if available.
    if let Some(sess) = session_db.as_ref() {
        for result in &new_results {
            sess.update_result(result)?;
        }
        // Store the current timestamp for incremental mode.
        let epoch_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _meta = sess.set_metadata("last_run_at", &epoch_secs.to_string());
    }
    let duration = start.elapsed();
    reporter.finish_mutants(was_cancelled);

    let tested = new_results.len() + prior_results.len();
    reporter.phase_complete(
        "Mutants tested",
        Some(&format!("{tested} mutants")),
        start.elapsed(),
    );

    // Combine prior results (from session) with new results.
    let mut results = prior_results;
    results.extend(new_results);

    // Build the report and emit the summary scoreboard.
    let report = report::MutationReport::from_results(
        results,
        files_scanned,
        mutants_generated,
        duration,
        config.seed,
    );
    emit_summary(&reporter, &report, duration);

    // Shut down the render task before writing the stdout report.
    runtime.block_on(render.shutdown());

    let colored = matches!(
        mode,
        progress::RenderMode::Fancy | progress::RenderMode::Verbose
    ) && std::io::stdout().is_terminal();
    let list_survived = !matches!(mode, progress::RenderMode::Fancy);
    let formatted = report::format_report(&report, &config.output, colored, list_survived)?;
    write_output(&formatted)?;

    check_final_status(was_cancelled, &report, &config)
}

/// Run the preparation phases of the pipeline: configuration, discovery,
/// mutant generation, and coverage collection.
///
/// Returns the merged config, generated mutants, coverage map, and file
/// count so that the caller can proceed with mutant execution.
///
/// # Errors
///
/// Returns [`Error`] if any preparation phase fails.
fn run_preparation_phases(
    args: &cli::RunArgs,
    reporter: &progress::ProgressReporter,
) -> Result<
    (
        config::FestConfig,
        Vec<mutation::Mutant>,
        coverage::CoverageMap,
        usize,
        PathBuf,
    ),
    Error,
> {
    // 1. Load configuration.
    reporter.phase_start("Loading configuration");
    let t0 = std::time::Instant::now();
    let project_dir = resolve_project_dir(args)?;
    let file_config = config::load(&project_dir)?;
    let config = cli::merge_config(args, file_config);
    reporter.phase_complete("Configuration loaded", Some("fest.toml"), t0.elapsed());

    // 2. Build the mutator registry.
    reporter.phase_start("Building mutator registry");
    let t1 = std::time::Instant::now();
    let registry = mutation::build_registry(&config.mutators);
    let registry_count = registry.len();
    reporter.phase_complete(
        "Mutator registry built",
        Some(&format!("{registry_count} mutators")),
        t1.elapsed(),
    );

    // 3. Discover source files and generate mutants.
    reporter.phase_start("Discovering source files");
    let t2 = std::time::Instant::now();
    let files = mutation::discover_files(&config.source, &config.exclude, &project_dir)?;
    let files_scanned = files.len();
    reporter.phase_complete(
        "Source files discovered",
        Some(&format!("{files_scanned} files")),
        t2.elapsed(),
    );

    reporter.phase_start("Generating mutants");
    let t3 = std::time::Instant::now();
    let gen_opts = mutation::GenerationOptions {
        seed: config.seed,
        filter_operators: &config.filter_operators,
        filter_paths: &config.filter_paths,
        per_file: &config.per_file,
        global_mutators: &config.mutators,
    };
    let mutants = mutation::generate_mutants_for_files(&files, &registry, &gen_opts)?;
    let mutants_generated = mutants.len();
    reporter.phase_complete(
        "Mutants generated",
        Some(&format!("{mutants_generated} mutants")),
        t3.elapsed(),
    );

    // 4. Collect coverage.
    reporter.phase_start("Collecting coverage");
    let t4 = std::time::Instant::now();
    let coverage_map = resolve_coverage(&config, &project_dir)?;
    reporter.phase_complete("Coverage collected", None, t4.elapsed());

    Ok((config, mutants, coverage_map, files_scanned, project_dir))
}

/// Send the summary scoreboard event to the render task.
fn emit_summary(
    reporter: &progress::ProgressReporter,
    report: &report::MutationReport,
    duration: core::time::Duration,
) {
    reporter.summary(progress::SummaryInfo {
        score: report.mutation_score(),
        killed: report.killed,
        survived: report.survived,
        timeouts: report.timeouts,
        errors: report.errors,
        no_coverage: report.no_coverage,
        duration,
    });
}

/// Check for cancellation and threshold violations after the report is
/// written.
///
/// # Errors
///
/// Returns [`Error::Cancelled`] if the run was interrupted by a signal,
/// or [`Error::Threshold`] if the mutation score is below `fail_under`.
fn check_final_status(
    was_cancelled: bool,
    report: &report::MutationReport,
    config: &config::FestConfig,
) -> Result<(), Error> {
    if was_cancelled {
        return Err(Error::Cancelled(
            "run interrupted by signal; partial results reported above".to_owned(),
        ));
    }
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

/// Resolve coverage data based on the configuration.
///
/// When `coverage_from` is set, loads from the user-provided file.
/// When `coverage_cache` is enabled and the cache is fresh, loads from
/// the cached `.coverage.json`. Otherwise, runs a full collection via
/// pytest.
///
/// # Errors
///
/// Returns [`Error::Coverage`] if the selected coverage source fails.
fn resolve_coverage(
    config: &config::FestConfig,
    project_dir: &Path,
) -> Result<coverage::CoverageMap, Error> {
    if let Some(path) = config.coverage_from.as_ref() {
        return coverage::load_coverage_from(path, project_dir);
    }
    if config.coverage_cache && coverage::is_coverage_cache_fresh(project_dir) {
        return coverage::load_cached_coverage(project_dir);
    }
    coverage::collect_coverage(project_dir, &config.source, config.fast_coverage)
}

/// Open a session database if configured.
///
/// When `config.session` is set, opens (or creates) the `SQLite` database
/// and stores the generated mutants. Returns `None` when no session is
/// configured.
///
/// # Errors
///
/// Returns [`Error::Session`] if the database cannot be opened.
fn open_session(
    config: &config::FestConfig,
    reporter: &progress::ProgressReporter,
) -> Result<Option<session::Session>, Error> {
    let Some(session_path) = config.session.as_ref() else {
        return Ok(None);
    };
    reporter.phase_start("Opening session");
    let t0 = std::time::Instant::now();
    let sess = session::Session::open(session_path)?;
    reporter.phase_complete(
        "Session opened",
        Some(&session_path.display().to_string()),
        t0.elapsed(),
    );
    Ok(Some(sess))
}

/// Prepare the session database: open, reset/incremental, store metadata.
///
/// # Errors
///
/// Returns [`Error::Session`] if any session operation fails.
fn prepare_session(
    config: &config::FestConfig,
    args: &cli::RunArgs,
    reporter: &progress::ProgressReporter,
) -> Result<Option<session::Session>, Error> {
    let session_db = open_session(config, reporter)?;
    let Some(sess) = session_db else {
        return Ok(None);
    };
    if args.reset {
        sess.delete_all_mutants()?;
    }
    if args.incremental {
        let changed = find_changed_source_files(&sess)?;
        if !changed.is_empty() {
            let _count = sess.reset_stale_files(&changed)?;
        }
    }
    sess.store_run_metadata(config.seed, env!("CARGO_PKG_VERSION"))?;
    Ok(Some(sess))
}

/// Find source files that have changed since the session was last run.
///
/// Reads `last_run_at` from the session metadata and compares it against
/// file modification times. Returns file paths whose mtime is newer.
///
/// # Errors
///
/// Returns [`Error::Session`] if the session database query fails.
fn find_changed_source_files(sess: &session::Session) -> Result<Vec<PathBuf>, Error> {
    let last_run = sess.get_metadata("last_run_at")?;
    let Some(timestamp_str) = last_run else {
        // No previous run — treat all files as changed (no-op: reset won't change pending).
        return Ok(Vec::new());
    };

    // Parse the stored timestamp (seconds since epoch).
    let last_run_secs: u64 = timestamp_str.parse().unwrap_or(0_u64);
    let last_run_time = std::time::UNIX_EPOCH + core::time::Duration::from_secs(last_run_secs);

    // Get distinct file paths from the session.
    let pending = sess.load_pending_mutants()?;
    let completed = sess.load_completed_results()?;

    let mut file_paths: Vec<PathBuf> = pending
        .iter()
        .map(|mutant| mutant.file_path.clone())
        .chain(completed.iter().map(|res| res.mutant.file_path.clone()))
        .collect();
    file_paths.sort();
    file_paths.dedup();

    let changed: Vec<PathBuf> = file_paths
        .into_iter()
        .filter(|path| {
            path.metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|mtime| mtime > last_run_time)
        })
        .collect();

    Ok(changed)
}

/// Determine which mutants to run based on session state.
///
/// If a session is active, stores all generated mutants and returns only
/// the pending ones. Previously completed results are returned separately.
/// Without a session, returns all mutants with no prior results.
///
/// # Errors
///
/// Returns [`Error::Session`] if a session database operation fails.
fn resolve_session_mutants(
    session_db: Option<&session::Session>,
    mutants: &[mutation::Mutant],
) -> Result<(Vec<mutation::Mutant>, Vec<mutation::MutantResult>), Error> {
    let Some(sess) = session_db else {
        return Ok((mutants.to_vec(), Vec::new()));
    };

    sess.store_mutants(mutants)?;
    let pending = sess.load_pending_mutants()?;
    let completed = sess.load_completed_results()?;
    Ok((pending, completed))
}

/// Run all mutants in parallel and collect their results.
///
/// The execution proceeds in three phases:
///
/// **Phase A — Pre-read source files.** Scans the mutants and coverage map
/// to find which files have at least one covered mutant, then reads them
/// into a `HashMap<PathBuf, String>` so that the parallel phase can share
/// the cache via a plain `&HashMap` (no locks needed).
///
/// **Phase B — Parallel execution.** Builds a scoped rayon thread pool
/// sized to [`config::FestConfig::resolved_workers`] and maps over the
/// mutants with `par_iter`. Each task checks for cancellation, looks up
/// coverage, and either marks the mutant as `NoCoverage` or runs the test
/// suite via the configured runner backend.
///
/// **Phase C — Collect.** Filters out `None` results (from cancelled
/// tasks) and detects whether any cancellation occurred.
///
/// Returns the collected results and a boolean indicating whether the run
/// was cancelled by a signal.
///
/// # Errors
///
/// Returns [`Error`] if a source file cannot be read or the thread pool
/// fails to build.
#[allow(
    clippy::too_many_lines,
    reason = "main pipeline orchestration function"
)]
fn run_mutants(
    mutants: &[mutation::Mutant],
    coverage_map: &coverage::CoverageMap,
    config: &config::FestConfig,
    ctx: &RunContext<'_>,
    project_dir: &Path,
) -> Result<(Vec<mutation::MutantResult>, bool), Error> {
    let runner = runner::build_runner(&config.backend, config.timeout, project_dir.to_path_buf());
    let total = mutants.len();

    // Phase A: Pre-read source files that have at least one covered mutant.
    let source_cache = build_source_cache(mutants, coverage_map)?;

    // Phase B: Parallel execution via rayon.
    let num_workers = config.resolved_workers();

    // Start the runner (spawns persistent workers for the plugin backend).
    ctx.progress.phase_start("Starting test workers");
    let start_workers = std::time::Instant::now();
    ctx.runtime
        .block_on(runner.start(num_workers, project_dir))?;
    if runner.did_plugin_fail() {
        ctx.progress
            .warning("plugin runner failed to start, falling back to subprocess");
    }
    ctx.progress.phase_complete(
        "Test workers ready",
        Some(&format!("{num_workers} workers")),
        start_workers.elapsed(),
    );

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_workers)
        .build()
        .map_err(|err| Error::Runner(format!("failed to build thread pool: {err}")))?;

    let completed = AtomicUsize::new(0_usize);
    let was_cancelled = AtomicBool::new(false);

    let parallel_output: Result<Vec<Option<mutation::MutantResult>>, Error> = pool.install(|| {
        mutants
            .par_iter()
            .map(|mutant| {
                // Check for cancellation.
                if ctx.cancel.is_cancelled() {
                    was_cancelled.store(true, Ordering::Relaxed);
                    return Ok(None);
                }

                // Look up coverage for this mutant's file and line.
                let coverage_key = (mutant.file_path.clone(), mutant.line);
                let covering_tests = coverage_map.get(&coverage_key);

                let result = match covering_tests {
                    Some(tests) if !tests.is_empty() => {
                        let source = source_cache
                            .get(&mutant.file_path)
                            .map(String::as_str)
                            .ok_or_else(|| {
                                Error::Mutation(format!(
                                    "source cache missing for covered file {}",
                                    mutant.file_path.display()
                                ))
                            })?;
                        ctx.runtime
                            .block_on(runner.run_mutant(mutant, source, tests))?
                    }
                    _ => mutation::MutantResult {
                        mutant: mutant.clone(),
                        status: mutation::MutantStatus::NoCoverage,
                        tests_run: Vec::new(),
                        duration: core::time::Duration::from_secs(0_u64),
                    },
                };

                let index = completed.fetch_add(1_usize, Ordering::Relaxed);
                ctx.progress.report_mutant(index, total, &result);
                Ok(Some(result))
            })
            .collect()
    });

    // Warn if the plugin failed mid-run and fell back to subprocess.
    if runner.did_plugin_fail() {
        ctx.progress
            .warning("plugin runner failed mid-run, fell back to subprocess");
    }

    // Stop the runner (shuts down persistent workers).
    let _stop = ctx.runtime.block_on(runner.stop());

    // Phase C: Collect results, filtering out None (cancelled) entries.
    let results: Vec<mutation::MutantResult> = parallel_output?.into_iter().flatten().collect();
    let cancelled = was_cancelled.load(Ordering::Relaxed);

    Ok((results, cancelled))
}

/// Pre-read source files that have at least one covered mutant.
///
/// Scans all mutants and checks whether their file + line appears in the
/// coverage map with a non-empty test list. Files that match are read into
/// the returned cache so the parallel phase can share the data lock-free.
///
/// # Errors
///
/// Returns [`Error::Mutation`] if a source file cannot be read.
fn build_source_cache(
    mutants: &[mutation::Mutant],
    coverage_map: &coverage::CoverageMap,
) -> Result<HashMap<PathBuf, String>, Error> {
    let mut cache = HashMap::new();
    for mutant in mutants {
        if cache.contains_key(&mutant.file_path) {
            continue;
        }
        let key = (mutant.file_path.clone(), mutant.line);
        let needs_source = coverage_map
            .get(&key)
            .is_some_and(|tests| !tests.is_empty());
        if needs_source {
            let content = std::fs::read_to_string(&mutant.file_path).map_err(|err| {
                Error::Mutation(format!(
                    "failed to read source file {}: {err}",
                    mutant.file_path.display()
                ))
            })?;
            let _prev = cache.insert(mutant.file_path.clone(), content);
        }
    }
    Ok(cache)
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

    /// Helper to build a `RunContext` with a default runtime, quiet reporter,
    /// and fresh cancellation state. Returns owned values and the context.
    struct TestRunContext {
        /// Tokio runtime.
        runtime: tokio::runtime::Runtime,
        /// Render handle for the quiet-mode render task.
        render: Option<progress::RenderHandle>,
        /// Cancellation state.
        cancel: signal::CancellationState,
    }

    impl TestRunContext {
        /// Create a new test context with quiet progress.
        fn new() -> Self {
            let runtime = tokio::runtime::Runtime::new().expect("should create runtime");
            let render = progress::RenderHandle::new(&runtime, progress::RenderMode::Quiet);
            Self {
                runtime,
                render: Some(render),
                cancel: signal::CancellationState::new(),
            }
        }

        /// Borrow as a `RunContext`.
        fn as_ctx(&self) -> RunContext<'_> {
            RunContext {
                runtime: &self.runtime,
                progress: self.render.as_ref().expect("render handle").reporter(),
                cancel: &self.cancel,
            }
        }
    }

    impl Drop for TestRunContext {
        fn drop(&mut self) {
            if let Some(handle) = self.render.take() {
                self.runtime.block_on(handle.shutdown());
            }
        }
    }

    /// Build a `cli::RunArgs` with all defaults — override individual fields
    /// via struct update syntax in each test.
    fn default_run_args() -> cli::RunArgs {
        cli::RunArgs {
            verbose: false,
            source: None,
            exclude: None,
            workers: None,
            workers_cpu_ratio: None,
            timeout: None,
            fail_under: None,
            output: None,
            config: None,
            no_coverage_cache: false,
            coverage_from: None,
            no_fast_coverage: false,
            backend: None,
            progress: cli::ProgressStyle::Auto,
            seed: None,
            filter_operators: None,
            filter_paths: None,
            session: None,
            reset: false,
            incremental: false,
        }
    }

    /// Build a test `Mutant` with sensible defaults — override fields via
    /// struct update syntax.
    fn test_mutant() -> mutation::Mutant {
        mutation::Mutant {
            file_path: PathBuf::from("src/app.py"),
            line: 1_u32,
            column: 1_u32,
            byte_offset: 0_usize,
            byte_length: 1_usize,
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            mutator_name: "arithmetic_op".to_owned(),
        }
    }

    /// `resolve_project_dir` returns the parent of a config path.
    #[test]
    fn resolve_project_dir_from_config_path() {
        let args = cli::RunArgs {
            config: Some(PathBuf::from("/home/user/project/fest.toml")),
            ..default_run_args()
        };

        let dir = resolve_project_dir(&args).expect("should resolve");
        assert_eq!(dir, PathBuf::from("/home/user/project"));
    }

    /// `resolve_project_dir` falls back to the current working directory.
    #[test]
    fn resolve_project_dir_cwd_fallback() {
        let dir = resolve_project_dir(&default_run_args()).expect("should resolve");
        let cwd = std::env::current_dir().expect("should get cwd");
        assert_eq!(dir, cwd);
    }

    /// `resolve_project_dir` handles a config path with no parent
    /// (bare filename) by returning `"."`.
    #[test]
    fn resolve_project_dir_bare_filename() {
        let args = cli::RunArgs {
            config: Some(PathBuf::from("fest.toml")),
            ..default_run_args()
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

    /// `build_source_cache` reads covered files into the cache.
    #[test]
    fn build_source_cache_reads_covered_files() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let py_file = dir.path().join("cached.py");
        std::fs::write(&py_file, "x = 1 + 2\n").expect("write file");

        let mutant = mutation::Mutant {
            file_path: py_file.clone(),
            column: 5_u32,
            byte_offset: 4_usize,
            ..test_mutant()
        };

        let mut coverage_map = coverage::CoverageMap::new();
        let _prev = coverage_map.insert(
            (py_file.clone(), 1_u32),
            vec!["test_app.py::test_it".to_owned()],
        );

        let cache = build_source_cache(&[mutant], &coverage_map).expect("should build");
        assert_eq!(cache.get(&py_file).map(String::as_str), Some("x = 1 + 2\n"));
    }

    /// `build_source_cache` skips files with no coverage.
    #[test]
    fn build_source_cache_skips_uncovered() {
        let coverage_map = coverage::CoverageMap::new();
        let cache = build_source_cache(&[test_mutant()], &coverage_map).expect("should build");
        assert!(cache.is_empty());
    }

    /// `build_source_cache` returns an error for a non-existent file.
    #[test]
    fn build_source_cache_missing_file_errors() {
        let mutant = mutation::Mutant {
            file_path: PathBuf::from("/nonexistent/path/missing.py"),
            ..test_mutant()
        };

        let mut coverage_map = coverage::CoverageMap::new();
        let _prev = coverage_map.insert(
            (PathBuf::from("/nonexistent/path/missing.py"), 1_u32),
            vec!["test.py::test_x".to_owned()],
        );

        let result = build_source_cache(&[mutant], &coverage_map);
        assert!(result.is_err());
    }

    /// `run_mutants` marks mutants as `NoCoverage` when the coverage map
    /// is empty.
    #[test]
    fn run_mutants_no_coverage_when_map_empty() {
        let coverage_map = coverage::CoverageMap::new();
        let config = config::FestConfig::default();
        let test_ctx = TestRunContext::new();

        let (results, cancelled) = run_mutants(
            &[test_mutant()],
            &coverage_map,
            &config,
            &test_ctx.as_ctx(),
            Path::new("."),
        )
        .expect("should succeed");

        assert!(!cancelled);
        assert_eq!(results.len(), 1_usize);
        assert_eq!(results[0_usize].status, mutation::MutantStatus::NoCoverage);
        assert!(results[0_usize].tests_run.is_empty());
    }

    /// `run_mutants` marks mutants as `NoCoverage` when the coverage map
    /// has entries but none for the mutant's file and line.
    #[test]
    fn run_mutants_no_coverage_when_line_not_covered() {
        let mutant = mutation::Mutant {
            line: 10_u32,
            byte_offset: 50_usize,
            ..test_mutant()
        };

        let mut coverage_map = coverage::CoverageMap::new();
        // Coverage for a different line.
        let _prev = coverage_map.insert(
            (PathBuf::from("src/app.py"), 5_u32),
            vec!["test_app.py::test_something".to_owned()],
        );

        let config = config::FestConfig::default();
        let test_ctx = TestRunContext::new();

        let (results, cancelled) = run_mutants(
            &[mutant],
            &coverage_map,
            &config,
            &test_ctx.as_ctx(),
            Path::new("."),
        )
        .expect("should succeed");

        assert!(!cancelled);
        assert_eq!(results.len(), 1_usize);
        assert_eq!(results[0_usize].status, mutation::MutantStatus::NoCoverage);
    }

    /// `run_mutants` marks mutants as `NoCoverage` when the covering test
    /// list is empty.
    #[test]
    fn run_mutants_no_coverage_when_test_list_empty() {
        let mut coverage_map = coverage::CoverageMap::new();
        // Entry exists but with empty test list.
        let _prev = coverage_map.insert((PathBuf::from("src/app.py"), 1_u32), Vec::new());

        let config = config::FestConfig::default();
        let test_ctx = TestRunContext::new();

        let (results, cancelled) = run_mutants(
            &[test_mutant()],
            &coverage_map,
            &config,
            &test_ctx.as_ctx(),
            Path::new("."),
        )
        .expect("should succeed");

        assert!(!cancelled);
        assert_eq!(results.len(), 1_usize);
        assert_eq!(results[0_usize].status, mutation::MutantStatus::NoCoverage);
    }

    /// `run_mutants` with an empty mutant slice returns an empty result.
    #[test]
    fn run_mutants_empty_slice() {
        let coverage_map = coverage::CoverageMap::new();
        let config = config::FestConfig::default();
        let test_ctx = TestRunContext::new();

        let (results, cancelled) = run_mutants(
            &[],
            &coverage_map,
            &config,
            &test_ctx.as_ctx(),
            Path::new("."),
        )
        .expect("should succeed");

        assert!(!cancelled);
        assert!(results.is_empty());
    }

    /// `run_mutants` returns partial results when cancelled.
    #[test]
    fn run_mutants_cancelled_returns_partial() {
        let mutants: Vec<mutation::Mutant> = (0_u32..5_u32)
            .map(|idx| mutation::Mutant {
                line: idx + 1_u32,
                ..test_mutant()
            })
            .collect();

        let coverage_map = coverage::CoverageMap::new();
        let config = config::FestConfig::default();
        let test_ctx = TestRunContext::new();

        // Pre-cancel before running.
        test_ctx.cancel.set_cancelled_for_test();

        let (results, cancelled) = run_mutants(
            &mutants,
            &coverage_map,
            &config,
            &test_ctx.as_ctx(),
            Path::new("."),
        )
        .expect("should succeed");

        assert!(cancelled);
        // Should have 0 results since cancellation is checked before each mutant.
        assert!(results.is_empty());
    }

    /// Parallel execution preserves input order of mutant results.
    #[test]
    fn run_mutants_parallel_preserves_order() {
        let mutants: Vec<mutation::Mutant> = (0_u32..20_u32)
            .map(|idx| mutation::Mutant {
                file_path: PathBuf::from(format!("src/mod_{idx}.py")),
                line: idx + 1_u32,
                ..test_mutant()
            })
            .collect();

        let coverage_map = coverage::CoverageMap::new();
        let config = config::FestConfig::default();
        let test_ctx = TestRunContext::new();

        let (results, cancelled) = run_mutants(
            &mutants,
            &coverage_map,
            &config,
            &test_ctx.as_ctx(),
            Path::new("."),
        )
        .expect("should succeed");

        assert!(!cancelled);
        assert_eq!(results.len(), 20_usize);

        // Results must preserve the original mutant order.
        for (idx, result) in results.iter().enumerate() {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "idx is at most 19, fits in u32"
            )]
            let expected_line = (idx as u32) + 1_u32;
            assert_eq!(
                result.mutant.line, expected_line,
                "result at index {idx} has wrong line"
            );
            assert_eq!(result.status, mutation::MutantStatus::NoCoverage);
        }
    }
}
