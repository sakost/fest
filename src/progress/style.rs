//! Terminal styling and formatting utilities for progress output.
//!
//! Provides [`format_duration`], [`format_status_tag`], and per-mutant
//! line formatting used by the render loop and verbose output.

use core::time::Duration;

use console::style;

use super::event::{MutantDisplay, SummaryInfo};
use crate::mutation::MutantStatus;

// ---------------------------------------------------------------------------
// Duration formatting
// ---------------------------------------------------------------------------

/// Format a [`Duration`] for human-readable display.
///
/// - Sub-second: `"42ms"`
/// - Under a minute: `"4.2s"`
/// - Over a minute: `"2m 34s"`
#[inline]
#[must_use]
pub(super) fn format_duration(duration: Duration) -> String {
    let total_ms = duration.as_millis();
    if total_ms < 1000_u128 {
        return format!("{total_ms}ms");
    }

    let secs = duration.as_secs();
    if secs < 60_u64 {
        #[allow(clippy::integer_division, reason = "extracting tenths of a second")]
        let tenths = duration.subsec_millis() / 100_u32;
        return format!("{secs}.{tenths}s");
    }

    #[allow(clippy::integer_division, reason = "computing minutes from seconds")]
    let mins = secs / 60_u64;
    let rem_secs = secs % 60_u64;
    format!("{mins}m {rem_secs}s")
}

// ---------------------------------------------------------------------------
// Status tag formatting
// ---------------------------------------------------------------------------

/// Format a status tag string (e.g. `"KILLED"`, `"SURVIVED"`).
#[inline]
#[must_use]
pub(super) const fn format_status_tag(status: &MutantStatus) -> &'static str {
    match *status {
        MutantStatus::Killed => "KILLED",
        MutantStatus::Survived => "SURVIVED",
        MutantStatus::Timeout => "TIMEOUT",
        MutantStatus::NoCoverage => "NO_COV",
        MutantStatus::Error(_) => "ERROR",
    }
}

// ---------------------------------------------------------------------------
// Per-mutant line formatting
// ---------------------------------------------------------------------------

/// Format a single mutant result line for plain (uncolored) verbose output.
///
/// Produces lines like:
/// `[42/847] KILLED     src/app.py:5  arithmetic_op  \`+\` -> \`-\`  (125ms)`
#[cfg(test)]
#[inline]
#[must_use]
pub(super) fn format_mutant_line(index: usize, total: usize, summary: &MutantDisplay) -> String {
    let tag = format_status_tag(&summary.status);
    let duration_ms = summary.duration.as_millis();
    format!(
        "[{idx}/{total}] {tag:<10} {path}:{line}  {mutator}  `{orig}` -> `{mutated}`  \
         ({duration_ms}ms)",
        idx = index + 1_usize,
        path = summary.file_path,
        line = summary.line,
        mutator = summary.mutator_name,
        orig = summary.original_text,
        mutated = summary.mutated_text,
    )
}

/// Format a single mutant result line with terminal colors.
///
/// Uses green for killed, red for survived, yellow for timeout,
/// and dim for no-coverage mutants.
#[must_use]
pub(super) fn format_colored_mutant_line(
    index: usize,
    total: usize,
    summary: &MutantDisplay,
) -> String {
    let tag = format_status_tag(&summary.status);
    let (icon, styled_tag) = status_icon_and_tag(tag, &summary.status);

    let location = style(format!("{}:{}", summary.file_path, summary.line))
        .cyan()
        .force_styling(true);
    let mutator = style(&summary.mutator_name).dim().force_styling(true);
    let timing = style(format!("({}ms)", summary.duration.as_millis()))
        .dim()
        .force_styling(true);

    format!(
        "  [{idx}/{total}] {icon} {styled_tag:<10} {location}  {mutator}  `{orig}` -> `{mutated}`  {timing}",
        idx = index + 1_usize,
        orig = summary.original_text,
        mutated = summary.mutated_text,
    )
}

/// Return the styled icon and tag for a given status.
fn status_icon_and_tag(
    tag: &str,
    status: &MutantStatus,
) -> (
    console::StyledObject<&'static str>,
    console::StyledObject<String>,
) {
    let tag_owned = tag.to_owned();
    match *status {
        MutantStatus::Killed => (
            style("✔").green().bold().force_styling(true),
            style(tag_owned).green().bold().force_styling(true),
        ),
        MutantStatus::Survived | MutantStatus::Error(_) => (
            style("✘").red().bold().force_styling(true),
            style(tag_owned).red().bold().force_styling(true),
        ),
        MutantStatus::Timeout => (
            style("✘").yellow().bold().force_styling(true),
            style(tag_owned).yellow().bold().force_styling(true),
        ),
        MutantStatus::NoCoverage => (
            style("∅").dim().force_styling(true),
            style(tag_owned).dim().force_styling(true),
        ),
    }
}

// ---------------------------------------------------------------------------
// Summary / banner formatting
// ---------------------------------------------------------------------------

/// Format the final summary scoreboard line.
///
/// The score is colored by threshold: green (≥ 90 %), yellow (70–90 %),
/// red (< 70 %).  When `colored` is false, produces a plain-text line.
#[must_use]
pub(super) fn format_summary_line(info: &SummaryInfo, colored: bool) -> String {
    let score_str = format!("{:.1}%", info.score);
    let separator = if colored { "┃" } else { "|" };

    let score_display = styled_score(&score_str, info.score, colored);
    let dim_sep = if colored {
        style(separator).dim().force_styling(true).to_string()
    } else {
        separator.to_owned()
    };

    format!(
        "  Mutation Score: {score_display}  {dim_sep}  Killed: {killed}  Survived: {survived}  \
         Timeout: {timeouts}  Errors: {errors}",
        killed = info.killed,
        survived = info.survived,
        timeouts = info.timeouts,
        errors = info.errors,
    )
}

