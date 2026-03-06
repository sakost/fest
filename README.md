# fest

**An extremely fast mutation testing tool for Python.**

<p align="center">
  <img src="https://img.shields.io/pypi/v/fest-mutate?style=flat-square&color=blue" alt="PyPI">
  <img src="https://img.shields.io/github/license/sakost/fest?style=flat-square" alt="License">
  <img src="https://img.shields.io/badge/rust-1.93+-orange?style=flat-square" alt="Rust">
  <img src="https://img.shields.io/badge/python-3.9+-3776AB?style=flat-square" alt="Python">
</p>

---

fest generates small changes (_mutants_) to your Python source code and checks whether your test
suite catches them. Surviving mutants reveal gaps that line coverage alone cannot find.

Built in Rust with [ruff](https://github.com/astral-sh/ruff)'s Python parser. **~25× faster** than
[cosmic-ray](https://github.com/sixty-north/cosmic-ray) on real-world projects
([benchmark](docs/benchmark-2026-03-06.md)).

## Highlights

- 🔥 **Parallel execution** — runs mutants across all CPU cores simultaneously
- 🎯 **Coverage-guided** — only runs tests that cover the mutated line, via per-test `pytest-cov` context
- ⚡ **In-process plugin** — a persistent pytest worker pool avoids per-mutant startup overhead
- 🧬 **17 mutation operators** — arithmetic, comparison, boolean, return value, constants, decorators, loops, and more
- 📊 **Multiple output formats** — text, JSON, and self-contained HTML reports
- 🔄 **Session support** — stop and resume long runs with SQLite-backed sessions

## Installation

### From PyPI (recommended)

```bash
pip install fest-mutate
```

### From source

```bash
git clone https://github.com/sakost/fest.git
cd fest
cargo build --release
```

The binary will be at `target/release/fest`. Make sure `pytest` and `pytest-cov` are installed in
the Python environment you want to test:

```bash
pip install pytest pytest-cov
```

## Quick start

```bash
cd your-python-project
fest run
```

fest will:

1. Discover Python source files matching `src/**/*.py` (configurable)
2. Run pytest with coverage to build a per-test line map
3. Generate mutants from the discovered source
4. Test each mutant against only the relevant tests
5. Print a summary report

```
fest — mutation testing for Python

  Configuration loaded (fest.toml)  0ms
  Mutator registry built (14 mutators)  0ms
  Source files discovered (14 files)  0ms
  Mutants generated (4290 mutants)  23ms
  Coverage collected  40ms
  Session opened (.fest-session.db)  0ms
  Test workers ready (24 workers)  994ms
  Mutants tested (4186 mutants)  4m 6s

  Mutation Score: 85.7%  |  Killed: 2401  Survived: 314  Timeout: 80  Errors: 8
```

## Configuration

Create `fest.toml` in your project root (or add `[tool.fest]` to `pyproject.toml`):

```toml
[fest]
source = ["src/**/*.py"]
exclude = ["**/test_*.py", "**/conftest.py"]
timeout = 30
workers = 8                    # default: 75% of CPU cores
fail_under = 80.0              # exit 1 if score is below this
output = "text"                # "text", "json", or "html"
backend = "plugin"             # "plugin" or "subprocess"
session = ".fest-session.db"   # enable stop/resume
```

All fields are optional — fest picks sensible defaults.

## CLI

```
fest run [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `-s, --source <GLOB>` | Source file patterns |
| `-e, --exclude <GLOB>` | Exclude patterns |
| `-w, --workers <N>` | Parallel test workers |
| `-t, --timeout <SEC>` | Per-test timeout (default: 10) |
| `--fail-under <SCORE>` | Minimum mutation score (0–100) |
| `-o, --output <FMT>` | `text` · `json` · `html` |
| `-b, --backend <BE>` | `plugin` (default) · `subprocess` |
| `--coverage-from <PATH>` | Use existing `.coverage` file |
| `--session <PATH>` | SQLite session for stop/resume |
| `--reset` | Reset session before running |
| `--incremental` | Only re-test changed files |
| `--seed <N>` | Deterministic mutation seed |
| `--filter-operators <PAT>` | Include/exclude operators by name |
| `--filter-paths <GLOB>` | Restrict mutation to matching files |
| `--progress <STYLE>` | `auto` · `fancy` · `plain` · `verbose` · `quiet` |
| `-v, --verbose` | Per-mutant progress output |

## Mutation operators

| Operator | Example |
|----------|---------|
| `arithmetic_op` | `x + y` → `x - y` |
| `augmented_assign` | `x += 1` → `x -= 1` |
| `bitwise_op` | `a & b` → `a \| b` |
| `boolean_op` | `a and b` → `a or b` |
| `break_continue` | `break` → `continue` |
| `comparison_op` | `a == b` → `a != b` |
| `constant_replace` | `True` → `False`, `0` → `1` |
| `exception_swallow` | `raise Error()` → `pass` |
| `negate_condition` | `if x:` → `if not x:` |
| `remove_decorator` | `@cache` → _(removed)_ |
| `remove_super` | `super().__init__()` → _(removed)_ |
| `return_value` | `return val` → `return None` |
| `statement_deletion` | `do_something()` → `pass` |
| `unary_op` | `-x` → `x`, `~x` → `x` |
| `variable_replace` | `a = x` → `a = y` (same-scope same-type) |
| `variable_insert` | `a = f(x)` → `a = f(y)` |
| `zero_iteration` | `for x in items:` → `for x in []:` |

## Backends

| Backend | How it works | Speed | Compatibility |
|---------|-------------|-------|---------------|
| **plugin** (default) | Patches modules in a long-lived pytest process | ⚡ Fast | Most projects |
| **subprocess** | Overwrites source file on disk, runs `pytest` | Slower | Universal |

The plugin backend falls back to subprocess automatically on infrastructure errors.

## Performance

On [python-ecdsa](https://github.com/tlsfuzzer/python-ecdsa) (17k lines, 1,477 tests):

| | fest | cosmic-ray |
|---|---|---|
| **Throughput** | 17.4 mut/s | 0.7 mut/s |
| **Time to complete** | 4 min | ~6 hours (estimated) |
| **Speedup** | **~25×** | baseline |

See the full [benchmark report](docs/benchmark-2026-03-06.md) for methodology and reproduction
steps.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
