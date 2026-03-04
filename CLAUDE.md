# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**fest** is a mutation-testing library for Python, written in Rust (edition 2024). Pre-1.0 stage — breaking API changes are acceptable.

## Build & Development Commands

Uses [just](https://github.com/casey/just) as the task runner. Key commands:

| Command | Description |
|---------|-------------|
| `just build` | Build (`cargo build --all-features`) |
| `just check` | Fast compile check |
| `just test` | Run tests (`cargo test --all-features`) |
| `just lint` | Clippy lints (`cargo clippy --all-features`) |
| `just fmt` | Format code (requires **nightly**: `cargo +nightly fmt`) |
| `just fmt-check` | Check formatting (nightly) |
| `just deny` | Audit dependencies (advisories, licenses, bans, sources) |
| `just machete` | Detect unused dependencies |
| `just coverage` | Code coverage — **fails below 95% line coverage** |
| `just check-all` | Full pre-commit suite: fmt-check, lint, deny, machete, test, jscpd |

Run a single test: `cargo test --all-features <test_name>`

## Toolchain

- Rust **1.93** (pinned in `rust-toolchain.toml`)
- Components: cargo, clippy, rustfmt, llvm-tools, rust-src
- Formatting requires **nightly** for the full feature set

## Code Style & Linting

- **rustfmt**: 4-space indent, 100-char max width, Unix newlines, crate-level import grouping (nightly)
- **Clippy** (strict): cognitive complexity ≤ 15, max function args ≤ 5, max function lines ≤ 60, wildcard imports warned everywhere
- Placeholder names (`foo`, `bar`, `baz`, etc.) are disallowed

## Conventions

- **Conventional commits** required — changelog generated from: `feat`, `fix`, `docs`, `perf`, `refactor`, `build`, `ci`
- Git tags: `v{{ version }}`
- Dual-licensed: MIT / Apache-2.0
- Dependencies must come from crates.io only; wildcard version specs denied
