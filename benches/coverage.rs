//! Benchmarks for coverage data parsing (SQLite and JSON).
//!
//! Requires Flask coverage data at `../flask/.coverage` and `../flask/.coverage.json`.
//! Generate with:
//!   cd ../flask && source .venv/bin/activate
//!   python -m pytest --cov=src/flask --cov-context=test --no-header -q
//!   python -m coverage json --show-contexts -o .coverage.json

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

use criterion::{Criterion, criterion_group, criterion_main};
use fest::coverage::{load_cached_coverage, load_coverage_from};

/// Resolve the Flask project directory.
fn flask_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../flask")
        .canonicalize()
        .expect("Flask project not found at ../flask — clone it first")
}

/// Benchmark: parse `.coverage` SQLite database.
fn bench_parse_coverage_sqlite(criterion: &mut Criterion) {
    let flask = flask_dir();
    let db_path = flask.join(".coverage");
    assert!(
        db_path.exists(),
        ".coverage not found — run pytest --cov in Flask first"
    );

    criterion.bench_function("parse_coverage_sqlite", |bench| {
        bench.iter(|| load_cached_coverage(&flask).expect("SQLite coverage parse failed"));
    });
}

/// Benchmark: parse `.coverage.json` file.
fn bench_parse_coverage_json(criterion: &mut Criterion) {
    let flask = flask_dir();
    let json_path = flask.join(".coverage.json");
    assert!(
        json_path.exists(),
        ".coverage.json not found — run `coverage json --show-contexts` in Flask first"
    );

    criterion.bench_function("parse_coverage_json", |bench| {
        bench.iter(|| load_coverage_from(&json_path, &flask).expect("JSON coverage parse failed"));
    });
}

criterion_group!(
    benches,
    bench_parse_coverage_sqlite,
    bench_parse_coverage_json
);
criterion_main!(benches);
