//! Mutation engine — AST rewriting and mutant generation.
//!
//! This module contains the core mutation logic: walking a Python AST,
//! applying mutation operators, and producing [`Mutant`] descriptors.

use std::path::{Path, PathBuf};

use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};

use crate::{
    Error,
    config::{MutatorConfig, PerFileConfig},
};

/// Built-in mutation operators shipped with fest.
pub mod builtin;

/// Structured diff IR for mutations dispatched to the plugin backend.
pub mod diff;

/// Data types for representing mutants and their execution results.
pub mod mutant;
/// Mutator trait and registry for mutation operators.
pub mod mutator;
/// Deterministic per-mutation value derivation from a seed.
pub(crate) mod seed;

pub use diff::MutationDiff;
pub use mutant::{Mutant, MutantResult, MutantStatus};
pub use mutator::{Mutation, MutationContext, Mutator, MutatorRegistry};

/// Options for mutant generation that control filtering and per-file overrides.
#[derive(Debug)]
pub struct GenerationOptions<'opts> {
    /// RNG seed for deterministic mutant generation.
    pub seed: Option<u64>,
    /// Operator filter patterns.
    pub filter_operators: &'opts [String],
    /// Path filter patterns.
    pub filter_paths: &'opts [String],
    /// Per-file configuration overrides.
    pub per_file: &'opts [PerFileConfig],
    /// Global mutator config for merging with per-file overrides.
    pub global_mutators: &'opts MutatorConfig,
}

/// Build a [`MutatorRegistry`] from a [`MutatorConfig`].
///
/// Reads each boolean flag in the configuration and registers the
/// corresponding built-in mutator when the flag is `true`.
///
/// The mapping is:
/// - `arithmetic_op` -> [`builtin::arithmetic::ArithmeticOp`]
/// - `comparison_op` -> [`builtin::comparison::ComparisonOp`]
/// - `boolean_op` -> [`builtin::boolean::BooleanOp`]
/// - `return_value` -> [`builtin::return_value::ReturnValue`]
/// - `negate_condition` -> [`builtin::negate_condition::NegateCondition`]
/// - `remove_decorator` -> [`builtin::remove_decorator::RemoveDecorator`]
/// - `constant_replace` -> [`builtin::constant::ConstantReplace`]
/// - `exception_swallow` -> [`builtin::exception::ExceptionSwallow`]
#[inline]
#[must_use]
pub fn build_registry(config: &MutatorConfig) -> MutatorRegistry {
    let mut registry = MutatorRegistry::new();

    if config.arithmetic_op {
        registry.register(Box::new(builtin::arithmetic::ArithmeticOp));
    }
    if config.comparison_op {
        registry.register(Box::new(builtin::comparison::ComparisonOp));
    }
    if config.boolean_op {
        registry.register(Box::new(builtin::boolean::BooleanOp));
    }
    if config.return_value {
        registry.register(Box::new(builtin::return_value::ReturnValue));
    }
    if config.negate_condition {
        registry.register(Box::new(builtin::negate_condition::NegateCondition));
    }
    if config.remove_decorator {
        registry.register(Box::new(builtin::remove_decorator::RemoveDecorator));
    }
    if config.constant_replace {
        registry.register(Box::new(builtin::constant::ConstantReplace));
    }
    if config.exception_swallow {
        registry.register(Box::new(builtin::exception::ExceptionSwallow));
    }
    if config.break_continue {
        registry.register(Box::new(builtin::break_continue::BreakContinue));
    }
    if config.unary_op {
        registry.register(Box::new(builtin::unary::UnaryOp));
    }
    if config.zero_iteration_loop {
        registry.register(Box::new(builtin::zero_iteration::ZeroIterationLoop));
    }
    if config.augmented_assign {
        registry.register(Box::new(builtin::augmented_assign::AugmentedAssign));
    }
    if config.statement_deletion {
        registry.register(Box::new(builtin::statement_deletion::StatementDeletion));
    }
    if config.bitwise_op {
        registry.register(Box::new(builtin::bitwise::BitwiseOp));
    }
    if config.remove_super_call {
        registry.register(Box::new(builtin::remove_super::RemoveSuperCall));
    }
    if config.variable_replace {
        registry.register(Box::new(builtin::variable_replace::VariableReplace));
    }
    if config.variable_insert {
        registry.register(Box::new(builtin::variable_insert::VariableInsert));
    }

    registry
}

