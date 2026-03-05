//! CLI argument parsing for fest.
//!
//! This module provides clap-based argument parsing and a [`merge_config`]
//! function that applies CLI overrides on top of a [`FestConfig`] loaded from
//! a configuration file.

use std::path::PathBuf;

use clap::Parser;

use crate::config::{FestConfig, OutputFormat, RunnerBackend};

// ---------------------------------------------------------------------------
// Progress style
// ---------------------------------------------------------------------------

/// Progress output style for the CLI.
///
/// Controls how pipeline progress is displayed on stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum ProgressStyle {
    /// Automatically select fancy on TTY stderr, quiet otherwise.
    #[default]
    Auto,
    /// Rich output: phase animations, progress bar, colored summary.
    Fancy,
    /// Uncolored plain-text output with timing (no overwrite, no bar).
    Plain,
    /// Colored per-mutant lines and phase checkmarks (no progress bar).
    Verbose,
    /// Suppress all stderr progress output.
    Quiet,
}

// ---------------------------------------------------------------------------
// Top-level CLI
// ---------------------------------------------------------------------------

/// fest -- a fast mutation-testing tool for Python.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Available subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Run mutation testing (default when no subcommand is given).
    Run(RunArgs),
}

/// Arguments for the `run` subcommand.
#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Enable verbose per-mutant progress output.
    ///
    /// Prints one line per mutant to stderr showing the status, file, mutator,
    /// and duration. When not set, a progress bar is shown on TTY stderr, or
    /// no output on non-TTY stderr.
    #[arg(short, long)]
    pub verbose: bool,

    /// Glob patterns matching Python source files to mutate.
    ///
    /// Overrides the `source` list from the config file.
    #[arg(short, long)]
    pub source: Option<Vec<String>>,

    /// Glob patterns for files to exclude from mutation.
    ///
    /// Overrides the `exclude` list from the config file.
    #[arg(short, long)]
    pub exclude: Option<Vec<String>>,

    /// Number of parallel test workers.
    ///
    /// Overrides `workers` in the config file.
    #[arg(short, long)]
    pub workers: Option<usize>,

    /// Fraction of available CPUs to use when `--workers` is not set.
    ///
    /// Overrides `workers_cpu_ratio` in the config file.
    #[arg(long)]
    pub workers_cpu_ratio: Option<f64>,

    /// Timeout in seconds for each individual test run.
    ///
    /// Overrides `timeout` in the config file.
    #[arg(short, long)]
    pub timeout: Option<u64>,

    /// Minimum mutation score (0-100) to consider the run successful.
    ///
    /// Overrides `fail_under` in the config file.
    #[arg(long)]
    pub fail_under: Option<f64>,

    /// Output format for the mutation report.
    ///
    /// Overrides `output` in the config file.
    #[arg(short, long)]
    pub output: Option<OutputFormat>,

    /// Path to the configuration file.
    ///
    /// When omitted, fest searches the current directory for `fest.toml`
    /// or `pyproject.toml`.
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Disable mtime-based coverage caching.
    ///
    /// Forces a fresh coverage collection even when `.coverage.json` exists
    /// and all `.py` files are older than it.
    #[arg(long)]
    pub no_coverage_cache: bool,

    /// Path to a pre-existing `.coverage` or `.coverage.json` file.
    ///
    /// When set, skips running pytest for coverage and uses this file
    /// directly. `.json` files are parsed immediately; `.coverage` `SQLite`
    /// databases are exported to JSON first.
    #[arg(long)]
    pub coverage_from: Option<PathBuf>,

    /// Disable forcing the fast C-based coverage tracer.
    ///
    /// By default fest sets `COVERAGE_CORE=ctrace` for faster coverage
    /// collection. Use this flag to let coverage.py pick its own backend.
    #[arg(long)]
    pub no_fast_coverage: bool,

    /// Execution backend: "subprocess" (default) or "plugin".
    ///
    /// Overrides `backend` in the config file.
    #[arg(short, long)]
    pub backend: Option<RunnerBackend>,

    /// Progress output style: auto, fancy, plain, verbose, quiet.
    ///
    /// `auto` selects `fancy` on TTY stderr, `quiet` otherwise.
    /// `plain` forces uncolored plain-text output.
    /// `verbose` shows one colored line per mutant.
    /// `quiet` suppresses all progress output.
    #[arg(long, default_value = "auto")]
    pub progress: ProgressStyle,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse command-line arguments via [`clap`].
