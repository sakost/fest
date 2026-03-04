//! Plain-text report formatter for terminal output.
//!
//! Produces a human-readable summary of the mutation-testing run including
//! overall statistics and a listing of survived mutants.

use core::fmt::Write as _;

use super::types::MutationReport;
use crate::mutation::MutantStatus;

/// Format a [`MutationReport`] as a plain-text summary string.
///
/// The output includes a header, aggregate statistics, and a listing of
/// any mutants that survived (i.e. were not detected by the test suite).
///
/// # Errors
///
/// Returns [`crate::Error::Report`] if string formatting fails.
#[inline]
pub fn format_text(report: &MutationReport) -> Result<String, crate::Error> {
    let mut output = String::new();

    write_header(&mut output)?;
    write_statistics(report, &mut output)?;
    write_survived_mutants(report, &mut output)?;

    Ok(output)
}

/// Write the report header.
fn write_header(output: &mut String) -> Result<(), crate::Error> {
    writeln!(output, "fest mutation testing report")
        .map_err(|err| crate::Error::Report(format!("failed to format report header: {err}")))?;
    writeln!(output, "----------------------------")
        .map_err(|err| crate::Error::Report(format!("failed to format report header: {err}")))?;
    Ok(())
}

/// Write aggregate statistics.
fn write_statistics(report: &MutationReport, output: &mut String) -> Result<(), crate::Error> {
    let score = report.mutation_score();

    writeln!(output, "Files scanned:      {}", report.files_scanned)
        .map_err(|err| crate::Error::Report(format!("failed to format statistics: {err}")))?;
    writeln!(output, "Mutants generated:  {}", report.mutants_generated)
        .map_err(|err| crate::Error::Report(format!("failed to format statistics: {err}")))?;

    if report.no_coverage > 0 {
        writeln!(
            output,
            "Mutants tested:     {}  ({} no coverage)",
            report.mutants_tested, report.no_coverage
        )
        .map_err(|err| crate::Error::Report(format!("failed to format statistics: {err}")))?;
    } else {
        writeln!(output, "Mutants tested:     {}", report.mutants_tested)
            .map_err(|err| crate::Error::Report(format!("failed to format statistics: {err}")))?;
    }

    write_score_line("Killed:", report.killed, score, output)?;
    write_survived_line(report, output)?;

    writeln!(output, "Timeout:            {}", report.timeouts)
        .map_err(|err| crate::Error::Report(format!("failed to format statistics: {err}")))?;
    writeln!(output, "Errors:             {}", report.errors)
        .map_err(|err| crate::Error::Report(format!("failed to format statistics: {err}")))?;

    Ok(())
}

/// Write the "Killed" score line with percentage.
fn write_score_line(
    label: &str,
    count: usize,
    score: f64,
    output: &mut String,
) -> Result<(), crate::Error> {
    writeln!(output, "{label:<20}{count}  ({score:.1}%)")
        .map_err(|err| crate::Error::Report(format!("failed to format score line: {err}")))?;
    Ok(())
}

/// Write the "Survived" line with the complement percentage.
fn write_survived_line(report: &MutationReport, output: &mut String) -> Result<(), crate::Error> {
    let survived_pct = if report.mutants_tested == 0 {
        0.0_f64
    } else {
        #[allow(
            clippy::cast_precision_loss,
            reason = "mutant counts are small enough to fit in f64 mantissa"
        )]
        let pct = (report.survived as f64) / (report.mutants_tested as f64) * 100.0_f64;
        pct
    };
    writeln!(
        output,
        "{:<20}{}  ({:.1}%)",
        "Survived:", report.survived, survived_pct
    )
    .map_err(|err| crate::Error::Report(format!("failed to format survived line: {err}")))?;
    Ok(())
}

/// Write listing of survived mutants.
fn write_survived_mutants(
    report: &MutationReport,
    output: &mut String,
) -> Result<(), crate::Error> {
    let survived: Vec<_> = report
        .results
        .iter()
        .filter(|result| result.status == MutantStatus::Survived)
        .collect();

    if survived.is_empty() {
        return Ok(());
    }

    writeln!(output)
        .map_err(|err| crate::Error::Report(format!("failed to format survived list: {err}")))?;
    writeln!(output, "Survived mutants:")
        .map_err(|err| crate::Error::Report(format!("failed to format survived list: {err}")))?;

    for result in survived {
        let mutant = &result.mutant;
        writeln!(
            output,
            "  {}:{}    {}    `{}` -> `{}`",
            mutant.file_path.display(),
            mutant.line,
            mutant.mutator_name,
            mutant.original_text,
            mutant.mutated_text,
        )
        .map_err(|err| crate::Error::Report(format!("failed to format survived mutant: {err}")))?;
    }

    Ok(())
}
