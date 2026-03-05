# Contributing to fest

Thank you for your interest in contributing to fest! This document covers the development setup, workflow, and conventions you should follow.

## Getting started

### Prerequisites

- **Rust 1.93** -- pinned in `rust-toolchain.toml`, installed automatically via rustup
- **Rust nightly** -- required only for formatting (`cargo +nightly fmt`)
- **Python 3** with `pytest` and `pytest-cov`
- **[just](https://github.com/casey/just)** -- task runner
- **[cargo-deny](https://github.com/EmbarkStudios/cargo-deny)** -- dependency auditing
- **[cargo-machete](https://github.com/bnjbvr/cargo-machete)** -- unused dependency detection
- **[cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov)** -- code coverage
- **[jscpd](https://github.com/kucherenko/jscpd)** -- copy-paste detection

### Setup

```bash
git clone <repo-url>
cd fest

# Rust toolchain is auto-installed from rust-toolchain.toml
rustup install nightly          # for formatting

# Install dev tools
cargo install cargo-deny cargo-machete cargo-llvm-cov
npm install -g jscpd            # or use npx

# Verify everything works
just check-all
```

## Development commands

All commands use `just`:

| Command | Description |
|---------|-------------|
| `just build` | Build (`cargo build --all-features`) |
| `just check` | Fast compile check |
| `just test` | Run tests (`cargo test --all-features`) |
| `just lint` | Clippy lints |
| `just fmt` | Format code (requires nightly) |
| `just fmt-check` | Check formatting |
| `just deny` | Audit dependencies |
| `just machete` | Detect unused dependencies |
| `just coverage` | Code coverage -- **fails below 95%** |
| `just coverage-html` | Generate HTML coverage report |
| `just jscpd` | Detect copy-paste code |
| `just check-all` | Full pre-commit suite |

Run a single test:

```bash
cargo test --all-features <test_name>
```

## Code style

### Formatting

- 4-space indent, 100-character max width, Unix newlines
- Crate-level import grouping (nightly feature)
- Always format before committing: `just fmt`

### Clippy

Clippy runs in strict mode with `pedantic` and `nursery` groups elevated to deny, plus extensive restriction lints. Key limits:

- Cognitive complexity: 15
- Max function arguments: 5
- Max function lines: 60
- No wildcard imports
- No `unwrap`, `expect`, `panic`, `todo`, `unimplemented`, `dbg!`, `print!`
- No placeholder names (`foo`, `bar`, `baz`)

See `[lints]` in `Cargo.toml` for the full configuration.

### Documentation

- All public items must have doc comments (`missing_docs = "deny"`)
- All private items must have doc comments (`missing_docs_in_private_items = "deny"`)
- Technical terms in doc comments must be backtick-quoted (clippy `doc_markdown`)

## Testing

- All new code must have tests
- Coverage must stay at or above **95% line coverage**
- Run `just coverage` to verify before submitting
- Tests live in a `#[cfg(test)] mod tests` block at the bottom of each module

## Dependencies

- All dependencies must come from **crates.io** (no git sources except ruff)
- Wildcard version specs are denied
- New dependencies must pass `cargo deny check` (license allowlist, advisory database)
- If a dependency is only used via derive macros, add it to `[package.metadata.cargo-machete] ignored`

## Commit conventions

This project uses **conventional commits**. The changelog is generated from commit messages, so please follow the format:

```
<type>: <short description>

[optional body]
```

Types used in the changelog:

| Type | Use for |
|------|---------|
| `feat` | New features |
| `fix` | Bug fixes |
| `docs` | Documentation changes |
| `perf` | Performance improvements |
| `refactor` | Code restructuring without behavior changes |
| `build` | Build system changes |
| `ci` | CI/CD changes |

## Project structure

```
src/
  main.rs              CLI entry point
  lib.rs               Pipeline orchestrator
  cli.rs               Argument parsing
  config.rs            Config file loading
  config/types.rs      Config structs
  error.rs             Error types
  coverage.rs          Coverage collection
  coverage/            JSON + SQLite parsers
  mutation.rs          Mutant generation
  mutation/builtin/    8 built-in mutators
  runner.rs            Runner trait + dispatch
  runner/              Plugin and subprocess backends
  report.rs            Report generation
  report/              Text, JSON, HTML formatters
  progress/            Progress bar rendering
  signal.rs            Signal handling
  plugin.rs            Embedded pytest plugin
```

## License

By contributing, you agree that your contributions will be dual-licensed under MIT and Apache-2.0, consistent with the project license.
