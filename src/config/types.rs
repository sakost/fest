//! Configuration types for fest.
//!
//! This module defines the structs that represent the configuration of a fest
//! mutation-testing run. They can be deserialized from `fest.toml` or from the
//! `[tool.fest]` section of `pyproject.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Execution backend for running mutants against the test suite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum RunnerBackend {
    /// Subprocess runner: writes mutated file to tmpdir, spawns pytest.
    Subprocess,
    /// Pytest plugin runner: in-process patching via Unix socket protocol.
    /// Falls back to subprocess automatically on infrastructure errors.
    #[default]
    Plugin,
}

/// Output format for the mutation-testing report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// Plain-text report written to stdout.
    #[default]
    Text,
    /// Machine-readable JSON report.
    Json,
    /// HTML report written to a directory.
    Html,
}

/// Configuration for a single custom (text-pattern) mutator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomMutator {
    /// Human-readable name for this mutator.
    pub name: String,
    /// Text pattern to match in the source.
    pub pattern: String,
    /// Replacement text to substitute.
    pub replacement: String,
}

/// Configuration for a Python-based mutator plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythonMutator {
    /// Filesystem path to the Python mutator script.
    pub path: String,
}

/// Configuration for a shared-library (dylib) mutator plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DylibMutator {
    /// Filesystem path to the shared-library mutator.
    pub path: String,
}

/// Configuration for which mutators are enabled and any external plugins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool maps to a named mutator toggle in the config file"
)]
pub struct MutatorConfig {
    /// Enable the arithmetic-operator mutator.
    #[serde(default = "default_true")]
    pub arithmetic_op: bool,
    /// Enable the comparison-operator mutator.
    #[serde(default = "default_true")]
    pub comparison_op: bool,
    /// Enable the boolean-operator mutator.
    #[serde(default = "default_true")]
    pub boolean_op: bool,
    /// Enable the return-value mutator.
    #[serde(default = "default_true")]
    pub return_value: bool,
    /// Enable the negate-condition mutator.
    #[serde(default = "default_true")]
    pub negate_condition: bool,
    /// Enable the remove-decorator mutator.
    #[serde(default = "default_true")]
    pub remove_decorator: bool,
    /// Enable the constant-replace mutator.
    #[serde(default = "default_true")]
    pub constant_replace: bool,
    /// Enable the exception-swallow mutator.
    #[serde(default = "default_true")]
    pub exception_swallow: bool,
    /// Enable the break/continue swap mutator.
    #[serde(default = "default_true")]
    pub break_continue: bool,
    /// Enable the unary operator mutator.
    #[serde(default = "default_true")]
    pub unary_op: bool,
    /// Enable the zero-iteration loop mutator.
    #[serde(default = "default_true")]
    pub zero_iteration_loop: bool,
    /// Enable the augmented-assignment operator mutator.
    #[serde(default = "default_true")]
    pub augmented_assign: bool,
    /// Enable the statement-deletion mutator.
    #[serde(default = "default_false")]
    pub statement_deletion: bool,
    /// Enable the bitwise operator mutator.
    #[serde(default = "default_true")]
    pub bitwise_op: bool,
    /// Enable the super-call removal mutator.
    #[serde(default = "default_true")]
    pub remove_super_call: bool,
    /// Enable the variable-replace mutator (requires seed).
    #[serde(default = "default_false")]
    pub variable_replace: bool,
    /// Enable the variable-insert mutator (requires seed).
    #[serde(default = "default_false")]
    pub variable_insert: bool,
    /// Custom text-pattern mutators.
    #[serde(default)]
    pub custom: Vec<CustomMutator>,
    /// Python-based mutator plugins.
    #[serde(default)]
    pub python: Vec<PythonMutator>,
    /// Shared-library mutator plugins.
    #[serde(default)]
    pub dylib: Vec<DylibMutator>,
}

impl Default for MutatorConfig {
    #[inline]
    fn default() -> Self {
        Self {
            arithmetic_op: true,
            comparison_op: true,
            boolean_op: true,
            return_value: true,
            negate_condition: true,
            remove_decorator: true,
            constant_replace: true,
            exception_swallow: true,
            break_continue: true,
            unary_op: true,
            zero_iteration_loop: true,
            augmented_assign: true,
            statement_deletion: false,
            bitwise_op: true,
            remove_super_call: true,
            variable_replace: false,
            variable_insert: false,
            custom: Vec::new(),
            python: Vec::new(),
            dylib: Vec::new(),
        }
    }
}

