//! Progress reporting for the mutation-testing pipeline.
//!
//! Provides [`ProgressReporter`] which adapts its output style based on
//! the terminal environment: verbose per-mutant lines, a progress bar, or
//! silent (for CI / piped output).

use std::io::{IsTerminal as _, Write as _};

use indicatif::{ProgressBar, ProgressStyle};

use crate::mutation::{MutantResult, MutantStatus};

/// How progress information is displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    /// Print one line per mutant to stderr.
    Verbose,
    /// Show an indicatif progress bar (when stderr is a TTY).
    Bar,
    /// No output at all (CI / piped stderr).
    Quiet,
}

/// Reports pipeline progress to stderr.
#[derive(Debug)]
pub struct ProgressReporter {
    /// Active display mode.
    mode: ProgressMode,
    /// Optional progress bar (only present in [`ProgressMode::Bar`]).
    bar: Option<ProgressBar>,
}

impl ProgressReporter {
    /// Create a new reporter.
    ///
    /// - `verbose = true` → [`ProgressMode::Verbose`]
    /// - stderr is a TTY  → [`ProgressMode::Bar`]
    /// - otherwise         → [`ProgressMode::Quiet`]
    #[inline]
    #[must_use]
    pub fn new(verbose: bool) -> Self {
        let mode = if verbose {
            ProgressMode::Verbose
        } else if std::io::stderr().is_terminal() {
            ProgressMode::Bar
        } else {
            ProgressMode::Quiet
        };
        Self { mode, bar: None }
    }

    /// Create a reporter with an explicit mode (useful for testing).
    #[cfg(test)]
    #[must_use]
    fn with_mode(mode: ProgressMode) -> Self {
        Self { mode, bar: None }
    }

    /// Print a phase header to stderr.
    ///
    /// Example: `[1/7] Loading configuration...`
    #[inline]
    pub fn phase(&self, msg: &str) {
        if self.mode == ProgressMode::Quiet {
            return;
        }
        let mut stderr = std::io::stderr().lock();
        // Ignore write errors on stderr — best-effort.
        let _result = writeln!(stderr, "{msg}");
    }

    /// Initialise progress tracking for the mutant execution phase.
    #[inline]
    pub fn start_mutants(&mut self, total: u64) {
        if self.mode != ProgressMode::Bar {
            return;
        }
        let bar = ProgressBar::new(total);
        let style = ProgressStyle::with_template(
            "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} mutants ({eta} remaining)",
        );
        if let Ok(style) = style {
            bar.set_style(style.progress_chars("#>-"));
        }
        self.bar = Some(bar);
    }

    /// Report the result of a single mutant.
    #[inline]
    pub fn report_mutant(&self, index: usize, total: usize, result: &MutantResult) {
        match self.mode {
            ProgressMode::Verbose => {
                let line = format_mutant_line(index, total, result);
                let mut stderr = std::io::stderr().lock();
                let _result = writeln!(stderr, "{line}");
            }
            ProgressMode::Bar => {
                if let Some(bar) = self.bar.as_ref() {
                    bar.set_message(format_status_tag(result.status.clone()));
                    bar.inc(1_u64);
                }
            }
            ProgressMode::Quiet => {}
        }
    }

    /// Mark progress as successfully finished.
    #[inline]
    pub fn finish(&self) {
        if let Some(bar) = self.bar.as_ref() {
            bar.finish_and_clear();
        }
    }

