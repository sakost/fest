//! Benchmarks for mutant generation on real Python source files (Flask).
//!
//! Requires Flask source at `../flask/src/flask/` relative to the project root.

#![allow(
    missing_docs,
    clippy::missing_docs_in_private_items,
    unused_results,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::missing_inline_in_public_items,
    clippy::default_numeric_fallback
)]

use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use fest::{
    config::MutatorConfig,
    mutation::{GenerationOptions, build_registry, generate_mutants_for_files},
};

/// Resolve the Flask source directory relative to the project root.
fn flask_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../flask/src/flask")
        .canonicalize()
        .expect("Flask source not found at ../flask/src/flask — clone it first")
}

/// Collect Flask `.py` files sorted by line count (ascending).
fn flask_files_by_size() -> Vec<(String, PathBuf)> {
    let dir = flask_dir();
    let mut files: Vec<(String, PathBuf)> = Vec::new();

    for entry in std::fs::read_dir(&dir).expect("read Flask dir") {
        let entry = entry.expect("read entry");
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "py") {
            let name = path
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .to_string();
            files.push((name, path));
        }
    }

    // Sort by file size so we get small/medium/large ordering.
    files.sort_by_key(|(_, path)| std::fs::metadata(path).map(|m| m.len()).unwrap_or(0));
    files
}

/// Pick representative small/medium/large files from Flask.
fn pick_representative_files() -> Vec<(String, Vec<PathBuf>)> {
    let files = flask_files_by_size();
    let mut groups: Vec<(String, Vec<PathBuf>)> = Vec::new();

    // Small: first file (~smallest)
    if let Some((name, path)) = files.first() {
        groups.push((format!("small ({name})"), vec![path.clone()]));
    }

    // Medium: middle file
    if files.len() > 2 {
        let mid = files.len() / 2;
        let (name, path) = &files[mid];
        groups.push((format!("medium ({name})"), vec![path.clone()]));
    }

    // Large: last file (~largest)
    if files.len() > 1 {
        if let Some((name, path)) = files.last() {
            groups.push((format!("large ({name})"), vec![path.clone()]));
        }
    }

    // All Flask files
    let all_paths: Vec<PathBuf> = files.iter().map(|(_, p)| p.clone()).collect();
    groups.push((format!("all ({} files)", all_paths.len()), all_paths));

    groups
}

/// Benchmark: generate mutants for individual files and all of Flask.
fn bench_generate_mutants(criterion: &mut Criterion) {
    let config = MutatorConfig::default();
    let registry = build_registry(&config);
    let opts = GenerationOptions {
        seed: Some(42),
        filter_operators: &[],
        filter_paths: &[],
        per_file: &[],
        global_mutators: &config,
    };

    let groups = pick_representative_files();

    let mut group = criterion.benchmark_group("generate_mutants");
    for (label, files) in &groups {
        group.bench_with_input(BenchmarkId::new("flask", label), files, |bench, files| {
            bench.iter(|| {
                generate_mutants_for_files(files, &registry, &opts)
                    .expect("mutant generation should not fail")
            });
        });
    }
    group.finish();
}

/// Benchmark: Python AST parsing only (via ruff), no mutation.
fn bench_parse_python(criterion: &mut Criterion) {
    let groups = pick_representative_files();

    let mut group = criterion.benchmark_group("parse_python");
    for (label, files) in &groups {
        // Pre-read file contents to benchmark parsing only, not I/O.
        let sources: Vec<String> = files
            .iter()
            .map(|path| std::fs::read_to_string(path).expect("read file"))
            .collect();

        group.bench_with_input(
            BenchmarkId::new("flask", label),
            &sources,
            |bench, sources| {
                bench.iter(|| {
                    for source in sources {
                        let _parsed = ruff_python_parser::parse_module(source)
                            .expect("parse should not fail");
                    }
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_generate_mutants, bench_parse_python);
criterion_main!(benches);
