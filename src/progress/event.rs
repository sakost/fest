//! Render events sent from the pipeline to the terminal render task.
//!
//! Events flow through an unbounded channel from pipeline workers to
//! a dedicated tokio render task.  This decouples I/O from computation.

use core::time::Duration;

use crate::mutation::MutantStatus;

/// Lightweight summary of a mutant result for display.
///
/// Contains only the fields needed for terminal rendering, omitting the
/// full [`crate::mutation::MutantResult`] (which carries the potentially
/// large `tests_run` vector).
#[derive(Debug, Clone)]
#[allow(
    unreachable_pub,
    reason = "pub needed for sibling module access within progress"
)]
pub struct MutantDisplay {
    /// Outcome status of this mutant.
    pub status: MutantStatus,
    /// Source file path as a display string.
    pub file_path: String,
    /// Line number in the source file.
    pub line: u32,
    /// Name of the mutator that produced this mutant.
    pub mutator_name: String,
    /// Original source text before mutation.
    pub original_text: String,
    /// Mutated replacement text.
    pub mutated_text: String,
    /// Duration of the test run for this mutant.
    pub duration: Duration,
}

/// Aggregated summary data for the final scoreboard display.
#[derive(Debug, Clone)]
pub struct SummaryInfo {
    /// Mutation score as a percentage (0.0–100.0).
    pub score: f64,
    /// Number of killed mutants.
    pub killed: usize,
    /// Number of survived mutants.
    pub survived: usize,
    /// Number of timed-out mutants.
    pub timeouts: usize,
    /// Number of errored mutants.
    pub errors: usize,
    /// Number of mutants with no test coverage.
    pub no_coverage: usize,
    /// Total wall-clock duration of the run.
    pub duration: Duration,
}

/// Events that the pipeline sends to the render task.
///
/// Each variant represents a distinct milestone in the
/// mutation-testing pipeline, allowing the render task to update
/// the terminal accordingly.
#[derive(Debug)]
#[allow(
    unreachable_pub,
    reason = "pub needed for sibling module access within progress"
)]
pub enum RenderEvent {
    /// A pipeline phase has started (e.g. "Loading configuration").
    PhaseStart {
        /// Human-readable label for this phase.
        label: String,
    },
    /// A pipeline phase has completed successfully.
    PhaseComplete {
        /// Completion detail message (e.g. "Configuration loaded").
        detail: String,
        /// Optional count detail (e.g. "42 files", "fest.toml").
        count_detail: Option<String>,
        /// Time elapsed during this phase.
        elapsed: Duration,
    },
    /// Mutant execution has started.
    MutantsStart {
        /// Total number of mutants to process.
        total: u64,
    },
    /// A single mutant has been processed.
    MutantCompleted {
        /// Zero-based index of the completed mutant.
        index: usize,
        /// Total number of mutants.
        total: usize,
        /// Display-friendly summary of the mutant result.
        summary: MutantDisplay,
    },
    /// All mutants have been processed (or cancelled).
    MutantsFinish {
        /// Whether the run was cancelled by a signal.
        cancelled: bool,
    },
    /// Final summary of the mutation-testing run.
    FinalSummary(SummaryInfo),
    /// Shut down the render task gracefully.
    Shutdown,
}