    /// Mark progress as abandoned (e.g. due to cancellation).
    #[inline]
    pub fn abandon(&self) {
        if let Some(bar) = self.bar.as_ref() {
            bar.abandon_with_message("cancelled");
        }
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Format a single mutant result line for verbose output.
///
/// Example: `[42/847] KILLED    src/app.py:5  arithmetic_op  \`+\` -> \`-\`  (125ms)`
#[must_use]
pub fn format_mutant_line(index: usize, total: usize, result: &MutantResult) -> String {
    let status_tag = format_status_tag(result.status.clone());
    let path = result.mutant.file_path.display();
    let line = result.mutant.line;
    let mutator = &result.mutant.mutator_name;
    let original = &result.mutant.original_text;
    let mutated = &result.mutant.mutated_text;
    let duration_ms = result.duration.as_millis();

    format!(
        "[{index}/{total}] {status_tag:<10} {path}:{line}  {mutator}  `{original}` -> `{mutated}`  ({duration_ms}ms)",
        index = index + 1_usize,
    )
}

/// Format a status tag (e.g. `KILLED`, `SURVIVED`).
#[must_use]
pub fn format_status_tag(status: MutantStatus) -> String {
    match status {
        MutantStatus::Killed => "KILLED".to_owned(),
        MutantStatus::Survived => "SURVIVED".to_owned(),
        MutantStatus::Timeout => "TIMEOUT".to_owned(),
        MutantStatus::NoCoverage => "NO_COV".to_owned(),
        MutantStatus::Error(_) => "ERROR".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::mutation::{Mutant, MutantResult, MutantStatus};

    /// Helper to build a dummy mutant result for formatting tests.
    fn dummy_result(status: MutantStatus) -> MutantResult {
        MutantResult {
            mutant: Mutant {
                file_path: PathBuf::from("src/app.py"),
                line: 5_u32,
                column: 10_u32,
                byte_offset: 42_usize,
                byte_length: 1_usize,
                original_text: "+".to_owned(),
                mutated_text: "-".to_owned(),
                mutator_name: "arithmetic_op".to_owned(),
            },
            status,
            tests_run: vec!["test_add".to_owned()],
            duration: core::time::Duration::from_millis(125_u64),
        }
    }

    /// `format_status_tag` maps each variant correctly.
    #[test]
    fn status_tag_mapping() {
        assert_eq!(format_status_tag(MutantStatus::Killed), "KILLED");
        assert_eq!(format_status_tag(MutantStatus::Survived), "SURVIVED");
        assert_eq!(format_status_tag(MutantStatus::Timeout), "TIMEOUT");
        assert_eq!(format_status_tag(MutantStatus::NoCoverage), "NO_COV");
        assert_eq!(
            format_status_tag(MutantStatus::Error("oops".to_owned())),
            "ERROR"
        );
    }

    /// `format_mutant_line` produces the expected verbose output.
    #[test]
    fn format_mutant_line_killed() {
        let result = dummy_result(MutantStatus::Killed);
        let line = format_mutant_line(41_usize, 847_usize, &result);
        assert!(line.starts_with("[42/847]"));
        assert!(line.contains("KILLED"));
        assert!(line.contains("src/app.py:5"));
        assert!(line.contains("arithmetic_op"));
        assert!(line.contains("`+` -> `-`"));
        assert!(line.contains("(125ms)"));
    }

    /// `format_mutant_line` uses 1-based indexing.
    #[test]
    fn format_mutant_line_one_based_index() {
        let result = dummy_result(MutantStatus::Survived);
        let line = format_mutant_line(0_usize, 10_usize, &result);
        assert!(line.starts_with("[1/10]"));
    }

    /// Reporter can be constructed in each mode without panicking.
    #[test]
    fn reporter_construction() {
        let _verbose = ProgressReporter::with_mode(ProgressMode::Verbose);
        let _bar = ProgressReporter::with_mode(ProgressMode::Bar);
        let _quiet = ProgressReporter::with_mode(ProgressMode::Quiet);
    }

    /// Quiet reporter does not panic on any method call.
    #[test]
    fn quiet_reporter_no_ops() {
        let mut reporter = ProgressReporter::with_mode(ProgressMode::Quiet);
        reporter.phase("[1/7] Loading configuration...");
        reporter.start_mutants(100_u64);
        let result = dummy_result(MutantStatus::Killed);
        reporter.report_mutant(0_usize, 100_usize, &result);
        reporter.finish();
        reporter.abandon();
    }

    /// Verbose reporter does not panic on any method call.
    #[test]
    fn verbose_reporter_no_ops() {
        let mut reporter = ProgressReporter::with_mode(ProgressMode::Verbose);
        reporter.phase("[1/7] Loading configuration...");
        reporter.start_mutants(100_u64);
        let result = dummy_result(MutantStatus::Killed);
        reporter.report_mutant(0_usize, 100_usize, &result);
        reporter.finish();
    }
}
