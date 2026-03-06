# Benchmark: fest vs cosmic-ray on python-ecdsa

**Date:** 2026-03-06
**fest version:** 0.1.0 (commit 684a499, `master`)
**cosmic-ray version:** 8.4.4

## Target project

| | |
|---|---|
| **Project** | [python-ecdsa](https://github.com/tlsfuzzer/python-ecdsa) (ECDSA cryptographic signature library) |
| **Commit** | `55aca78` (HEAD of `master`, 2026-03-06) |
| **Source files** | 14 files under `src/ecdsa/` (excluding tests, `_sha3.py`, `_version.py`) |
| **Source lines** | ~17,800 total |
| **Tests** | 1,477 passing in 1.4 s (`pytest --timeout 30 -x --fast -m 'not slow' src/`) |

This project was chosen because it ships a
[`cosmic-ray.toml`](https://github.com/tlsfuzzer/python-ecdsa/blob/master/cosmic-ray.toml)
in its repository, making it a realistic, unbiased benchmark target.

## Environment

| | |
|---|---|
| **CPU** | Intel Core i9-14900KF (32 logical cores) |
| **RAM** | 64 GB DDR5 |
| **OS** | Linux 6.19.5 (NixOS) |
| **Python** | 3.13.11 |
| **Rust** | 1.93.1 |

## How to reproduce

### 1. Clone and set up python-ecdsa

```bash
git clone https://github.com/tlsfuzzer/python-ecdsa.git
cd python-ecdsa
git checkout 55aca78  # optional: pin to this exact commit

uv venv .venv --python 3.13
uv pip install -e . pytest hypothesis cosmic-ray pytest-cov pytest-timeout
```

### 2. Verify tests pass

```bash
source .venv/bin/activate
python -m pytest --timeout 30 -x --fast -m 'not slow' src/ -q --no-header
# Expected: 1477 passed, 1 skipped, 271 deselected in ~1.4s
```

### 3. Collect coverage with test context

fest uses per-test coverage data to run only the relevant tests for each
mutant. Generate a `.coverage` SQLite database with test context:

```bash
python -m pytest --timeout 30 -x --fast -m 'not slow' src/ \
    -q --no-header --cov=src/ecdsa --cov-context=test --cov-report=
# Creates .coverage (~1 MB) with 1,879 test contexts
```

### 4. Create fest.toml

```toml
[fest]
source = ["src/ecdsa/**/*.py"]
exclude = ["**/test_*.py", "src/ecdsa/_sha3.py", "src/ecdsa/_version.py"]
timeout = 30
session = ".fest-session.db"
```

### 5. Run fest

```bash
# Build fest (from fest repo)
cargo build --release

# Run (from python-ecdsa directory, venv activated)
fest run --coverage-from .coverage --backend plugin --timeout 30 --progress plain --reset
```

### 6. Run cosmic-ray

python-ecdsa already ships `cosmic-ray.toml`:

```toml
[cosmic-ray]
module-path = "src"
timeout = 20.0
excluded-modules = ['src/ecdsa/_sha3.py', 'src/ecdsa/_version.py', 'src/ecdsa/test*']
test-command = "pytest --timeout 30 -x --fast -m 'not slow' src/"

[cosmic-ray.distributor]
name = "local"
```

```bash
cosmic-ray init cosmic-ray.toml session.sqlite
cosmic-ray exec cosmic-ray.toml session.sqlite
```

> **Note:** cosmic-ray runs mutants sequentially (one at a time). On this
> project it processes ~0.7 mutants/sec, so a full run of 15,356 mutants takes
> approximately 6 hours. We ran it for 10 minutes (420 mutants completed) and
> extrapolated.

## Results

### fest (plugin backend, 24 workers)

| Metric | Value |
|--------|-------|
| Source files scanned | 14 |
| Mutants generated | 4,290 |
| Mutants tested | 2,803 (1,383 had no coverage) |
| Killed | 2,401 (85.7%) |
| Survived | 314 (11.2%) |
| Timeout | 80 |
| Errors | 8 |
| **Wall time** | **4 min 6 sec** |
| Throughput | ~17.4 mutants/sec |

### cosmic-ray 8.4.4 (local distributor, sequential)

| Metric | Value |
|--------|-------|
| Mutants generated | 15,356 |
| Completed in 10 min | 420 (2.7%) |
| Killed | 316 (75.2%) |
| Survived | 104 (24.8%) |
| Timeout | 0 |
| Errors | 0 |
| **Wall time (10 min sample)** | **10 min → ~366 min estimated total** |
| Throughput | ~0.7 mutants/sec |

### Comparison

| | fest | cosmic-ray | Notes |
|---|---|---|---|
| **Throughput** | 17.4 mut/s | 0.7 mut/s | **~25x faster** |
| **Est. total time** | 4m 6s | ~6h 6m | fest completes while CR is at 2.7% |
| **Kill rate** | 85.7% | 75.2% | Different operator sets, not directly comparable |
| **Mutant count** | 4,290 | 15,356 | CR generates ~3.6x more mutants (213 operators vs 14) |

## Key differences

### Why fest is faster

1. **Parallel execution:** fest runs 24 test workers concurrently (one per CPU
   core). cosmic-ray's `local` distributor runs mutants sequentially.
2. **Coverage-guided test selection:** fest only runs tests that cover the
   mutated line, typically a small subset. cosmic-ray runs the full test suite
   for every mutant.
3. **In-process plugin backend:** fest's plugin backend patches modules in a
   long-lived pytest process, avoiding per-mutant process startup overhead.

### Why mutant counts differ

cosmic-ray has 213 built-in operators (mostly fine-grained binary operator
replacements like `Add→Sub`, `Add→Mul`, etc.). fest has 14 higher-level
operators (e.g., `arithmetic_op` replaces `+` with `-`, `*`, `/`, `//`). This
means cosmic-ray generates ~3.6x more mutants but many are redundant variants
of the same logical mutation.

### Why kill rates differ

The 10.5 percentage point difference is expected given the different operator
sets and the fact that cosmic-ray's 10-minute sample covered only 420 mutants
(possibly biased toward easier-to-kill early modules). fest's coverage-guided
approach also skips mutants in uncovered code, focusing testing effort on
reachable mutations.

## Caveats

- cosmic-ray supports distributed execution via Celery, which would improve
  throughput on multi-machine setups. This benchmark only tests local execution.
- fest's plugin backend has known limitations with some import patterns (e.g.,
  `from module import func` binds at import time and won't see in-process
  patches). The subprocess backend avoids this via in-place file mutation but
  is slower.
- The cosmic-ray result is extrapolated from a 10-minute sample (420/15,356
  mutants). Actual total time may vary due to per-module test time differences.
