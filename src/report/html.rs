//! HTML report formatter with source-annotated output.
//!
//! Generates a self-contained HTML document (all CSS inline) that shows a
//! summary section and per-file source views with line-by-line annotations
//! colored by mutation status.

extern crate alloc;

use alloc::collections::BTreeMap;
use core::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::types::MutationReport;
use crate::mutation::{MutantResult, MutantStatus};

/// Format a [`MutationReport`] as a self-contained HTML document.
///
/// The output includes inline CSS, a summary statistics section, and
/// per-file source views with color-coded line annotations based on
/// mutation outcomes.
///
/// # Errors
///
/// Returns [`crate::Error::Report`] if string formatting fails.
#[inline]
pub fn format_html(report: &MutationReport) -> Result<String, crate::Error> {
    let mut output = String::new();

    write_document_open(&mut output)?;
    write_summary_section(report, &mut output)?;
    write_file_sections(report, &mut output)?;
    write_document_close(&mut output)?;

    Ok(output)
}

// ---------------------------------------------------------------------------
// Document structure helpers
// ---------------------------------------------------------------------------

/// Write the opening HTML structure including `DOCTYPE`, `<head>`, and
/// inline CSS styles.
fn write_document_open(output: &mut String) -> Result<(), crate::Error> {
    writeln!(output, "<!DOCTYPE html>").map_err(fmt_err)?;
    writeln!(output, "<html lang=\"en\">").map_err(fmt_err)?;
    write_head_section(output)?;
    writeln!(output, "<body>").map_err(fmt_err)?;
    writeln!(output, "<div class=\"container\">").map_err(fmt_err)?;
    writeln!(output, "<h1>fest mutation testing report</h1>").map_err(fmt_err)?;
    Ok(())
}

/// Write the `<head>` element with inline CSS.
fn write_head_section(output: &mut String) -> Result<(), crate::Error> {
    writeln!(output, "<head>").map_err(fmt_err)?;
    writeln!(output, "<meta charset=\"utf-8\">").map_err(fmt_err)?;
    writeln!(output, "<title>fest mutation testing report</title>").map_err(fmt_err)?;
    write_inline_css(output)?;
    writeln!(output, "</head>").map_err(fmt_err)?;
    Ok(())
}

/// Write the inline `<style>` block with all CSS rules.
fn write_inline_css(output: &mut String) -> Result<(), crate::Error> {
    writeln!(output, "<style>").map_err(fmt_err)?;
    write_css_body(output);
    writeln!(output, "</style>").map_err(fmt_err)?;
    Ok(())
}

/// Write the CSS rule definitions.
fn write_css_body(output: &mut String) {
    let css = concat!(
        "body { font-family: monospace; margin: 0; padding: 20px; ",
        "background: #f5f5f5; }\n",
        ".container { max-width: 1200px; margin: 0 auto; }\n",
        "h1 { color: #333; }\n",
        "h2 { color: #555; margin-top: 30px; }\n",
        ".summary { background: #fff; padding: 15px; ",
        "border-radius: 5px; margin-bottom: 20px; }\n",
        ".summary td { padding: 2px 10px; }\n",
        ".file-section { background: #fff; padding: 15px; ",
        "border-radius: 5px; margin-bottom: 20px; }\n",
        ".line { white-space: pre; }\n",
        ".line-num { display: inline-block; width: 50px; ",
        "text-align: right; padding-right: 10px; ",
        "color: #999; user-select: none; }\n",
        ".killed { background-color: #d4edda; }\n",
        ".survived { background-color: #f8d7da; }\n",
        ".no-coverage { background-color: #e2e3e5; }\n",
        ".mutation-detail { padding-left: 70px; font-size: 0.9em; ",
        "color: #555; border-left: 3px solid #ccc; ",
        "margin-left: 50px; }\n",
        ".status-killed { color: #155724; }\n",
        ".status-survived { color: #721c24; }\n",
        ".status-timeout { color: #856404; }\n",
        ".status-no-coverage { color: #383d41; }\n",
        ".status-error { color: #721c24; font-style: italic; }\n",
    );
    output.push_str(css);
}