/// Apply score-threshold coloring to the given score string.
///
/// Returns `score_str` unmodified when `colored` is false.  Otherwise
/// applies green (≥ 90 %), yellow (70–90 %), or red (< 70 %) styling.
#[allow(
    unreachable_pub,
    reason = "re-exported as pub(crate) from progress module"
)]
pub fn styled_score(score_str: &str, score: f64, colored: bool) -> String {
    if !colored {
        return score_str.to_owned();
    }
    if score >= 90.0 {
        style(score_str)
            .green()
            .bold()
            .force_styling(true)
            .to_string()
    } else if score >= 70.0 {
        style(score_str)
            .yellow()
            .bold()
            .force_styling(true)
            .to_string()
    } else {
        style(score_str)
            .red()
            .bold()
            .force_styling(true)
            .to_string()
    }
}

/// Format the startup banner line.
#[must_use]
pub(super) fn format_banner(colored: bool) -> String {
    let version = env!("CARGO_PKG_VERSION");
    if colored {
        let name = style("fest").cyan().bold().force_styling(true);
        let ver = style(format!("v{version}")).dim().force_styling(true);
        format!("{name} {ver} — mutation testing for Python")
    } else {
        format!("fest v{version} — mutation testing for Python")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_milliseconds() {
        let dur = Duration::from_millis(42_u64);
        assert_eq!(format_duration(dur), "42ms");
    }

    #[test]
    fn format_duration_seconds() {
        let dur = Duration::from_millis(4200_u64);
        assert_eq!(format_duration(dur), "4.2s");
    }

    #[test]
    fn format_duration_minutes() {
        let dur = Duration::from_secs(154_u64);
        assert_eq!(format_duration(dur), "2m 34s");
    }

    #[test]
    fn format_duration_exact_minute() {
        let dur = Duration::from_secs(60_u64);
        assert_eq!(format_duration(dur), "1m 0s");
    }

    #[test]
    fn format_duration_zero() {
        let dur = Duration::from_millis(0_u64);
        assert_eq!(format_duration(dur), "0ms");
    }

    #[test]
    fn format_duration_boundary_999ms() {
        let dur = Duration::from_millis(999_u64);
        assert_eq!(format_duration(dur), "999ms");
    }

    #[test]
    fn format_duration_boundary_1000ms() {
        let dur = Duration::from_millis(1000_u64);
        assert_eq!(format_duration(dur), "1.0s");
    }

    #[test]
    fn status_tag_mapping() {
        assert_eq!(format_status_tag(&MutantStatus::Killed), "KILLED");
        assert_eq!(format_status_tag(&MutantStatus::Survived), "SURVIVED");
        assert_eq!(format_status_tag(&MutantStatus::Timeout), "TIMEOUT");
        assert_eq!(format_status_tag(&MutantStatus::NoCoverage), "NO_COV");
        assert_eq!(
            format_status_tag(&MutantStatus::Error("oops".to_owned())),
            "ERROR"
        );
    }

    #[test]
    fn format_mutant_line_killed() {
        let summary = MutantDisplay {
            status: MutantStatus::Killed,
            file_path: "src/app.py".to_owned(),
            line: 5_u32,
            mutator_name: "arithmetic_op".to_owned(),
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            duration: Duration::from_millis(125_u64),
        };
        let line = format_mutant_line(41_usize, 847_usize, &summary);
        assert!(line.starts_with("[42/847]"));
        assert!(line.contains("KILLED"));
        assert!(line.contains("src/app.py:5"));
        assert!(line.contains("arithmetic_op"));
        assert!(line.contains("`+` -> `-`"));
        assert!(line.contains("(125ms)"));
    }

    #[test]
    fn format_mutant_line_one_based_index() {
        let summary = MutantDisplay {
            status: MutantStatus::Survived,
            file_path: "src/app.py".to_owned(),
            line: 1_u32,
            mutator_name: "op".to_owned(),
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            duration: Duration::from_millis(10_u64),
        };
        let line = format_mutant_line(0_usize, 10_usize, &summary);
        assert!(line.starts_with("[1/10]"));
    }

    #[test]
    fn format_banner_plain() {
        let banner = format_banner(false);
        assert!(banner.starts_with("fest v"));
        assert!(banner.contains("mutation testing for Python"));
        // No ANSI escape codes.
        assert!(!banner.contains("\x1b["));
    }

    #[test]
    fn format_banner_colored() {
        let banner = format_banner(true);
        assert!(banner.contains("fest"));
        assert!(banner.contains("mutation testing for Python"));
        // Should contain ANSI escape codes.
        assert!(banner.contains("\x1b["));
    }

    #[test]
    fn format_summary_plain() {
        let info = SummaryInfo {
            score: 93.8,
            killed: 750_usize,
            survived: 50_usize,
            timeouts: 0_usize,
            errors: 0_usize,
            no_coverage: 47_usize,
            duration: Duration::from_secs(154_u64),
        };
        let line = format_summary_line(&info, false);
        assert!(line.contains("93.8%"));
        assert!(line.contains("Killed: 750"));
        assert!(line.contains("Survived: 50"));
        assert!(!line.contains("\x1b["));
    }

    #[test]
    fn format_summary_colored() {
        let info = SummaryInfo {
            score: 93.8,
            killed: 750_usize,
            survived: 50_usize,
            timeouts: 0_usize,
            errors: 0_usize,
            no_coverage: 47_usize,
            duration: Duration::from_secs(154_u64),
        };
        let line = format_summary_line(&info, true);
        assert!(line.contains("93.8%"));
        assert!(line.contains("\x1b[")); // ANSI present
    }
}
