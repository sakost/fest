# fest Architecture Design

## Overview

fest is a fast, Python-specific mutation testing tool written in Rust. It parses Python
source files, generates mutants via AST manipulation, runs only the relevant tests against
each mutant, and produces a mutation score report.

## Goals

- Fast: in-memory mutation via pytest plugin, parallel workers, coverage-guided test selection
- Ready-to-use: sensible defaults, single `fest` command to run
- Python-specific: uses ruff_python_ast for accurate Python parsing
- Pluggable mutations: built-in mutators, TOML declarative, Rust dylib, per-project Python scripts

## High-Level Architecture

```
+----------------------------------+
|           CLI (clap)             |  Config, args, progress display
+----------------------------------+
|         Orchestrator             |  Coverage analysis, mutant generation,
|  (rayon + tokio async I/O)       |  scheduling, result collection
+-----------+----------------------+
|  pytest   |   subprocess         |  Two execution backends
|  plugin   |   runner             |
| (primary) |  (fallback)          |
+-----------+----------------------+
```

### Flow

1. fest parses config and discovers Python source files.
2. Runs `pytest --cov --cov-context=test` to build a line-to-test map.
3. Parses each `.py` file with `ruff_python_ast`, applies mutators to generate mutants.
4. Distributes mutants across worker threads (rayon).
5. Each worker sends the mutant to a pytest plugin (primary) or subprocess (fallback).
6. Collects results and produces a mutation score report.

## Mutant Generation

Parsing and mutation happen entirely in Rust on the rayon thread pool.

Each mutant holds:
- File path
- Line/column span
- Original AST node (serialized)
- Mutated source text (the replacement string)
- Mutator name (e.g. "negate_condition")

Source generation uses text-level splicing: the original source bytes are taken and the byte
range `[start..end]` is replaced with the mutated text. This preserves formatting, comments,
and everything outside the mutation point.

### Built-in Mutators

| Mutator            | Example                              |
|--------------------|--------------------------------------|
| ArithmeticOp       | `+` -> `-`, `*` -> `/`              |
| ComparisonOp       | `==` -> `!=`, `<` -> `>=`           |
| BooleanOp          | `and` -> `or`, `not x` -> `x`       |
| ReturnValue        | `return x` -> `return None`          |
| NegateCondition    | `if cond:` -> `if not cond:`         |
| RemoveDecorator    | `@cache` -> removed                  |
| ConstantReplace    | `True` -> `False`, `0` -> `1`        |
| ExceptionSwallow   | `except: raise` -> `except: pass`    |

### Pluggable Mutator Interfaces

1. **TOML declarative**: simple pattern-based mutations in config (e.g. `pattern = "assert {expr}"`, `replacement = "assert not {expr}"`). fest compiles these into AST matchers at startup.
2. **Rust dylib**: users compile shared libraries implementing the `Mutator` trait. fest loads them at runtime via `libloading`.
3. **Per-project Python scripts**: local `.py` files that define mutation functions. Loaded via the plugin worker process.

The internal `Mutator` trait is the common interface. Built-in, TOML, dylib, and Python
mutators all implement it (Python mutators are proxied through the worker).

## Execution Backends

**Why tokio**: rayon handles CPU-bound mutant generation, but the worker communication
(Unix sockets to pytest processes, timeout management, subprocess I/O) is I/O-bound.
tokio provides async socket reads, process spawning with timeout, and select-style
multiplexing across workers without blocking rayon threads.

### Primary: pytest plugin

fest embeds a `_fest_plugin.py` (via `include_str!`) and writes it to a temp location at
startup. It spawns N pytest worker processes (one per rayon thread), each injected with
`pytest -p _fest_plugin --fest-socket <path>`.

Protocol (JSON over Unix socket, one message per line):

```
fest (Rust)                          pytest (Python)
    |                                     |
    |-- spawn: pytest -p _fest_plugin --> |
    |                                     |
    | <--------- READY ------------------ |  plugin connects to socket
    |                                     |
    |-- MUTANT {file, func, code} ------> |
    |                                     |  plugin does:
    |                                     |  1. compile + load mutated code into module.__dict__
    |                                     |  2. runs relevant tests
    |                                     |  3. restores module.__dict__
    | <--------- RESULT {killed/survived} |
    |                                     |
    |-- MUTANT ... ---------------------> |  repeat
    |                                     |
    |-- SHUTDOWN -----------------------> |
```

Each worker is a separate Python process. No shared state between workers.

### Fallback: subprocess runner

For situations where the plugin approach cannot be used:

1. fest writes the mutated `.py` file to a temp directory.
2. Spawns `python -m pytest <test_file>::<test_func>`.
3. Reads exit code: 0 = survived, non-zero = killed.
4. Cleans up the temp file.

Slower (process per mutant) but universally compatible. unittest support may be added here
later if it proves easy to maintain.

## Coverage Analysis

fest uses `pytest-cov` with per-test context to build a line-to-test map.

1. Runs `python -m pytest --cov --cov-context=test`.
2. Parses the `.coverage` sqlite database (well-documented schema: `context` table maps to test names, `line_bits` table maps to covered lines per file per context).
3. Builds an in-memory `HashMap<(FilePath, LineNumber), Vec<TestId>>`.

For each mutant at line L in file F:
- Look up `coverage_map[(F, L)]` to get the relevant tests.
- If empty: mark as "no coverage" (reported separately, not counted as survived).
- If non-empty: only those tests run against this mutant.