/// Per-file mutator overrides.
///
/// Each field is `Option<bool>` — `Some(value)` overrides the global setting,
/// `None` keeps the global setting unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors MutatorConfig; each Option<bool> maps to a named mutator toggle"
)]
pub struct MutatorOverrides {
    /// Override the arithmetic-operator mutator.
    #[serde(default)]
    pub arithmetic_op: Option<bool>,
    /// Override the comparison-operator mutator.
    #[serde(default)]
    pub comparison_op: Option<bool>,
    /// Override the boolean-operator mutator.
    #[serde(default)]
    pub boolean_op: Option<bool>,
    /// Override the return-value mutator.
    #[serde(default)]
    pub return_value: Option<bool>,
    /// Override the negate-condition mutator.
    #[serde(default)]
    pub negate_condition: Option<bool>,
    /// Override the remove-decorator mutator.
    #[serde(default)]
    pub remove_decorator: Option<bool>,
    /// Override the constant-replace mutator.
    #[serde(default)]
    pub constant_replace: Option<bool>,
    /// Override the exception-swallow mutator.
    #[serde(default)]
    pub exception_swallow: Option<bool>,
    /// Override the break/continue swap mutator.
    #[serde(default)]
    pub break_continue: Option<bool>,
    /// Override the unary operator mutator.
    #[serde(default)]
    pub unary_op: Option<bool>,
    /// Override the zero-iteration loop mutator.
    #[serde(default)]
    pub zero_iteration_loop: Option<bool>,
    /// Override the augmented-assignment operator mutator.
    #[serde(default)]
    pub augmented_assign: Option<bool>,
    /// Override the statement-deletion mutator.
    #[serde(default)]
    pub statement_deletion: Option<bool>,
    /// Override the bitwise operator mutator.
    #[serde(default)]
    pub bitwise_op: Option<bool>,
    /// Override the super-call removal mutator.
    #[serde(default)]
    pub remove_super_call: Option<bool>,
    /// Override the variable-replace mutator.
    #[serde(default)]
    pub variable_replace: Option<bool>,
    /// Override the variable-insert mutator.
    #[serde(default)]
    pub variable_insert: Option<bool>,
}

impl MutatorConfig {
    /// Create a new `MutatorConfig` by merging overrides on top of `self`.
    ///
    /// Fields in `overrides` that are `Some(value)` replace the corresponding
    /// field in `self`; `None` fields keep the value from `self`.
    #[inline]
    #[must_use]
    pub fn with_overrides(&self, overrides: &MutatorOverrides) -> Self {
        Self {
            arithmetic_op: overrides.arithmetic_op.unwrap_or(self.arithmetic_op),
            comparison_op: overrides.comparison_op.unwrap_or(self.comparison_op),
            boolean_op: overrides.boolean_op.unwrap_or(self.boolean_op),
            return_value: overrides.return_value.unwrap_or(self.return_value),
            negate_condition: overrides.negate_condition.unwrap_or(self.negate_condition),
            remove_decorator: overrides.remove_decorator.unwrap_or(self.remove_decorator),
            constant_replace: overrides.constant_replace.unwrap_or(self.constant_replace),
            exception_swallow: overrides
                .exception_swallow
                .unwrap_or(self.exception_swallow),
            break_continue: overrides.break_continue.unwrap_or(self.break_continue),
            unary_op: overrides.unary_op.unwrap_or(self.unary_op),
            zero_iteration_loop: overrides
                .zero_iteration_loop
                .unwrap_or(self.zero_iteration_loop),
            augmented_assign: overrides.augmented_assign.unwrap_or(self.augmented_assign),
            statement_deletion: overrides
                .statement_deletion
                .unwrap_or(self.statement_deletion),
            bitwise_op: overrides.bitwise_op.unwrap_or(self.bitwise_op),
            remove_super_call: overrides
                .remove_super_call
                .unwrap_or(self.remove_super_call),
            variable_replace: overrides.variable_replace.unwrap_or(self.variable_replace),
            variable_insert: overrides.variable_insert.unwrap_or(self.variable_insert),
            custom: self.custom.clone(),
            python: self.python.clone(),
            dylib: self.dylib.clone(),
        }
    }
}

/// Per-file configuration override.
///
/// Matches files by glob pattern and allows overriding mutator settings,
/// timeout, or skipping mutation entirely for matching files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerFileConfig {
    /// Glob pattern to match file paths.
    pub pattern: String,
    /// Override mutator configuration for matching files (merged with globals).
    #[serde(default)]
    pub mutators: Option<MutatorOverrides>,
    /// Override timeout for matching files.
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Skip matching files entirely when `true`.
    #[serde(default)]
    pub skip: bool,
}

