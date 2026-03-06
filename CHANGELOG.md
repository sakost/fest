# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/sakost/fest/releases/tag/v0.1.0) - 2026-03-06

### Added

- in-place mutation, CI/CD, PyPI packaging, and benchmark
- add filtering, session management, per-file config, and seed reports
- add 9 new mutation operators and seed support
- add persistent pytest worker pool for faster mutation testing
- read .coverage SQLite database directly, eliminating coverage json export
- add fancy CLI output with dedicated render task
- add runner backend selection with plugin default and subprocess fallback
- speed up coverage collection with caching and fast backend
- integrate progress and signal into pipeline
- add signal handling module
- add progress module with verbose and bar modes
- add pytest plugin backend with JSON-over-Unix-socket protocol
- add HTML report formatter with source-annotated output
- wire up run() pipeline with registry-from-config
- add report module with types, text reporter, and JSON reporter
- add Runner trait and subprocess fallback backend
- add coverage analysis module with JSON-based coverage parsing
- add mutant generation orchestrator with file discovery and text splicing
- add CLI argument parsing with clap derive
- implement all 8 built-in mutation operators
- add core mutation types, Mutator trait, and MutatorRegistry
- implement config module with types and TOML loading
- add project scaffolding with lib+bin targets and module skeleton
- initial project setup with strict linting and architecture design

### Fixed

- resolve CI failures (clippy, fmt, release-plz, Windows long paths)
- animate phase spinner with indicatif steady tick
- invalidate coverage cache when config files change
- resolve coverage paths to absolute for mutant matching
- apply rustfmt and add runner selection TODO
- address code quality review issues in pytest plugin backend
- address spec review issues in HTML reporter
- address code review issues in run() pipeline
- apply nightly formatting and fix silent pyproject.toml error swallowing

### Other

- add cosmic-ray feature parity design
- add README and CONTRIBUTING guide
- extract shared build_python_path and remove unused import
- add NO COVERAGE label and div balance tests for HTML reporter