Prerequisite: `pytest-cov` must be installed. fest checks at startup and gives a clear error
if missing.

## Report Formats

### Terminal (default)

Uses indicatif for progress display during run, then prints a summary:

```
fest mutation testing report
----------------------------
Files scanned:      12
Mutants generated:  347
Mutants tested:     312  (35 no coverage)
Killed:             289  (92.6%)
Survived:            23  (7.4%)
Timeout:              0
Errors:               0

Survived mutants:
  src/parser.py:42    ArithmeticOp    `x + 1` -> `x - 1`
  src/parser.py:87    NegateCondition `if valid:` -> `if not valid:`
```

### JSON (`--output json`)

Machine-readable. Contains every mutant with status, file, line, mutator name, original text,
mutated text, and which tests ran. This is the canonical format.

### HTML (`--output html`)

Source file view with line-by-line annotations: green for killed, red for survived, grey for
no coverage.

### CI integration

`--fail-under <score>` flag. Exit code 1 if mutation score is below threshold.

## Configuration

`fest.toml` at project root, or `[tool.fest]` section in `pyproject.toml`:

```toml
[fest]
source = ["src/**/*.py"]
exclude = ["src/generated/**", "src/migrations/**"]
test_runner = "pytest"
workers = 4                  # explicit count, takes priority
workers_cpu_ratio = 0.75     # fraction of CPUs (default), floor(ratio * num_cpus), min 1
timeout = 10                 # seconds per mutant
fail_under = 80.0            # minimum mutation score for CI
output = "text"              # "text", "json", "html"

[fest.mutators]
arithmetic_op = true
comparison_op = true
boolean_op = true
return_value = true
negate_condition = true
remove_decorator = true
constant_replace = true
exception_swallow = true

[[fest.mutators.custom]]
name = "swap_assert"
pattern = "assert {expr}"
replacement = "assert not {expr}"

[[fest.mutators.python]]
path = "mutators/my_custom.py"

[[fest.mutators.dylib]]
path = "target/release/libmy_mutator.so"
```

Worker count resolution: if `workers` is set, use it. Otherwise
`floor(workers_cpu_ratio * num_cpus)`, minimum 1.

## Project Structure

Follows the Rust lib + thin binary pattern:

```
src/
  main.rs              # Thin entry point: parses CLI, calls fest::run()
  lib.rs               # Public library root, re-exports modules
  config/
    mod.rs             # Config loading (fest.toml / pyproject.toml)
    types.rs           # Config structs (serde)
  mutation/
    mod.rs             # Mutant generation orchestrator
    mutant.rs          # Mutant struct definition
    mutator.rs         # Mutator trait + registry
    builtin/
      mod.rs
      arithmetic.rs
      comparison.rs
      boolean.rs
      return_value.rs
      negate_condition.rs
      remove_decorator.rs
      constant.rs
      exception.rs
    toml_mutator.rs    # TOML declarative pattern compiler
    dylib_loader.rs    # Rust dylib plugin loader
  coverage/
    mod.rs             # Coverage runner + parser
    sqlite.rs          # .coverage DB reader
  runner/
    mod.rs             # Runner trait + dispatch
    pytest_plugin.rs   # Pytest plugin backend
    subprocess.rs      # Subprocess fallback backend
  report/
    mod.rs             # Report generation dispatcher
    types.rs           # Result structs
    text.rs            # Terminal output
    json.rs            # JSON output
    html.rs            # HTML report
  plugin/
    _fest_plugin.py    # Embedded pytest plugin (include_str!)
```

`main.rs` is minimal:

```rust
fn main() -> Result<(), fest::Error> {
    let args = fest::cli::parse();
    fest::run(args)
}
```

All logic lives in `lib.rs` and its submodules. This makes the library independently testable
and usable as a dependency by other tools.

## Benchmark and Comparison Suite

### Comparison targets

mutmut (most popular), cosmic-ray, optionally pytest-gremlins.

### Metrics

| Metric           | Measurement method                                           |
|------------------|--------------------------------------------------------------|
| Wall-clock time  | hyperfine or built-in timing, end-to-end                     |
| Throughput       | Mutants tested per second                                    |
| Correctness      | Mutation score agreement with mutmut on same project         |
| False positives  | Survived mutants that are actually equivalent (manual audit)  |
| False negatives  | Killed mutants due to flaky tests (re-run in subprocess mode)|
| Memory (peak RSS)| /usr/bin/time -v on Linux                                    |
| CPU utilization  | pidstat or /proc/stat sampling                               |
| Scaling          | Same project with 1, 2, 4, 8, N cores                       |
| Determinism      | Run 3 times, check identical results                         |

### Benchmark projects

| Size   | Candidate                      | Purpose                        |
|--------|--------------------------------|--------------------------------|
| Small  | Custom toy project (~500 LOC)  | Controlled, fast iteration     |
| Medium | httpx or pydantic (~5k LOC)    | Real test suites               |
| Large  | requests or flask (~15k+ LOC)  | Stress test at scale           |

### Structure

```
benchmarks/
  projects/          # git submodules or download scripts
  run.py             # Orchestrates running fest + competitors
  compare.py         # Parses results, produces tables and charts
```

CI runs benchmarks on tagged releases only (not every PR).