/// Top-level fest configuration.
///
/// This struct represents the full configuration for a fest run, loaded from
/// `fest.toml`, `pyproject.toml`, or falling back to defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FestConfig {
    /// Glob patterns matching Python source files to mutate.
    #[serde(default = "default_source")]
    pub source: Vec<String>,
    /// Glob patterns for files to exclude from mutation.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Name of the test runner command (e.g. `"pytest"`).
    #[serde(default = "default_test_runner")]
    pub test_runner: String,
    /// Explicit worker count. If `None`, computed from
    /// [`workers_cpu_ratio`](Self::workers_cpu_ratio).
    #[serde(default)]
    pub workers: Option<usize>,
    /// Fraction of available CPUs to use when `workers` is not set.
    #[serde(default = "default_workers_cpu_ratio")]
    pub workers_cpu_ratio: f64,
    /// Timeout in seconds for each individual test run.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// Minimum mutation score (0-100) to consider the run successful.
    #[serde(default)]
    pub fail_under: Option<f64>,
    /// Output format for the mutation report.
    #[serde(default)]
    pub output: OutputFormat,
    /// Execution backend for running mutants against the test suite.
    #[serde(default)]
    pub backend: RunnerBackend,
    /// Mutator configuration.
    #[serde(default)]
    pub mutators: MutatorConfig,
    /// Reuse cached `.coverage.json` when all `.py` files are older.
    #[serde(default = "default_true")]
    pub coverage_cache: bool,
    /// Path to a user-provided `.coverage` or `.coverage.json` file.
    #[serde(default)]
    pub coverage_from: Option<PathBuf>,
    /// Force the fastest C-based coverage tracer (`COVERAGE_CORE=ctrace`).
    #[serde(default = "default_true")]
    pub fast_coverage: bool,
    /// Seed for deterministic randomised mutation operators.
    #[serde(default)]
    pub seed: Option<u64>,
    /// Operator filter patterns. Prefix `!` excludes, no prefix includes.
    /// Patterns are matched as substrings against operator names.
    #[serde(default)]
    pub filter_operators: Vec<String>,
    /// Additional glob patterns to restrict which discovered files get mutated.
    #[serde(default)]
    pub filter_paths: Vec<String>,
    /// Path to a session database for stop/resume support.
    #[serde(default)]
    pub session: Option<PathBuf>,
    /// Per-file configuration overrides.
    #[serde(default, rename = "per-file")]
    pub per_file: Vec<PerFileConfig>,
}

impl Default for FestConfig {
    #[inline]
    fn default() -> Self {
        Self {
            source: default_source(),
            exclude: Vec::new(),
            test_runner: default_test_runner(),
            workers: None,
            workers_cpu_ratio: default_workers_cpu_ratio(),
            timeout: default_timeout(),
            fail_under: None,
            output: OutputFormat::default(),
            backend: RunnerBackend::default(),
            mutators: MutatorConfig::default(),
            coverage_cache: true,
            coverage_from: None,
            fast_coverage: true,
            seed: None,
            filter_operators: Vec::new(),
            filter_paths: Vec::new(),
            session: None,
            per_file: Vec::new(),
        }
    }
}

impl FestConfig {
    /// Resolve the effective number of worker threads.
    ///
    /// Returns [`workers`](Self::workers) if explicitly set, otherwise computes
    /// `floor(workers_cpu_ratio * available_cpus)` with a minimum of 1.
    #[inline]
    pub fn resolved_workers(&self) -> usize {
        if let Some(explicit) = self.workers {
            return explicit.max(1_usize);
        }

        let cpus: usize = std::thread::available_parallelism()
            .map(core::num::NonZero::get)
            .unwrap_or(1_usize);

        #[allow(
            clippy::cast_possible_truncation,
            reason = "intentional: converting f64 product to usize for worker count"
        )]
        #[allow(
            clippy::cast_sign_loss,
            reason = "ratio and cpu count are both non-negative so the product is non-negative"
        )]
        #[allow(
            clippy::cast_precision_loss,
            reason = "cpu count fits comfortably in f64 mantissa"
        )]
        let computed = (self.workers_cpu_ratio * (cpus as f64)) as usize;

        computed.max(1_usize)
    }
}

// ---------------------------------------------------------------------------
// Serde default helpers
// ---------------------------------------------------------------------------

/// Returns `true` — used as `#[serde(default)]` helper for boolean mutator flags.
const fn default_true() -> bool {
    true
}

/// Returns `false` — used as `#[serde(default)]` helper for opt-in mutator flags.
const fn default_false() -> bool {
    false
}

/// Default source globs.
fn default_source() -> Vec<String> {
    vec!["src/**/*.py".to_owned()]
}

/// Default test runner name.
fn default_test_runner() -> String {
    "pytest".to_owned()
}

/// Default CPU ratio for automatic worker-count calculation.
const fn default_workers_cpu_ratio() -> f64 {
    0.75
}

/// Default per-test timeout in seconds.
const fn default_timeout() -> u64 {
    10_u64
}
