//! Mutation engine — AST rewriting and mutant generation.
//!
//! This module contains the core mutation logic: walking a Python AST,
//! applying mutation operators, and producing [`Mutant`] descriptors.

use std::path::{Path, PathBuf};

use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};

use crate::Error;

/// Built-in mutation operators shipped with fest.
pub mod builtin;
/// Data types for representing mutants and their execution results.
pub mod mutant;
/// Mutator trait and registry for mutation operators.
pub mod mutator;

pub use mutant::{Mutant, MutantResult, MutantStatus};
pub use mutator::{Mutation, Mutator, MutatorRegistry};

/// Discover Python source files matching `source_patterns`, excluding any
/// files that match `exclude_patterns`.
///
/// All glob patterns are resolved relative to `base_dir`.
///
/// # Errors
///
/// Returns [`Error::Mutation`] if any glob pattern is invalid.
fn discover_files(
    source_patterns: &[String],
    exclude_patterns: &[String],
    base_dir: &Path,
) -> Result<Vec<PathBuf>, Error> {
    let exclude_compiled: Vec<glob::Pattern> = exclude_patterns
        .iter()
        .map(|pat| {
            let full_pattern = base_dir.join(pat).display().to_string();
            glob::Pattern::new(&full_pattern)
                .map_err(|err| Error::Mutation(format!("invalid exclude pattern '{pat}': {err}")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut files = Vec::new();

    for pattern in source_patterns {
        let full_pattern = base_dir.join(pattern).display().to_string();
        let entries = glob::glob(&full_pattern)
            .map_err(|err| Error::Mutation(format!("invalid source pattern '{pattern}': {err}")))?;

        for entry in entries {
            let path =
                entry.map_err(|err| Error::Mutation(format!("error reading glob entry: {err}")))?;
            let excluded = exclude_compiled
                .iter()
                .any(|exclude| exclude.matches_path(&path));
            if !excluded {
                files.push(path);
            }
        }
    }

    files.sort();
    files.dedup();
    Ok(files)
}

/// Compute 1-based line and column numbers from a byte offset into `source`.
///
/// Iterates through the source bytes, counting newlines. Returns
/// `(line, column)` where both are 1-based.
fn line_column_from_offset(source: &str, byte_offset: usize) -> (u32, u32) {
    let mut line = 1_u32;
    let mut last_newline_offset = 0_usize;
    let mut found_any_newline = false;

    for (idx, byte) in source.bytes().enumerate() {
        if idx >= byte_offset {
            break;
        }
        if byte == b'\n' {
            line += 1_u32;
            last_newline_offset = idx + 1_usize;
            found_any_newline = true;
        }
    }

    let column = if found_any_newline {
        byte_offset - last_newline_offset + 1_usize
    } else {
        byte_offset + 1_usize
    };

    #[allow(
        clippy::cast_possible_truncation,
        reason = "column offset in a source file line always fits in u32"
    )]
    let column_u32 = column as u32;

    (line, column_u32)
}

/// Generate mutants for a single file.
///
/// Reads the source, parses the AST, and runs each mutator to produce
/// [`Mutant`] descriptors.
fn generate_mutants_for_file(
    path: &Path,
    registry: &MutatorRegistry,
) -> Result<Vec<Mutant>, Error> {
    let source = std::fs::read_to_string(path)
        .map_err(|err| Error::Mutation(format!("failed to read {}: {err}", path.display())))?;

    let parsed = ruff_python_parser::parse_module(&source)
        .map_err(|err| Error::Mutation(format!("failed to parse {}: {err}", path.display())))?;
    let ast = parsed.into_syntax();

    let mut mutants = Vec::new();

    for mutator in registry.iter() {
        let mutations = mutator.find_mutations(&source, &ast);

        for mutation in mutations {
            let (line, column) = line_column_from_offset(&source, mutation.byte_offset);
            mutants.push(Mutant {
                file_path: path.to_path_buf(),
                line,
                column,
                byte_offset: mutation.byte_offset,
                byte_length: mutation.byte_length,
                original_text: mutation.original_text,
                mutated_text: mutation.replacement_text,
                mutator_name: mutator.name().to_owned(),
            });
        }
    }

    Ok(mutants)
}

/// Discover Python source files and generate all mutants in parallel.
///
/// 1. Resolves `source_patterns` against `base_dir` to find `.py` files, filtering out any that
///    match `exclude_patterns`.
/// 2. For each file, reads the source, parses the AST with `ruff_python_parser`, and runs every
///    mutator in the `registry`.
/// 3. Each [`Mutation`] is converted into a [`Mutant`] with the file path, line/column numbers, and
///    byte offset information.
///
/// File processing is parallelized across available cores using
/// [rayon](https://docs.rs/rayon).
///
/// # Errors
///
/// Returns [`Error::Mutation`] if glob patterns are invalid, a file cannot
/// be read, or a Python source file fails to parse.
#[inline]
pub fn generate_mutants(
    source_patterns: &[String],
    exclude_patterns: &[String],
    base_dir: &Path,
    registry: &MutatorRegistry,
) -> Result<Vec<Mutant>, Error> {
    let files = discover_files(source_patterns, exclude_patterns, base_dir)?;

    let results: Result<Vec<Vec<Mutant>>, Error> = files
        .par_iter()
        .map(|path| generate_mutants_for_file(path, registry))
        .collect();

    let nested = results?;
    let mut all_mutants: Vec<Mutant> = nested.into_iter().flatten().collect();
    all_mutants.sort_by(|lhs, rhs| {
        lhs.file_path
            .cmp(&rhs.file_path)
            .then(lhs.byte_offset.cmp(&rhs.byte_offset))
    });

    Ok(all_mutants)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    // -- File discovery tests -----------------------------------------------

    /// Discover `.py` files from a simple glob pattern.
    #[test]
    fn discover_files_finds_py_files() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("create src dir");

        let py_file = src.join("app.py");
        drop(std::fs::File::create(&py_file).expect("create app.py"));

        let txt_file = src.join("notes.txt");
        drop(std::fs::File::create(&txt_file).expect("create notes.txt"));

        let patterns = vec!["src/**/*.py".to_owned()];
        let excludes: Vec<String> = Vec::new();
        let files =
            discover_files(&patterns, &excludes, dir.path()).expect("should discover files");

        assert_eq!(files.len(), 1_usize);
        assert_eq!(files[0_usize], py_file);
    }

    /// Exclude patterns filter out matching files.
    #[test]
    fn discover_files_excludes_patterns() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let src = dir.path().join("src");
        let generated = src.join("generated");
        std::fs::create_dir_all(&generated).expect("create dirs");

        let keep = src.join("main.py");
        drop(std::fs::File::create(&keep).expect("create main.py"));

        let skip = generated.join("auto.py");
        drop(std::fs::File::create(&skip).expect("create auto.py"));

        let patterns = vec!["src/**/*.py".to_owned()];
        let excludes = vec!["src/generated/**/*.py".to_owned()];
        let files =
            discover_files(&patterns, &excludes, dir.path()).expect("should discover files");

        assert_eq!(files.len(), 1_usize);
        assert_eq!(files[0_usize], keep);
    }

    /// An empty source pattern list yields no files.
    #[test]
    fn discover_files_empty_patterns() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let patterns: Vec<String> = Vec::new();
        let excludes: Vec<String> = Vec::new();
        let files =
            discover_files(&patterns, &excludes, dir.path()).expect("should discover files");

        assert!(files.is_empty());
    }

    /// Invalid glob pattern returns an error.
    #[test]
    fn discover_files_invalid_pattern() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let patterns = vec!["[invalid".to_owned()];
        let excludes: Vec<String> = Vec::new();
        let result = discover_files(&patterns, &excludes, dir.path());

        assert!(result.is_err());
    }

    // -- Line/column computation tests --------------------------------------

    /// Line and column are both 1-based at the start of the source.
    #[test]
    fn line_column_at_start() {
        let (line, col) = line_column_from_offset("hello", 0_usize);
        assert_eq!(line, 1_u32);
        assert_eq!(col, 1_u32);
    }

    /// Offset in the middle of the first line.
    #[test]
    fn line_column_first_line_middle() {
        let (line, col) = line_column_from_offset("x = a + b", 6_usize);
        assert_eq!(line, 1_u32);
        assert_eq!(col, 7_u32);
    }

    /// Offset on the second line.
    #[test]
    fn line_column_second_line() {
        // "line1\nline2"
        // Byte 6 is 'l' in "line2"
        let (line, col) = line_column_from_offset("line1\nline2", 6_usize);
        assert_eq!(line, 2_u32);
        assert_eq!(col, 1_u32);
    }

    /// Offset on the third line, not at column 1.
    #[test]
    fn line_column_third_line_offset() {
        // "a\nb\ncde"
        // Byte 5 is 'd'
        let (line, col) = line_column_from_offset("a\nb\ncde", 5_usize);
        assert_eq!(line, 3_u32);
        assert_eq!(col, 2_u32);
    }

    // -- Mutant generation (integration) tests ------------------------------

    /// Generate mutants for a file with arithmetic operators.
    #[test]
    fn generate_mutants_for_arithmetic() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let py_file = dir.path().join("calc.py");
        {
            let mut file = std::fs::File::create(&py_file).expect("create file");
            file.write_all(b"x = a + b\n").expect("write file");
        }

        let mut registry = MutatorRegistry::new();
        registry.register(Box::new(builtin::arithmetic::ArithmeticOp));

        let mutants =
            generate_mutants_for_file(&py_file, &registry).expect("should generate mutants");

        assert_eq!(mutants.len(), 1_usize);
        assert_eq!(mutants[0_usize].original_text, "+");
        assert_eq!(mutants[0_usize].mutated_text, "-");
        assert_eq!(mutants[0_usize].file_path, py_file);
        assert_eq!(mutants[0_usize].line, 1_u32);
        assert_eq!(mutants[0_usize].mutator_name, "arithmetic_op");
    }

    /// The top-level `generate_mutants` function discovers files and produces
    /// mutants in parallel.
    #[test]
    fn generate_mutants_end_to_end() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("create src dir");

        let py_a = src.join("a.py");
        {
            let mut file = std::fs::File::create(&py_a).expect("create file");
            file.write_all(b"x = 1 + 2\n").expect("write file");
        }

        let py_b = src.join("b.py");
        {
            let mut file = std::fs::File::create(&py_b).expect("create file");
            file.write_all(b"y = 3 - 4\n").expect("write file");
        }

        let mut registry = MutatorRegistry::new();
        registry.register(Box::new(builtin::arithmetic::ArithmeticOp));

        let patterns = vec!["src/**/*.py".to_owned()];
        let excludes: Vec<String> = Vec::new();
        let mutants = generate_mutants(&patterns, &excludes, dir.path(), &registry)
            .expect("should generate mutants");

        assert_eq!(mutants.len(), 2_usize);
        // Sorted by file path then byte offset
        assert_eq!(mutants[0_usize].file_path, py_a);
        assert_eq!(mutants[1_usize].file_path, py_b);
    }

    /// Files that fail to parse return an error.
    #[test]
    fn generate_mutants_parse_error() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let py_file = dir.path().join("bad.py");
        {
            let mut file = std::fs::File::create(&py_file).expect("create file");
            file.write_all(b"def ((\n").expect("write file");
        }

        let registry = MutatorRegistry::new();
        let result = generate_mutants_for_file(&py_file, &registry);

        assert!(result.is_err());
    }

    /// An empty registry produces no mutants.
    #[test]
    fn generate_mutants_empty_registry() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let py_file = dir.path().join("empty.py");
        {
            let mut file = std::fs::File::create(&py_file).expect("create file");
            file.write_all(b"x = 1 + 2\n").expect("write file");
        }

        let registry = MutatorRegistry::new();
        let mutants =
            generate_mutants_for_file(&py_file, &registry).expect("should generate mutants");

        assert!(mutants.is_empty());
    }

    /// Mutants from multiple mutators are collected together.
    #[test]
    fn generate_mutants_multiple_mutators() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let py_file = dir.path().join("multi.py");
        {
            let mut file = std::fs::File::create(&py_file).expect("create file");
            // Contains both arithmetic and comparison operators
            file.write_all(b"result = (a + b) == c\n")
                .expect("write file");
        }

        let mut registry = MutatorRegistry::new();
        registry.register(Box::new(builtin::arithmetic::ArithmeticOp));
        registry.register(Box::new(builtin::comparison::ComparisonOp));

        let mutants =
            generate_mutants_for_file(&py_file, &registry).expect("should generate mutants");

        // At least 1 arithmetic + 1 comparison mutation
        assert!(mutants.len() >= 2_usize);

        let mutator_names: Vec<&str> = mutants.iter().map(|m| m.mutator_name.as_str()).collect();
        assert!(mutator_names.contains(&"arithmetic_op"));
        assert!(mutator_names.contains(&"comparison_op"));
    }

    /// Excluded files are skipped in end-to-end generation.
    #[test]
    fn generate_mutants_with_excludes() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let src = dir.path().join("src");
        let generated = src.join("gen");
        std::fs::create_dir_all(&generated).expect("create dirs");

        let keep = src.join("keep.py");
        {
            let mut file = std::fs::File::create(&keep).expect("create file");
            file.write_all(b"x = 1 + 2\n").expect("write file");
        }

        let skip = generated.join("skip.py");
        {
            let mut file = std::fs::File::create(&skip).expect("create file");
            file.write_all(b"y = 3 + 4\n").expect("write file");
        }

        let mut registry = MutatorRegistry::new();
        registry.register(Box::new(builtin::arithmetic::ArithmeticOp));

        let patterns = vec!["src/**/*.py".to_owned()];
        let excludes = vec!["src/gen/**/*.py".to_owned()];
        let mutants = generate_mutants(&patterns, &excludes, dir.path(), &registry)
            .expect("should generate mutants");

        assert_eq!(mutants.len(), 1_usize);
        assert_eq!(mutants[0_usize].file_path, keep);
    }
}
