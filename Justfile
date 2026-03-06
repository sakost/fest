# Justfile for fest
# Install just: cargo install just

# Default recipe: show available commands
default:
    @just --list

# Build the project
build:
    cargo build --all-features

# Fast compile check
check:
    cargo check --all-features

# Run tests
test:
    cargo test --all-features

# Run clippy lints
lint:
    cargo clippy --all-features -- -D warnings

# Format code (nightly for full feature set)
fmt:
    cargo +nightly fmt

# Check formatting (nightly for full feature set)
fmt-check:
    cargo +nightly fmt --check

# Run cargo-deny checks (advisories, licenses, bans, sources)
deny:
    cargo deny check

# Check for unused dependencies
machete:
    cargo machete

# Run code coverage, fail if below 95% line coverage
coverage:
    cargo llvm-cov --all-features --fail-under-lines 95

# Generate HTML coverage report
coverage-html:
    cargo llvm-cov --all-features --html --output-dir target/coverage-html

# Detect copy-paste code
jscpd:
    npx jscpd src/

# Run mutation testing on fest itself
mutants:
    cargo mutants --all-features

# Compute code metrics
metrics:
    rust-code-analysis-cli -m -p ./src/ --pr -O json

# Run all benchmarks
bench: bench-mutation bench-coverage

# Benchmark mutant generation (requires ../flask)
bench-mutation:
    cargo bench --bench mutation

# Benchmark coverage parsing (requires ../flask with .coverage data)
bench-coverage:
    cargo bench --bench coverage

# Compare fest vs other tools: just bench-compare [TARGET_DIR] [SOURCE_GLOB] [TEST_DIR]
bench-compare *args:
    bash bench/compare.sh {{args}}

# Full pre-commit suite (coverage is CI-only due to speed)
check-all: fmt-check lint deny machete test jscpd