#[inline]
#[must_use]
pub fn parse() -> Args {
    Args::parse()
}

/// Extract [`RunArgs`] from parsed CLI [`Args`].
///
/// When no subcommand is given the default is `run`, so this always
/// produces a valid [`RunArgs`].
#[inline]
#[must_use]
pub fn run_args(args: Args) -> RunArgs {
    match args.command {
        Some(Command::Run(run_args)) => run_args,
        None => RunArgs {
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
            progress: ProgressStyle::Auto,
        },
    }
}

/// Merge CLI overrides into a [`FestConfig`].
///
/// For each field where the user provided a CLI flag, the config-file value
/// is replaced. Fields that were not passed on the CLI retain the value from
/// the configuration file.
#[inline]
#[must_use]
pub fn merge_config(args: &RunArgs, mut config: FestConfig) -> FestConfig {
    if let Some(source) = args.source.as_ref() {
        config.source.clone_from(source);
    }
    if let Some(exclude) = args.exclude.as_ref() {
        config.exclude.clone_from(exclude);
    }
    if let Some(workers) = args.workers {
        config.workers = Some(workers);
    }
    if let Some(ratio) = args.workers_cpu_ratio {
        config.workers_cpu_ratio = ratio;
    }
    if let Some(timeout) = args.timeout {
        config.timeout = timeout;
    }
    if let Some(fail_under) = args.fail_under {
        config.fail_under = Some(fail_under);
    }
    if let Some(output) = args.output.as_ref() {
        config.output = output.clone();
    }
    if args.no_coverage_cache {
        config.coverage_cache = false;
    }
    if let Some(coverage_from) = args.coverage_from.as_ref() {
        config.coverage_from = Some(coverage_from.clone());
    }
    if args.no_fast_coverage {
        config.fast_coverage = false;
    }
    if let Some(backend) = args.backend.as_ref() {
        config.backend = backend.clone();
    }
    config
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `RunArgs` with all defaults — override individual fields
    /// via struct update syntax in each test.
    fn default_args() -> RunArgs {
        RunArgs {
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
            progress: ProgressStyle::Auto,
        }
    }

    /// When no CLI overrides are given, config is returned unchanged.
    #[test]
    fn merge_no_overrides_returns_config_unchanged() {
        let config = FestConfig::default();
        let merged = merge_config(&default_args(), config.clone());
        assert_eq!(merged, config);
    }

    /// CLI `--source` replaces the config source list.
    #[test]
    fn merge_source_override() {
        let args = RunArgs {
            source: Some(vec!["tests/**/*.py".to_owned()]),
            ..default_args()
        };
        let merged = merge_config(&args, FestConfig::default());
        assert_eq!(merged.source, vec!["tests/**/*.py"]);
    }

    /// CLI `--exclude` replaces the config exclude list.
    #[test]
    fn merge_exclude_override() {
        let args = RunArgs {
            exclude: Some(vec!["vendor/**".to_owned()]),
            ..default_args()
        };
        let merged = merge_config(&args, FestConfig::default());
        assert_eq!(merged.exclude, vec!["vendor/**"]);
    }

    /// CLI `--workers` overrides the config workers field.
    #[test]
    fn merge_workers_override() {
        let args = RunArgs {
            workers: Some(16_usize),
            ..default_args()
        };
        let merged = merge_config(&args, FestConfig::default());
        assert_eq!(merged.workers, Some(16_usize));
    }

    /// CLI `--workers-cpu-ratio` overrides the config ratio.
    #[test]
    fn merge_workers_cpu_ratio_override() {
        let args = RunArgs {
            workers_cpu_ratio: Some(0.5),
            ..default_args()
        };
        let merged = merge_config(&args, FestConfig::default());
        assert!((merged.workers_cpu_ratio - 0.5).abs() < f64::EPSILON);
    }

    /// CLI `--timeout` overrides the config timeout.
    #[test]
    fn merge_timeout_override() {
        let args = RunArgs {
            timeout: Some(120_u64),
            ..default_args()
        };
        let merged = merge_config(&args, FestConfig::default());
        assert_eq!(merged.timeout, 120_u64);
    }

    /// CLI `--fail-under` overrides the config fail_under.
    #[test]
    fn merge_fail_under_override() {
        let args = RunArgs {
            fail_under: Some(90.0),
            ..default_args()
        };
        let merged = merge_config(&args, FestConfig::default());
        assert!((merged.fail_under.unwrap_or(0.0) - 90.0).abs() < f64::EPSILON);
    }

    /// CLI `--output` overrides the config output format.
    #[test]
    fn merge_output_override() {
        let args = RunArgs {
            output: Some(OutputFormat::Json),
            ..default_args()
        };
        let merged = merge_config(&args, FestConfig::default());
        assert_eq!(merged.output, OutputFormat::Json);
    }

    /// Multiple CLI overrides are all applied simultaneously.
    #[test]
    fn merge_multiple_overrides() {
        let args = RunArgs {
            source: Some(vec!["lib/**/*.py".to_owned()]),
            exclude: Some(vec!["lib/generated/**".to_owned()]),
            workers: Some(4_usize),
            timeout: Some(30_u64),
            fail_under: Some(85.0),
            output: Some(OutputFormat::Html),
            ..default_args()
        };
        let merged = merge_config(&args, FestConfig::default());
        assert_eq!(merged.source, vec!["lib/**/*.py"]);
        assert_eq!(merged.exclude, vec!["lib/generated/**"]);
        assert_eq!(merged.workers, Some(4_usize));
        assert_eq!(merged.timeout, 30_u64);
        assert!((merged.fail_under.unwrap_or(0.0) - 85.0).abs() < f64::EPSILON);
        assert_eq!(merged.output, OutputFormat::Html);
        // Unset fields keep their config-file values
        assert!((merged.workers_cpu_ratio - 0.75).abs() < f64::EPSILON);
    }

    /// CLI `--backend` overrides the config backend.
    #[test]
    fn merge_backend_override() {
        let config = FestConfig::default();
        assert_eq!(config.backend, RunnerBackend::Plugin);

        let args = RunArgs {
            backend: Some(RunnerBackend::Plugin),
            ..default_args()
        };
        let merged = merge_config(&args, config);
        assert_eq!(merged.backend, RunnerBackend::Plugin);
    }

    /// CLI overrides take priority over non-default config values.
    #[test]
    fn merge_overrides_non_default_config() {
        let mut config = FestConfig::default();
        config.timeout = 60_u64;
        config.workers = Some(2_usize);
        config.output = OutputFormat::Json;

        let args = RunArgs {
            workers: Some(8_usize),
            timeout: Some(120_u64),
            output: Some(OutputFormat::Text),
            ..default_args()
        };
        let merged = merge_config(&args, config);
        assert_eq!(merged.workers, Some(8_usize));
        assert_eq!(merged.timeout, 120_u64);
        assert_eq!(merged.output, OutputFormat::Text);
    }

    /// CLI `--no-coverage-cache` sets `coverage_cache = false`.
    #[test]
    fn merge_no_coverage_cache() {
        let config = FestConfig::default();
        assert!(config.coverage_cache);

        let args = RunArgs {
            no_coverage_cache: true,
            ..default_args()
        };
        let merged = merge_config(&args, config);
        assert!(!merged.coverage_cache);
    }

    /// CLI `--coverage-from` populates `coverage_from` in config.
    #[test]
    fn merge_coverage_from() {
        let config = FestConfig::default();
        assert!(config.coverage_from.is_none());

        let args = RunArgs {
            coverage_from: Some(PathBuf::from("my/.coverage.json")),
            ..default_args()
        };
        let merged = merge_config(&args, config);
        assert_eq!(
            merged.coverage_from,
            Some(PathBuf::from("my/.coverage.json"))
        );
    }
}
