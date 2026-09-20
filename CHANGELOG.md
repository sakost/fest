# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.4](https://github.com/sakost/fest/compare/v0.1.3...v0.1.4) (2026-09-20)


### Features

* **plugin:** in-process pytest mutation backend (Plan G + G2) ([8092d10](https://github.com/sakost/fest/commit/8092d103e7398fc147530d29c98f39b7ebcf1a2e))
* **plugin:** in-process pytest mutation backend (Plan G + G2) ([fc495fb](https://github.com/sakost/fest/commit/fc495fb256b2a18d8ce6cfd5f428653ef37d3141))


### Bug Fixes

* **mutation:** never mutate module-level `if __name__ == "__main__":` guards ([08e4453](https://github.com/sakost/fest/commit/08e445335feff7869b9c212bb4c69fcb12e9be99))
* **mutator:** do not emit the equivalent `return None` -&gt; `return None` mutant ([#23](https://github.com/sakost/fest/issues/23)) ([899d8df](https://github.com/sakost/fest/commit/899d8dfb9b3cf2580fc480f676d9cf1e66dadb56))
* **mutator:** keep parens balanced when boolean_op removes 'not' ([c3e2b8a](https://github.com/sakost/fest/commit/c3e2b8a0add22839817b2cd48a6faa70c5c625d0))
* never mutate `__main__` guards; keep plugin workers alive across SystemExit ([95a2b68](https://github.com/sakost/fest/commit/95a2b68a6fd100824fc21cab8d9ddb50417c8dd9))
* **plugin:** clear memoization caches around function-body swaps ([af69cb0](https://github.com/sakost/fest/commit/af69cb0965505a73f0fe598eff05db18f4e99db4))
* **plugin:** correct three plugin-backend failures found on real-world validation ([691f8e4](https://github.com/sakost/fest/commit/691f8e4eed041f850e58a5c91ad981e0f9ad9fc0))
* **plugin:** run workers in isolated process groups and honour cancellation ([#24](https://github.com/sakost/fest/issues/24)) ([e34790b](https://github.com/sakost/fest/commit/e34790bb87cc02cac9851f631e1bc7e7d3025445))
* **plugin:** survive `SystemExit` raised while applying a mutant ([1406023](https://github.com/sakost/fest/commit/14060238cd37e470d774a2ec9ba17348618ae367))
* **report:** always list survived mutants and point at the HTML report ([#22](https://github.com/sakost/fest/issues/22)) ([fd77f77](https://github.com/sakost/fest/commit/fd77f7755bb0d1f887395f87f0c30d1c5b93d34e))
* **report:** show file paths relative to the project directory ([#25](https://github.com/sakost/fest/issues/25)) ([680707a](https://github.com/sakost/fest/commit/680707ab33656931a378d21e8ea4fc1c928e67c5))
* **runner:** gate unix-only process-group items so Windows builds warning-free ([b3054c8](https://github.com/sakost/fest/commit/b3054c8207b9b963e5676ad23b33f103ce6df6bb))
* **runner:** kill timed-out pytest process trees and flush verdicts per mutant ([e81e561](https://github.com/sakost/fest/commit/e81e561a80bf1a9d442e197702ef2d1263d041f8))
* **runner:** kill timed-out pytest process trees and flush verdicts per mutant ([a7be2b3](https://github.com/sakost/fest/commit/a7be2b3a9bfac50d8284d1d9fc7e0e7fe979f6dd))
* **runner:** serialise subprocess mutants across files (false kills with --workers &gt; 1) ([1c2041a](https://github.com/sakost/fest/commit/1c2041a8e41466fc3af158af783630792452247f))
* **runner:** serialise subprocess mutants across files to stop cross-file contamination ([5682be9](https://github.com/sakost/fest/commit/5682be99dc2a2f8057219c4c075d63787a475567))

## [0.1.3](https://github.com/sakost/fest/compare/v0.1.2...v0.1.3) (2026-03-14)


### Bug Fixes

* prevent .pyc cache staleness in subprocess backend ([f19f4c0](https://github.com/sakost/fest/commit/f19f4c0ea5cab0d2c24d5eeb56649a331c6bf4ca))

## [0.1.2](https://github.com/sakost/fest/compare/v0.1.1...v0.1.2) (2026-03-09)


### Bug Fixes

* override --cov-fail-under so baseline check reflects test results only ([9b211f8](https://github.com/sakost/fest/commit/9b211f8dd42a2c8aefffce4fa6091f40947bffa6))

## [0.1.1](https://github.com/sakost/fest/compare/v0.1.0...v0.1.1) (2026-03-09)


### Features

* abort if baseline test suite fails before mutation testing ([7bccf88](https://github.com/sakost/fest/commit/7bccf8834bec385a45e47d2296cd24280ad3f18d))
* auto-detect Python from VIRTUAL_ENV and local .venv ([5686841](https://github.com/sakost/fest/commit/56868419666e5f7f48f961bc53c6f73a0fa64ee3))


### Bug Fixes

* use MultiProgress to prevent flickering between spinner and progress bar ([057b469](https://github.com/sakost/fest/commit/057b4698efc9b7842bba8a02d4f565bea53c2e33))

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