/// Discover Python source files matching `source_patterns`, excluding any
/// files that match `exclude_patterns`.
///
/// All glob patterns are resolved relative to `base_dir`.
///
/// # Errors
///
/// Returns [`Error::Mutation`] if any glob pattern is invalid.
pub(crate) fn discover_files(
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

/// Check whether a mutation at `byte_offset` in `source` is suppressed by a
/// pragma comment on the same line.
///
/// Supports two forms:
/// - `# pragma: no mutate` — suppresses ALL mutations on that line.
/// - `# pragma: no mutate(operator_name)` — suppresses only the named operator.
fn is_suppressed_by_pragma(source: &str, byte_offset: usize, mutator_name: &str) -> bool {
    // Find the line containing byte_offset using rfind/find on str slices.
    #[allow(
        clippy::string_slice,
        reason = "byte_offset is a valid UTF-8 boundary from AST byte offsets"
    )]
    let before = &source[..byte_offset];
    let line_start = before.rfind('\n').map_or(0_usize, |pos| pos + 1_usize);
    #[allow(
        clippy::string_slice,
        reason = "byte_offset is a valid UTF-8 boundary from AST byte offsets"
    )]
    let after = &source[byte_offset..];
    let line_end = after
        .find('\n')
        .map_or(source.len(), |pos| byte_offset + pos);

    #[allow(
        clippy::string_slice,
        reason = "line_start..line_end are valid UTF-8 boundaries from newline search"
    )]
    let line = &source[line_start..line_end];

    let Some(pragma_pos) = line.find("# pragma: no mutate") else {
        return false;
    };

    #[allow(
        clippy::string_slice,
        reason = "pragma_pos + len is within line bounds since find() returned it"
    )]
    let after_pragma = &line[pragma_pos + "# pragma: no mutate".len()..];
    // Check for `(operator_name)` suffix.
    if after_pragma.starts_with('(') {
        if let Some(close) = after_pragma.find(')') {
            #[allow(
                clippy::string_slice,
                reason = "close is a valid index within after_pragma from find()"
            )]
            let operator = &after_pragma[1_usize..close];
            return operator == mutator_name;
        }
        // Malformed pragma with `(` but no `)` — treat as no suppression.
        return false;
    }
    // No parenthesized operator — suppresses all mutations.
    true
}

