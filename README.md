# fest

A fast mutation-testing tool for Python, written in Rust.

fest generates small changes (mutants) to your Python source code and checks whether your test suite detects them. Surviving mutants indicate gaps in test coverage that line-coverage tools miss.

## Features

- **Fast** -- Rust-powered mutant generation using [ruff](https://github.com/astral-sh/ruff)'s Python parser, parallel execution with configurable workers, and a persistent pytest plugin runner that avoids per-mutant process overhead.
- **Coverage-guided** -- Only runs the tests that actually exercise each mutated line, using per-test context data from pytest-cov.
- **8 built-in mutators** -- Arithmetic, comparison, boolean, return value, negate condition, remove decorator, constant replacement, and exception swallowing operators.
- **Multiple output formats** -- Text (default), JSON, and self-contained HTML reports.
- **Configurable** -- fest.toml or `[tool.fest]` in pyproject.toml. Every option can be overridden from the CLI.

## Requirements

- **Rust 1.93+** (only for building from source)
- **Python 3** with:
  - [pytest](https://docs.pytest.org/) >= 7.0
  - [pytest-cov](https://pytest-cov.readthedocs.io/)

## Installation

```bash
cargo install --path .
```

Ensure `pytest` and `pytest-cov` are installed in the Python environment fest will test:

```bash
pip install pytest pytest-cov
```

## Quick start

Run fest in your project root:

```bash
fest run
```

fest will:

1. Discover Python source files matching `src/**/*.py` (default).
2. Run pytest with coverage to build a per-test line map.
3. Generate mutants from the discovered source.
4. Test each mutant against only the relevant tests.
5. Print a summary report.

## CLI reference

```
fest run [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `-s, --source <PATTERNS>` | Glob patterns for source files |
| `-e, --exclude <PATTERNS>` | Glob patterns to exclude |
| `-w, --workers <N>` | Number of parallel test workers |
| `--workers-cpu-ratio <RATIO>` | Fraction of CPUs (default: 0.75) |
| `-t, --timeout <SECONDS>` | Per-test timeout (default: 10) |
| `--fail-under <SCORE>` | Minimum mutation score (0--100) |
| `-o, --output <FORMAT>` | `text`, `json`, or `html` |
| `-b, --backend <BACKEND>` | `plugin` (default) or `subprocess` |
| `-c, --config <PATH>` | Path to config file |
| `--coverage-from <PATH>` | Use pre-existing `.coverage` or `.coverage.json` |
| `--no-coverage-cache` | Disable mtime-based coverage caching |
| `--no-fast-coverage` | Don't force C-based coverage tracer |
| `--progress <STYLE>` | `auto`, `fancy`, `plain`, `verbose`, `quiet` |
| `-v, --verbose` | Verbose per-mutant progress |

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Mutation score below `--fail-under` threshold |
| 130 | Cancelled by signal |

## Configuration

Create `fest.toml` in your project root, or add a `[tool.fest]` section to `pyproject.toml`:

```toml
[fest]
source = ["src/**/*.py"]
exclude = ["src/generated/**"]
test_runner = "pytest"
workers = 4
timeout = 10
fail_under = 80.0
output = "text"
backend = "plugin"
coverage_cache = true
fast_coverage = true

[fest.mutators]
arithmetic_op = true
comparison_op = true
boolean_op = true
return_value = true
negate_condition = true
remove_decorator = true
constant_replace = true
exception_swallow = true
```

All fields are optional and fall back to defaults.

## Mutation operators

| Mutator | Example |
|---------|---------|
| Arithmetic | `x + y` &rarr; `x - y` |
| Comparison | `a == b` &rarr; `a != b` |
| Boolean | `a and b` &rarr; `a or b`, `not x` &rarr; `x` |
| Return value | `return expr` &rarr; `return None` |
| Negate condition | `if cond:` &rarr; `if not (cond):` |
| Remove decorator | `@cache` &rarr; *(removed)* |
| Constant replace | `True` &rarr; `False`, `0` &rarr; `1`, `""` &rarr; `"mutant"` |
| Exception swallow | `except: handle()` &rarr; `except: pass` |

## Runner backends

fest ships two backends:

- **Plugin** (default) -- Embeds a pytest plugin that patches modules in-process. Faster because one pytest worker handles many mutants without restarting.
- **Subprocess** -- Spawns a fresh `python -m pytest` for each mutant. Slower but universally compatible. Used as automatic fallback if the plugin backend fails.

## Understanding results

fest reports a **mutation score**: the percentage of tested mutants that were killed (detected) by the test suite. Mutants with no test coverage are excluded from the denominator.

```
Mutation score: 85.0% (170/200 killed, 50 no coverage)
```

A surviving mutant means a code change was not caught -- a potential blind spot in your tests.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
