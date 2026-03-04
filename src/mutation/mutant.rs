//! Data types for representing mutants and their execution results.
//!
//! A [`Mutant`] describes a single source-level mutation (what was changed,
//! where, and by which mutator).  After running the test suite against a
//! mutant the outcome is captured in a [`MutantResult`].

use core::time::Duration;
use std::path::PathBuf;

use serde::Serialize;

/// A single source-level mutation.
///
/// Contains all the information needed to locate the mutation in the
/// original source file and to apply the text replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mutant {
    /// Path to the Python source file that was mutated.
    pub file_path: PathBuf,
    /// 1-based line number where the mutation starts.
    pub line: u32,
    /// 1-based column number where the mutation starts.
    pub column: u32,
    /// Byte offset into the source text where the replaced region begins.
    pub byte_offset: usize,
    /// Length in bytes of the replaced region.
    pub byte_length: usize,
    /// The original source text that was replaced.
    pub original_text: String,
    /// The replacement text that constitutes the mutation.
    pub mutated_text: String,
    /// Name of the mutator that produced this mutation.
    pub mutator_name: String,
}

impl Mutant {
    /// Apply this mutation to the given source text, returning the mutated source.
    ///
    /// Splices [`mutated_text`](Self::mutated_text) into the source at the
    /// byte range `[byte_offset .. byte_offset + byte_length]`, preserving
    /// everything before and after the replaced region.
    #[inline]
    #[must_use]
    #[allow(
        clippy::indexing_slicing,
        reason = "byte offsets originate from AST and are always valid"
    )]
    #[allow(
        clippy::string_slice,
        reason = "byte offsets originate from AST and are always valid UTF-8 boundaries"
    )]
    pub fn apply_to_source(&self, source: &str) -> String {
        let mut result =
            String::with_capacity(source.len() - self.byte_length + self.mutated_text.len());
        result.push_str(&source[..self.byte_offset]);
        result.push_str(&self.mutated_text);
        result.push_str(&source[self.byte_offset + self.byte_length..]);
        result
    }
}

/// Outcome of running the test suite against a single mutant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum MutantStatus {
    /// The test suite detected the mutant (at least one test failed).
    Killed,
    /// The test suite passed despite the mutation.
    Survived,
    /// The test suite exceeded the configured timeout.
    Timeout,
    /// No tests cover the mutated code, so the mutant was not exercised.
    NoCoverage,
    /// An unexpected error occurred while testing this mutant.
    Error(String),
}

/// Full result of testing a single mutant.
///
/// Combines the [`Mutant`] descriptor with the [`MutantStatus`] outcome,
/// the list of tests that were executed, and the wall-clock duration.
#[derive(Debug, Clone, Serialize)]
pub struct MutantResult {
    /// The mutant that was tested.
    pub mutant: Mutant,
    /// Outcome of the test run.
    pub status: MutantStatus,
    /// Names of the tests that were executed against this mutant.
    pub tests_run: Vec<String>,
    /// Wall-clock duration of the test run.
    pub duration: Duration,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct a `Mutant` and verify all fields.
    #[test]
    fn mutant_construction() {
        let mutant = Mutant {
            file_path: PathBuf::from("src/app.py"),
            line: 10_u32,
            column: 5_u32,
            byte_offset: 120_usize,
            byte_length: 1_usize,
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            mutator_name: "arithmetic_op".to_owned(),
        };

        assert_eq!(mutant.file_path, PathBuf::from("src/app.py"));
        assert_eq!(mutant.line, 10_u32);
        assert_eq!(mutant.column, 5_u32);
        assert_eq!(mutant.byte_offset, 120_usize);
        assert_eq!(mutant.byte_length, 1_usize);
        assert_eq!(mutant.original_text, "+");
        assert_eq!(mutant.mutated_text, "-");
        assert_eq!(mutant.mutator_name, "arithmetic_op");
    }

    /// Clone a `Mutant` and verify equality.
    #[test]
    fn mutant_clone_equals_original() {
        let mutant = Mutant {
            file_path: PathBuf::from("lib/utils.py"),
            line: 1_u32,
            column: 1_u32,
            byte_offset: 0_usize,
            byte_length: 4_usize,
            original_text: "True".to_owned(),
            mutated_text: "False".to_owned(),
            mutator_name: "boolean_op".to_owned(),
        };

        let cloned = mutant.clone();
        assert_eq!(mutant, cloned);
    }

    /// All `MutantStatus` variants can be constructed and compared.
    #[test]
    fn mutant_status_variants() {
        assert_eq!(MutantStatus::Killed, MutantStatus::Killed);
        assert_eq!(MutantStatus::Survived, MutantStatus::Survived);
        assert_eq!(MutantStatus::Timeout, MutantStatus::Timeout);
        assert_eq!(MutantStatus::NoCoverage, MutantStatus::NoCoverage);
        assert_eq!(
            MutantStatus::Error("oops".to_owned()),
            MutantStatus::Error("oops".to_owned()),
        );
        assert_ne!(MutantStatus::Killed, MutantStatus::Survived);
        assert_ne!(
            MutantStatus::Error("a".to_owned()),
            MutantStatus::Error("b".to_owned()),
        );
    }