/// Check whether a mutator name passes the operator filter.
///
/// - If `filters` is empty, returns `true` (no filtering).
/// - Patterns prefixed with `!` are exclusion filters (substring match).
/// - Patterns without `!` are inclusion filters (substring match).
/// - If any exclusion pattern matches, returns `false`.
/// - If there are inclusion patterns and none match, returns `false`.
/// - Otherwise returns `true`.
fn matches_operator_filter(mutator_name: &str, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }

    let mut has_includes = false;
    let mut any_include_matched = false;

    for filter in filters {
        if let Some(pattern) = filter.strip_prefix('!') {
            // Exclusion filter.
            if mutator_name.contains(pattern) {
                return false;
            }
        } else {
            // Inclusion filter.
            has_includes = true;
            if mutator_name.contains(filter.as_str()) {
                any_include_matched = true;
            }
        }
    }

    if has_includes {
        return any_include_matched;
    }

    true
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
/// [`Mutant`] descriptors. Mutators that do not match `filter_operators`
/// are skipped, and individual mutations suppressed by pragma comments
/// are excluded.
fn generate_mutants_for_file(
    path: &Path,
    registry: &MutatorRegistry,
    seed: Option<u64>,
    filter_operators: &[String],
) -> Result<Vec<Mutant>, Error> {
    let source = std::fs::read_to_string(path)
        .map_err(|err| Error::Mutation(format!("failed to read {}: {err}", path.display())))?;

    let parsed = ruff_python_parser::parse_module(&source)
        .map_err(|err| Error::Mutation(format!("failed to parse {}: {err}", path.display())))?;
    let ast = parsed.into_syntax();

    let ctx = MutationContext {
        file_path: path,
        seed,
    };

    let mut mutants = Vec::new();

    for mutator in registry.iter() {
        let name = mutator.name();

        // Skip mutators that do not match the operator filter.
        if !matches_operator_filter(name, filter_operators) {
            continue;
        }

        let mutations = mutator.find_mutations(&source, &ast, &ctx);

        for mutation in mutations {
            // Skip mutations suppressed by pragma comments.
            if is_suppressed_by_pragma(&source, mutation.byte_offset, name) {
                continue;
            }

            let (line, column) = line_column_from_offset(&source, mutation.byte_offset);
            mutants.push(Mutant {
                file_path: path.to_path_buf(),
                line,
                column,
                byte_offset: mutation.byte_offset,
                byte_length: mutation.byte_length,
                original_text: mutation.original_text,
                mutated_text: mutation.replacement_text,
                mutator_name: name.to_owned(),
            });
        }
    }

    Ok(mutants)
}