/// Write the closing HTML tags for the document.
fn write_document_close(output: &mut String) -> Result<(), crate::Error> {
    writeln!(output, "</div>").map_err(fmt_err)?;
    writeln!(output, "</body>").map_err(fmt_err)?;
    writeln!(output, "</html>").map_err(fmt_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Summary section
// ---------------------------------------------------------------------------

/// Write the summary statistics section of the report.
fn write_summary_section(report: &MutationReport, output: &mut String) -> Result<(), crate::Error> {
    writeln!(output, "<div class=\"summary\">").map_err(fmt_err)?;
    writeln!(output, "<h2>Summary</h2>").map_err(fmt_err)?;
    writeln!(output, "<table>").map_err(fmt_err)?;
    write_summary_rows(report, output)?;
    writeln!(output, "</table>").map_err(fmt_err)?;
    writeln!(output, "</div>").map_err(fmt_err)?;
    Ok(())
}

/// Write the individual summary table rows.
fn write_summary_rows(report: &MutationReport, output: &mut String) -> Result<(), crate::Error> {
    write_summary_row(output, "Files scanned", report.files_scanned)?;
    write_summary_row(output, "Mutants generated", report.mutants_generated)?;
    write_summary_row(output, "Mutants tested", report.mutants_tested)?;
    write_summary_row(output, "Killed", report.killed)?;
    write_summary_row(output, "Survived", report.survived)?;
    write_summary_row(output, "Timeout", report.timeouts)?;
    write_summary_row(output, "Errors", report.errors)?;
    write_summary_row(output, "No coverage", report.no_coverage)?;
    write_score_row(output, report.mutation_score())?;
    Ok(())
}

/// Write a single summary table row with a label and value.
fn write_summary_row(output: &mut String, label: &str, value: usize) -> Result<(), crate::Error> {
    writeln!(output, "<tr><td>{label}</td><td>{value}</td></tr>",).map_err(fmt_err)
}

/// Write the mutation score row with percentage formatting.
fn write_score_row(output: &mut String, score: f64) -> Result<(), crate::Error> {
    writeln!(
        output,
        "<tr><td><strong>Mutation score</strong></td><td><strong>{score:.1}%</strong></td></tr>",
    )
    .map_err(fmt_err)
}

// ---------------------------------------------------------------------------
// File sections
// ---------------------------------------------------------------------------

/// Group mutation results by file and write a section for each file.
fn write_file_sections(report: &MutationReport, output: &mut String) -> Result<(), crate::Error> {
    let grouped = group_results_by_file(&report.results);

    for (file_path, file_results) in &grouped {
        write_single_file_section(file_path, file_results, output)?;
    }

    Ok(())
}

/// Group a slice of [`MutantResult`] by file path into a sorted map.
///
/// Results within each file are further sorted by line number.
fn group_results_by_file(results: &[MutantResult]) -> BTreeMap<PathBuf, Vec<&MutantResult>> {
    let mut grouped: BTreeMap<PathBuf, Vec<&MutantResult>> = BTreeMap::new();

    for result in results {
        grouped
            .entry(result.mutant.file_path.clone())
            .or_default()
            .push(result);
    }

    // Sort each file's results by line number.
    for file_results in grouped.values_mut() {
        file_results.sort_by_key(|res: &&MutantResult| res.mutant.line);
    }

    grouped
}

/// Write the HTML section for a single source file.
fn write_single_file_section(
    file_path: &Path,
    file_results: &[&MutantResult],
    output: &mut String,
) -> Result<(), crate::Error> {
    let escaped_path = escape_html(&file_path.display().to_string());
    writeln!(output, "<div class=\"file-section\">").map_err(fmt_err)?;
    writeln!(output, "<h2>{escaped_path}</h2>").map_err(fmt_err)?;

    let by_line = group_results_by_line(file_results);
    write_annotated_lines(&by_line, output)?;

    writeln!(output, "</div>").map_err(fmt_err)?;
    Ok(())
}

/// Group file-level results by line number into a sorted map.
fn group_results_by_line<'src>(
    file_results: &[&'src MutantResult],
) -> BTreeMap<u32, Vec<&'src MutantResult>> {
    let mut by_line: BTreeMap<u32, Vec<&'src MutantResult>> = BTreeMap::new();

    for result in file_results {
        by_line.entry(result.mutant.line).or_default().push(result);
    }

    by_line
}

/// Write all annotated lines for a file section.
fn write_annotated_lines(
    by_line: &BTreeMap<u32, Vec<&MutantResult>>,
    output: &mut String,
) -> Result<(), crate::Error> {
    for (line_num, line_results) in by_line {
        let css_class = determine_line_class(line_results);
        write_line_header(*line_num, css_class, line_results, output)?;
        write_line_mutations(line_results, output)?;
    }
    Ok(())
}

/// Write the line number and source text for an annotated line.
///
/// Displays the `original_text` from the first mutation on the line
/// as inline `<code>`, giving the user context about the source code.
fn write_line_header(
    line_num: u32,
    css_class: &str,
    results: &[&MutantResult],
    output: &mut String,
) -> Result<(), crate::Error> {
    let source_text = results
        .first()
        .map(|res| escape_html(&res.mutant.original_text));
    let code_fragment = source_text.as_deref().unwrap_or("");

    writeln!(
        output,
        "<div class=\"line {css_class}\"><span class=\"line-num\">\
         {line_num}</span><code>{code_fragment}</code></div>",
    )
    .map_err(fmt_err)
}

/// Write mutation detail entries for all mutations on a line.
fn write_line_mutations(
    line_results: &[&MutantResult],
    output: &mut String,
) -> Result<(), crate::Error> {
    for result in line_results {
        write_single_mutation_detail(result, output)?;
    }
    Ok(())
}

/// Write a single mutation detail entry.
fn write_single_mutation_detail(
    result: &MutantResult,
    output: &mut String,
) -> Result<(), crate::Error> {
    let status_class = status_css_class(&result.status);
    let status_label = status_display_label(&result.status);
    let mutator = escape_html(&result.mutant.mutator_name);
    let original = escape_html(&result.mutant.original_text);
    let mutated = escape_html(&result.mutant.mutated_text);

    writeln!(
        output,
        "<div class=\"mutation-detail\"><span class=\"{status_class}\">[{status_label}]</span> \
         {mutator}: <code>{original}</code> &rarr; <code>{mutated}</code></div>",
    )
    .map_err(fmt_err)
}

// ---------------------------------------------------------------------------
// Classification helpers
// ---------------------------------------------------------------------------

/// Determine the CSS class for a source line based on mutation statuses.
///
/// Priority rules:
/// - If **any** mutant on the line survived, the line is red (`survived`).
/// - Else if **all** mutants were killed, the line is green (`killed`).
/// - Otherwise the line is grey (`no-coverage`).
fn determine_line_class(line_results: &[&MutantResult]) -> &'static str {
    let has_survived = line_results
        .iter()
        .any(|res| res.status == MutantStatus::Survived);
    if has_survived {
        return "survived";
    }

    let all_killed = line_results
        .iter()
        .all(|res| res.status == MutantStatus::Killed);
    if all_killed {
        return "killed";
    }

    "no-coverage"
}

/// Return the CSS class name for a mutation status.
const fn status_css_class(status: &MutantStatus) -> &'static str {
    match *status {
        MutantStatus::Killed => "status-killed",
        MutantStatus::Survived => "status-survived",
        MutantStatus::Timeout => "status-timeout",
        MutantStatus::NoCoverage => "status-no-coverage",
        MutantStatus::Error(_) => "status-error",
    }
}

/// Return a human-readable display label for a mutation status.
const fn status_display_label(status: &MutantStatus) -> &'static str {
    match *status {
        MutantStatus::Killed => "KILLED",
        MutantStatus::Survived => "SURVIVED",
        MutantStatus::Timeout => "TIMEOUT",
        MutantStatus::NoCoverage => "NO COVERAGE",
        MutantStatus::Error(_) => "ERROR",
    }
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

/// Escape HTML special characters to prevent rendering issues.
///
/// Replaces `&`, `<`, `>`, `"`, and `'` with their corresponding HTML
/// entities.
fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Convert a [`core::fmt::Error`] into a [`crate::Error::Report`].
fn fmt_err(err: core::fmt::Error) -> crate::Error {
    crate::Error::Report(format!("failed to format HTML report: {err}"))
}