    /// Construct a `MutantResult` and verify its fields.
    #[test]
    fn mutant_result_construction() {
        let mutant = Mutant {
            file_path: PathBuf::from("src/calc.py"),
            line: 42_u32,
            column: 12_u32,
            byte_offset: 500_usize,
            byte_length: 2_usize,
            original_text: "==".to_owned(),
            mutated_text: "!=".to_owned(),
            mutator_name: "comparison_op".to_owned(),
        };

        let result = MutantResult {
            mutant: mutant.clone(),
            status: MutantStatus::Killed,
            tests_run: vec!["test_add".to_owned(), "test_sub".to_owned()],
            duration: Duration::from_millis(150_u64),
        };

        assert_eq!(result.mutant, mutant);
        assert_eq!(result.status, MutantStatus::Killed);
        assert_eq!(result.tests_run.len(), 2_usize);
        assert_eq!(result.duration, Duration::from_millis(150_u64));
    }

    /// `MutantResult` with an error status.
    #[test]
    fn mutant_result_error_status() {
        let result = MutantResult {
            mutant: Mutant {
                file_path: PathBuf::from("err.py"),
                line: 1_u32,
                column: 1_u32,
                byte_offset: 0_usize,
                byte_length: 3_usize,
                original_text: "and".to_owned(),
                mutated_text: "or".to_owned(),
                mutator_name: "boolean_op".to_owned(),
            },
            status: MutantStatus::Error("segfault".to_owned()),
            tests_run: Vec::new(),
            duration: Duration::from_secs(0_u64),
        };

        assert_eq!(result.status, MutantStatus::Error("segfault".to_owned()));
        assert!(result.tests_run.is_empty());
    }

    /// `apply_to_source` replaces a single operator in the middle.
    #[test]
    fn apply_to_source_middle_replacement() {
        let source = "x = a + b";
        let mutant = Mutant {
            file_path: PathBuf::from("test.py"),
            line: 1_u32,
            column: 7_u32,
            byte_offset: 6_usize,
            byte_length: 1_usize,
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            mutator_name: "arithmetic_op".to_owned(),
        };

        assert_eq!(mutant.apply_to_source(source), "x = a - b");
    }

    /// `apply_to_source` handles replacement at the start of the source.
    #[test]
    fn apply_to_source_at_start() {
        let source = "True and False";
        let mutant = Mutant {
            file_path: PathBuf::from("test.py"),
            line: 1_u32,
            column: 1_u32,
            byte_offset: 0_usize,
            byte_length: 4_usize,
            original_text: "True".to_owned(),
            mutated_text: "False".to_owned(),
            mutator_name: "constant_replace".to_owned(),
        };

        assert_eq!(mutant.apply_to_source(source), "False and False");
    }

    /// `apply_to_source` handles replacement at the end of the source.
    #[test]
    fn apply_to_source_at_end() {
        let source = "x = True";
        let mutant = Mutant {
            file_path: PathBuf::from("test.py"),
            line: 1_u32,
            column: 5_u32,
            byte_offset: 4_usize,
            byte_length: 4_usize,
            original_text: "True".to_owned(),
            mutated_text: "False".to_owned(),
            mutator_name: "constant_replace".to_owned(),
        };

        assert_eq!(mutant.apply_to_source(source), "x = False");
    }

    /// `apply_to_source` handles replacement with different-length text.
    #[test]
    fn apply_to_source_different_length() {
        let source = "x = a // b";
        let mutant = Mutant {
            file_path: PathBuf::from("test.py"),
            line: 1_u32,
            column: 7_u32,
            byte_offset: 6_usize,
            byte_length: 2_usize,
            original_text: "//".to_owned(),
            mutated_text: "*".to_owned(),
            mutator_name: "arithmetic_op".to_owned(),
        };

        assert_eq!(mutant.apply_to_source(source), "x = a * b");
    }

    /// `apply_to_source` preserves surrounding multiline code.
    #[test]
    fn apply_to_source_preserves_multiline() {
        let source = "def calc():\n    return a + b\n    # done\n";
        // The `+` is at byte offset 25 ("def calc():\n    return a " = 25 bytes)
        let mutant = Mutant {
            file_path: PathBuf::from("test.py"),
            line: 2_u32,
            column: 14_u32,
            byte_offset: 25_usize,
            byte_length: 1_usize,
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            mutator_name: "arithmetic_op".to_owned(),
        };

        let result = mutant.apply_to_source(source);
        assert_eq!(result, "def calc():\n    return a - b\n    # done\n");
    }
}