/// Generate mutants for a pre-discovered set of files in parallel.
///
/// For each file in `files`, reads the source, parses the AST with
/// `ruff_python_parser`, and runs every mutator in the `registry`.
/// Each [`Mutation`] is converted into a [`Mutant`] with the file path,
/// line/column numbers, and byte offset information.
///
/// When `filter_paths` is non-empty, only files whose path matches at
/// least one glob pattern are processed. When `filter_operators` is
/// non-empty, only matching operators are applied.
///
/// File processing is parallelized across available cores using
/// [rayon](https://docs.rs/rayon).
///
/// # Errors
///
/// Returns [`Error::Mutation`] if a file cannot be read, a Python
/// source file fails to parse, or a `filter_paths` glob is invalid.
#[inline]
pub fn generate_mutants_for_files(
    files: &[PathBuf],
    registry: &MutatorRegistry,
    opts: &GenerationOptions<'_>,
) -> Result<Vec<Mutant>, Error> {
    let filtered_files = filter_files_by_path(files, opts.filter_paths)?;

    // Pre-compile per-file glob patterns.
    let compiled_per_file = compile_per_file_patterns(opts.per_file)?;

    let results: Result<Vec<Vec<Mutant>>, Error> = filtered_files
        .par_iter()
        .map(|path| {
            // Check for per-file overrides.
            if let Some(pf) = find_per_file_config(path, &compiled_per_file) {
                if pf.skip {
                    return Ok(Vec::new());
                }
                if let Some(overrides) = pf.mutators.as_ref() {
                    let merged = opts.global_mutators.with_overrides(overrides);
                    let per_file_reg = build_registry(&merged);
                    return generate_mutants_for_file(
                        path,
                        &per_file_reg,
                        opts.seed,
                        opts.filter_operators,
                    );
                }
            }
            generate_mutants_for_file(path, registry, opts.seed, opts.filter_operators)
        })
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

/// Filter a file list by glob patterns.
///
/// If `filter_paths` is empty, returns all files unchanged. Otherwise,
/// only files matching at least one pattern are kept.
///
/// # Errors
///
/// Returns [`Error::Mutation`] if a glob pattern is invalid.
fn filter_files_by_path<'fl>(
    files: &'fl [PathBuf],
    filter_paths: &[String],
) -> Result<Vec<&'fl PathBuf>, Error> {
    if filter_paths.is_empty() {
        return Ok(files.iter().collect());
    }

    let compiled: Vec<glob::Pattern> = filter_paths
        .iter()
        .map(|pat| {
            glob::Pattern::new(pat).map_err(|err| {
                Error::Mutation(format!("invalid filter_paths pattern '{pat}': {err}"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let matched = files
        .iter()
        .filter(|path| compiled.iter().any(|pat| pat.matches_path(path)))
        .collect();

    Ok(matched)
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
    opts: &GenerationOptions<'_>,
) -> Result<Vec<Mutant>, Error> {
    let files = discover_files(source_patterns, exclude_patterns, base_dir)?;
    generate_mutants_for_files(&files, registry, opts)
}

/// A compiled per-file configuration with pre-compiled glob pattern.
struct CompiledPerFile<'pf> {
    /// Compiled glob pattern.
    pattern: glob::Pattern,
    /// Reference to the original per-file config.
    config: &'pf PerFileConfig,
}

/// Pre-compile all per-file glob patterns.
///
/// # Errors
///
/// Returns [`Error::Mutation`] if a glob pattern is invalid.
fn compile_per_file_patterns(
    per_file: &[PerFileConfig],
) -> Result<Vec<CompiledPerFile<'_>>, Error> {
    per_file
        .iter()
        .map(|pf| {
            let pattern = glob::Pattern::new(&pf.pattern).map_err(|err| {
                Error::Mutation(format!("invalid per-file pattern '{}': {err}", pf.pattern))
            })?;
            Ok(CompiledPerFile {
                pattern,
                config: pf,
            })
        })
        .collect()
}

/// Find the last matching per-file config for a path.
///
/// Later entries override earlier ones ("last match wins").
fn find_per_file_config<'pf>(
    path: &Path,
    compiled: &'pf [CompiledPerFile<'pf>],
) -> Option<&'pf PerFileConfig> {
    compiled
        .iter()
        .rev()
        .find(|cpf| cpf.pattern.matches_path(path))
        .map(|cpf| cpf.config)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use crate::config::MutatorOverrides;

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

        let mutants = generate_mutants_for_file(&py_file, &registry, None, &[])
            .expect("should generate mutants");

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
        let mutants = generate_mutants(
            &patterns,
            &excludes,
            dir.path(),
            &registry,
            &GenerationOptions {
                seed: None,
                filter_operators: &[],
                filter_paths: &[],
                per_file: &[],
                global_mutators: &MutatorConfig::default(),
            },
        )
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
        let result = generate_mutants_for_file(&py_file, &registry, None, &[]);

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
        let mutants = generate_mutants_for_file(&py_file, &registry, None, &[])
            .expect("should generate mutants");

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

        let mutants = generate_mutants_for_file(&py_file, &registry, None, &[])
            .expect("should generate mutants");

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
        let mutants = generate_mutants(
            &patterns,
            &excludes,
            dir.path(),
            &registry,
            &GenerationOptions {
                seed: None,
                filter_operators: &[],
                filter_paths: &[],
                per_file: &[],
                global_mutators: &MutatorConfig::default(),
            },
        )
        .expect("should generate mutants");

        assert_eq!(mutants.len(), 1_usize);
        assert_eq!(mutants[0_usize].file_path, keep);
    }

    // -- build_registry tests -----------------------------------------------

    /// Default `MutatorConfig` registers all 8 built-in mutators.
    #[test]
    fn build_registry_default_config_registers_all() {
        let config = MutatorConfig::default();
        let registry = build_registry(&config);
        assert_eq!(registry.len(), 14_usize);
    }

    /// Disabling all flags produces an empty registry.
    #[test]
    fn build_registry_all_disabled_is_empty() {
        let config = MutatorConfig {
            arithmetic_op: false,
            comparison_op: false,
            boolean_op: false,
            return_value: false,
            negate_condition: false,
            remove_decorator: false,
            constant_replace: false,
            exception_swallow: false,
            break_continue: false,
            unary_op: false,
            zero_iteration_loop: false,
            augmented_assign: false,
            statement_deletion: false,
            bitwise_op: false,
            remove_super_call: false,
            variable_replace: false,
            variable_insert: false,
            custom: Vec::new(),
            python: Vec::new(),
            dylib: Vec::new(),
        };
        let registry = build_registry(&config);
        assert!(registry.is_empty());
    }

    /// Enabling only `arithmetic_op` registers exactly one mutator with
    /// the correct name.
    #[test]
    fn build_registry_single_flag_arithmetic() {
        let config = MutatorConfig {
            arithmetic_op: true,
            comparison_op: false,
            boolean_op: false,
            return_value: false,
            negate_condition: false,
            remove_decorator: false,
            constant_replace: false,
            exception_swallow: false,
            break_continue: false,
            unary_op: false,
            zero_iteration_loop: false,
            augmented_assign: false,
            statement_deletion: false,
            bitwise_op: false,
            remove_super_call: false,
            variable_replace: false,
            variable_insert: false,
            custom: Vec::new(),
            python: Vec::new(),
            dylib: Vec::new(),
        };
        let registry = build_registry(&config);
        assert_eq!(registry.len(), 1_usize);
        let names: Vec<&str> = registry.iter().map(Mutator::name).collect();
        assert_eq!(names, vec!["arithmetic_op"]);
    }

    /// Enabling only `comparison_op` registers exactly one mutator.
    #[test]
    fn build_registry_single_flag_comparison() {
        let config = MutatorConfig {
            arithmetic_op: false,
            comparison_op: true,
            boolean_op: false,
            return_value: false,
            negate_condition: false,
            remove_decorator: false,
            constant_replace: false,
            exception_swallow: false,
            break_continue: false,
            unary_op: false,
            zero_iteration_loop: false,
            augmented_assign: false,
            statement_deletion: false,
            bitwise_op: false,
            remove_super_call: false,
            variable_replace: false,
            variable_insert: false,
            custom: Vec::new(),
            python: Vec::new(),
            dylib: Vec::new(),
        };
        let registry = build_registry(&config);
        assert_eq!(registry.len(), 1_usize);
        let names: Vec<&str> = registry.iter().map(Mutator::name).collect();
        assert_eq!(names, vec!["comparison_op"]);
    }

    /// Enabling a subset of flags registers only the matching mutators.
    #[test]
    fn build_registry_partial_flags() {
        let config = MutatorConfig {
            arithmetic_op: true,
            comparison_op: false,
            boolean_op: true,
            return_value: false,
            negate_condition: true,
            remove_decorator: false,
            constant_replace: false,
            exception_swallow: true,
            break_continue: false,
            unary_op: false,
            zero_iteration_loop: false,
            augmented_assign: false,
            statement_deletion: false,
            bitwise_op: false,
            remove_super_call: false,
            variable_replace: false,
            variable_insert: false,
            custom: Vec::new(),
            python: Vec::new(),
            dylib: Vec::new(),
        };
        let registry = build_registry(&config);
        assert_eq!(registry.len(), 4_usize);

        let names: Vec<&str> = registry.iter().map(Mutator::name).collect();
        assert!(names.contains(&"arithmetic_op"));
        assert!(names.contains(&"boolean_op"));
        assert!(names.contains(&"negate_condition"));
        assert!(names.contains(&"exception_swallow"));
        assert!(!names.contains(&"comparison_op"));
        assert!(!names.contains(&"return_value"));
        assert!(!names.contains(&"remove_decorator"));
        assert!(!names.contains(&"constant_replace"));
    }

    /// Each flag maps to the correct mutator name.
    #[test]
    fn build_registry_flag_name_mapping() {
        /// Expected mapping from config field name to mutator name.
        const EXPECTED_NAMES: [&str; 14] = [
            "arithmetic_op",
            "comparison_op",
            "boolean_op",
            "return_value",
            "negate_condition",
            "remove_decorator",
            "constant_replace",
            "exception_swallow",
            "break_continue",
            "unary_op",
            "zero_iteration_loop",
            "augmented_assign",
            "bitwise_op",
            "remove_super_call",
        ];

        let config = MutatorConfig::default();
        let registry = build_registry(&config);
        let names: Vec<&str> = registry.iter().map(Mutator::name).collect();

        for expected in EXPECTED_NAMES {
            assert!(
                names.contains(&expected),
                "expected mutator '{expected}' not found in registry",
            );
        }
    }

    // -- Pragma suppression tests -------------------------------------------

    /// Global pragma suppresses all mutations on the line.
    #[test]
    fn pragma_suppresses_all_mutations() {
        let source = "x = a + b  # pragma: no mutate\n";
        assert!(is_suppressed_by_pragma(source, 6_usize, "arithmetic_op"));
        assert!(is_suppressed_by_pragma(source, 6_usize, "comparison_op"));
    }

    /// Per-operator pragma suppresses only the named operator.
    #[test]
    fn pragma_suppresses_specific_operator() {
        let source = "x = a + b  # pragma: no mutate(arithmetic_op)\n";
        assert!(is_suppressed_by_pragma(source, 6_usize, "arithmetic_op"));
        assert!(!is_suppressed_by_pragma(source, 6_usize, "comparison_op"));
    }

    /// Lines without pragma are not suppressed.
    #[test]
    fn pragma_absent_no_suppression() {
        let source = "x = a + b\n";
        assert!(!is_suppressed_by_pragma(source, 6_usize, "arithmetic_op"));
    }

    /// Pragma on a different line does not suppress.
    #[test]
    fn pragma_on_different_line_no_suppression() {
        let source = "x = a + b\ny = 1  # pragma: no mutate\n";
        assert!(!is_suppressed_by_pragma(source, 6_usize, "arithmetic_op"));
    }

    /// Pragma integration: mutants on suppressed lines are excluded.
    #[test]
    fn pragma_integration_filters_mutants() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let py_file = dir.path().join("suppressed.py");
        {
            let mut file = std::fs::File::create(&py_file).expect("create file");
            file.write_all(b"x = a + b  # pragma: no mutate\ny = c + d\n")
                .expect("write file");
        }

        let mut registry = MutatorRegistry::new();
        registry.register(Box::new(builtin::arithmetic::ArithmeticOp));

        let mutants = generate_mutants_for_file(&py_file, &registry, None, &[])
            .expect("should generate mutants");

        // Only the second line should produce a mutant.
        assert_eq!(mutants.len(), 1_usize);
        assert_eq!(mutants[0_usize].line, 2_u32);
    }

    // -- Operator filter tests ----------------------------------------------

    /// Empty filters match everything.
    #[test]
    fn operator_filter_empty_matches_all() {
        assert!(matches_operator_filter("arithmetic_op", &[]));
    }

    /// Include filter matches substring.
    #[test]
    fn operator_filter_include_match() {
        let filters = vec!["arithmetic".to_owned()];
        assert!(matches_operator_filter("arithmetic_op", &filters));
        assert!(!matches_operator_filter("comparison_op", &filters));
    }

    /// Exclude filter rejects substring match.
    #[test]
    fn operator_filter_exclude_match() {
        let filters = vec!["!arithmetic".to_owned()];
        assert!(!matches_operator_filter("arithmetic_op", &filters));
        assert!(matches_operator_filter("comparison_op", &filters));
    }

    /// Mixed include and exclude filters work together.
    #[test]
    fn operator_filter_mixed() {
        let filters = vec!["_op".to_owned(), "!arithmetic".to_owned()];
        // "arithmetic_op" matches include "_op" but is excluded by "!arithmetic".
        assert!(!matches_operator_filter("arithmetic_op", &filters));
        // "comparison_op" matches include "_op" and is not excluded.
        assert!(matches_operator_filter("comparison_op", &filters));
        // "return_value" does not match any include pattern.
        assert!(!matches_operator_filter("return_value", &filters));
    }

    /// Operator filter integration: filtered operators are skipped.
    #[test]
    fn operator_filter_integration() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let py_file = dir.path().join("filtered.py");
        {
            let mut file = std::fs::File::create(&py_file).expect("create file");
            file.write_all(b"result = (a + b) == c\n")
                .expect("write file");
        }

        let mut registry = MutatorRegistry::new();
        registry.register(Box::new(builtin::arithmetic::ArithmeticOp));
        registry.register(Box::new(builtin::comparison::ComparisonOp));

        let filters = vec!["arithmetic".to_owned()];
        let mutants = generate_mutants_for_file(&py_file, &registry, None, &filters)
            .expect("should generate mutants");

        // Only arithmetic mutations should be present.
        for mutant in &mutants {
            assert_eq!(mutant.mutator_name, "arithmetic_op");
        }
        assert!(!mutants.is_empty());
    }

    // -- Path filter tests --------------------------------------------------

    /// Empty path filters keep all files.
    #[test]
    fn path_filter_empty_keeps_all() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("create src dir");

        let py_a = src.join("a.py");
        let py_b = src.join("b.py");
        {
            let mut file = std::fs::File::create(&py_a).expect("create file");
            file.write_all(b"x = 1 + 2\n").expect("write file");
        }
        {
            let mut file = std::fs::File::create(&py_b).expect("create file");
            file.write_all(b"y = 3 + 4\n").expect("write file");
        }

        let mut registry = MutatorRegistry::new();
        registry.register(Box::new(builtin::arithmetic::ArithmeticOp));

        let files = vec![py_a.clone(), py_b.clone()];
        let opts = GenerationOptions {
            seed: None,
            filter_operators: &[],
            filter_paths: &[],
            per_file: &[],
            global_mutators: &MutatorConfig::default(),
        };
        let mutants =
            generate_mutants_for_files(&files, &registry, &opts).expect("should generate mutants");

        assert_eq!(mutants.len(), 2_usize);
    }

    /// Path filter restricts which files are mutated.
    #[test]
    fn path_filter_restricts_files() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("create src dir");

        let py_a = src.join("a.py");
        let py_b = src.join("b.py");
        {
            let mut file = std::fs::File::create(&py_a).expect("create file");
            file.write_all(b"x = 1 + 2\n").expect("write file");
        }
        {
            let mut file = std::fs::File::create(&py_b).expect("create file");
            file.write_all(b"y = 3 + 4\n").expect("write file");
        }

        let mut registry = MutatorRegistry::new();
        registry.register(Box::new(builtin::arithmetic::ArithmeticOp));

        let files = vec![py_a.clone(), py_b];
        let filter_paths = vec!["**/a.py".to_owned()];
        let opts = GenerationOptions {
            seed: None,
            filter_operators: &[],
            filter_paths: &filter_paths,
            per_file: &[],
            global_mutators: &MutatorConfig::default(),
        };
        let mutants =
            generate_mutants_for_files(&files, &registry, &opts).expect("should generate mutants");

        assert_eq!(mutants.len(), 1_usize);
        assert_eq!(mutants[0_usize].file_path, py_a);
    }

    /// Per-file config with `skip = true` excludes files from mutation.
    #[test]
    fn per_file_skip_excludes_file() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let py_a = dir.path().join("a.py");
        let py_b = dir.path().join("b.py");
        {
            let mut file = std::fs::File::create(&py_a).expect("create file");
            file.write_all(b"x = 1 + 2\n").expect("write file");
        }
        {
            let mut file = std::fs::File::create(&py_b).expect("create file");
            file.write_all(b"y = 3 + 4\n").expect("write file");
        }

        let mut registry = MutatorRegistry::new();
        registry.register(Box::new(builtin::arithmetic::ArithmeticOp));

        let per_file = vec![PerFileConfig {
            pattern: "**/a.py".to_owned(),
            mutators: None,
            timeout: None,
            skip: true,
        }];
        let files = vec![py_a, py_b.clone()];
        let opts = GenerationOptions {
            seed: None,
            filter_operators: &[],
            filter_paths: &[],
            per_file: &per_file,
            global_mutators: &MutatorConfig::default(),
        };
        let mutants =
            generate_mutants_for_files(&files, &registry, &opts).expect("should generate mutants");

        assert_eq!(mutants.len(), 1_usize);
        assert_eq!(mutants[0_usize].file_path, py_b);
    }

    /// Per-file config with mutator overrides merges with the global registry.
    #[test]
    fn per_file_custom_mutators() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let py_file = dir.path().join("target.py");
        {
            let mut file = std::fs::File::create(&py_file).expect("create file");
            // Has both arithmetic and comparison operators.
            file.write_all(b"x = (a + b) == c\n").expect("write file");
        }

        // Global config enables both arithmetic and comparison.
        let global_config = MutatorConfig {
            arithmetic_op: true,
            comparison_op: true,
            ..MutatorConfig::default()
        };
        let registry = build_registry(&global_config);

        // Per-file override: disable arithmetic only (merge semantics).
        let per_file_overrides = MutatorOverrides {
            arithmetic_op: Some(false),
            ..MutatorOverrides::default()
        };
        let per_file = vec![PerFileConfig {
            pattern: "**/target.py".to_owned(),
            mutators: Some(per_file_overrides),
            timeout: None,
            skip: false,
        }];
        let files = vec![py_file];
        let opts = GenerationOptions {
            seed: None,
            filter_operators: &[],
            filter_paths: &[],
            per_file: &per_file,
            global_mutators: &global_config,
        };
        let mutants =
            generate_mutants_for_files(&files, &registry, &opts).expect("should generate mutants");

        // Should only have comparison mutations, no arithmetic (disabled by override).
        let mutator_names: Vec<&str> = mutants.iter().map(|m| m.mutator_name.as_str()).collect();
        assert!(!mutator_names.contains(&"arithmetic_op"));
        assert!(mutator_names.contains(&"comparison_op"));
    }

    /// Pragma suppression suppresses all mutations on a line.
    #[test]
    fn pragma_no_mutate_suppresses_all() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let py_file = dir.path().join("pragma.py");
        {
            let mut file = std::fs::File::create(&py_file).expect("create file");
            file.write_all(b"x = 1 + 2  # pragma: no mutate\n")
                .expect("write file");
        }

        let mut registry = MutatorRegistry::new();
        registry.register(Box::new(builtin::arithmetic::ArithmeticOp));

        let mutants =
            generate_mutants_for_file(&py_file, &registry, None, &[]).expect("should generate");
        assert!(mutants.is_empty());
    }

    /// Pragma suppression with operator name only suppresses that operator.
    #[test]
    fn pragma_no_mutate_specific_operator() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let py_file = dir.path().join("pragma_op.py");
        {
            let mut file = std::fs::File::create(&py_file).expect("create file");
            file.write_all(b"x = 1 + 2  # pragma: no mutate(comparison_op)\n")
                .expect("write file");
        }

        let mut registry = MutatorRegistry::new();
        registry.register(Box::new(builtin::arithmetic::ArithmeticOp));

        let mutants =
            generate_mutants_for_file(&py_file, &registry, None, &[]).expect("should generate");
        // arithmetic_op should NOT be suppressed (only comparison_op is).
        assert_eq!(mutants.len(), 1_usize);
        assert_eq!(mutants[0_usize].mutator_name, "arithmetic_op");
    }
}
