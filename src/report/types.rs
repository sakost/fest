//! Core types for mutation-testing reports.
//!
//! [`MutationReport`] aggregates the results of a full mutation-testing run
//! and provides convenience methods for computing the mutation score and
//! checking it against a threshold.

use core::time::Duration;

use serde::Serialize;

use crate::mutation::{MutantResult, MutantStatus};

/// Aggregated results of a complete mutation-testing run.
///
/// Constructed via [`from_results`](Self::from_results), which derives the
/// various counters from the raw [`MutantResult`] vector.
#[derive(Debug, Clone, Serialize)]
pub struct MutationReport {
    /// Number of Python source files that were scanned for mutations.
    pub files_scanned: usize,
    /// Total number of mutants generated across all files.
    pub mutants_generated: usize,
    /// Number of mutants that were actually tested (excludes no-coverage).
    pub mutants_tested: usize,
    /// Number of mutants skipped because no tests cover them.
    pub no_coverage: usize,
    /// Number of mutants detected (killed) by the test suite.
    pub killed: usize,
    /// Number of mutants that survived (tests still passed).
    pub survived: usize,
    /// Number of mutants whose test run exceeded the timeout.
    pub timeouts: usize,
    /// Number of mutants whose test run encountered an error.
    pub errors: usize,
    /// Individual results for every mutant tested.
    pub results: Vec<MutantResult>,
    /// Total wall-clock duration of the mutation-testing run.
    pub duration: Duration,
}

impl MutationReport {
    /// Build a report from raw results and run metadata.
    ///
    /// Iterates through `results` to count killed, survived, timeout,
    /// no-coverage, and error outcomes.
    #[inline]
    #[must_use]
    pub fn from_results(
        results: Vec<MutantResult>,
        files_scanned: usize,
        mutants_generated: usize,
        duration: Duration,
    ) -> Self {
        let mut killed: usize = 0;
        let mut survived: usize = 0;
        let mut timeouts: usize = 0;
        let mut no_coverage: usize = 0;
        let mut errors: usize = 0;

        for result in &results {
            match result.status {
                MutantStatus::Killed => killed += 1,
                MutantStatus::Survived => survived += 1,
                MutantStatus::Timeout => timeouts += 1,
                MutantStatus::NoCoverage => no_coverage += 1,
                MutantStatus::Error(_) => errors += 1,
            }
        }

        let mutants_tested: usize = killed + survived + timeouts + errors;

        Self {
            files_scanned,
            mutants_generated,
            mutants_tested,
            no_coverage,
            killed,
            survived,
            timeouts,
            errors,
            results,
            duration,
        }
    }

    /// Compute the mutation score as a percentage.
    ///
    /// The score is `killed / tested * 100`. Returns `0.0` when no mutants
    /// were tested.
    #[inline]
    #[must_use]
    pub fn mutation_score(&self) -> f64 {
        if self.mutants_tested == 0 {
            return 0.0;
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "mutant counts are small enough to fit in f64 mantissa"
        )]
        let score = (self.killed as f64) / (self.mutants_tested as f64) * 100.0_f64;
        score
    }

    /// Check whether the mutation score meets or exceeds the given threshold.
    ///
    /// A threshold of `0.0` always passes. The comparison uses `>=`.
    #[inline]
    #[must_use]
    pub fn passes_threshold(&self, threshold: f64) -> bool {
        self.mutation_score() >= threshold
    }
}
