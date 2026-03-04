//! Configuration types for fest.
//!
//! This module defines the structs that represent the configuration of a fest
//! mutation-testing run. They can be deserialized from `fest.toml` or from the
//! `[tool.fest]` section of `pyproject.toml`.

use serde::{Deserialize, Serialize};

/// Output format for the mutation-testing report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
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
            custom: Vec::new(),
            python: Vec::new(),
            dylib: Vec::new(),
        }
    }
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
    /// Explicit worker count. If `None`, computed from [`workers_cpu_ratio`](Self::workers_cpu_ratio).
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
    /// Mutator configuration.
    #[serde(default)]
    pub mutators: MutatorConfig,
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
            mutators: MutatorConfig::default(),
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
