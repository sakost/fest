# Design: Persistent Pytest Worker Pool

## Problem

fest currently spawns a **new pytest process for every mutant**, even with the "plugin" backend. Despite the docstring claiming "single pytest worker process", each `run_mutant()` call creates a temp dir, binds a new socket, spawns `python -m pytest`, exchanges one mutant message, then kills the process. Additionally, the Python plugin calls `pytest.main()` per mutant inside the process — a full re-initialization (plugin loading, test collection, session setup) each time.

Two layers of waste:
1. **Process-level**: new OS process per mutant (~50-200ms overhead)
2. **pytest-level**: full `pytest.main()` re-initialization per mutant (~100-300ms overhead)

## Solution

Replace the spawn-per-mutant model with a **persistent worker pool**:

- **N long-lived pytest processes** (one per configured worker / rayon thread)
- Each process collects all tests **once** at startup
- For each mutant: patch module in-process, run specific test items via `runtestprotocol()`, restore module
- Workers stay alive for the entire mutation testing run
- Explicitly disable `pytest-xdist` via `-p no:xdist`

## Architecture

```
                     Rust (lib.rs)
                          |
              +-----------+-----------+
              |                       |
         start(N)                  stop()
              |                       |
    +---------+---------+     +-------+-------+
    |  WorkerPool (mpsc)|     | drain & SHUTDOWN
    +----+----+----+----+     +-------+-------+
         |    |    |
     Worker Worker Worker    (N persistent pytest processes)
         |    |    |
    [socket] [socket] [socket]

  rayon thread 1 ──→ borrow Worker from pool ──→ send MUTANT ──→ recv RESULT ──→ return Worker
  rayon thread 2 ──→ borrow Worker from pool ──→ ...
  rayon thread N ──→ ...
```

## Runner Trait Changes (`src/runner.rs`)

Add lifecycle methods with default no-ops:

```rust
pub trait Runner: Send + Sync {
    /// Spawn persistent workers. Default: no-op (for stateless runners).
    fn start(&self, _num_workers: usize)
        -> impl Future<Output = Result<(), Error>> + Send { async { Ok(()) } }

    /// Run the test suite against a single mutant.
    fn run_mutant(&self, mutant: &Mutant, source: &str, tests: &[String])
        -> impl Future<Output = Result<MutantResult, Error>> + Send;

    /// Shut down persistent workers. Default: no-op.
    fn stop(&self)
        -> impl Future<Output = Result<(), Error>> + Send { async { Ok(()) } }
}
```

`SubprocessRunner` gets free no-ops. `PytestPluginRunner` implements all three.

`AnyRunner` gains `start`/`stop` methods that delegate to the underlying variant.

## Python Plugin Changes (`src/plugin/_fest_plugin.py`)

### Hook restructure

- **Remove** `pytest_sessionstart` event loop
- **Add** `pytest_runtestloop(session)` — enters the event loop **after** collection is complete, returns `True` to tell pytest we handled execution
- Test item index built from `session.items` inside `pytest_runtestloop`

### Test execution

Replace `pytest.main()` with direct item execution:

```python
from _pytest.runner import runtestprotocol

def _run_tests(item_index, test_ids):
    items = [item_index[tid] for tid in test_ids if tid in item_index]
    if not items:
        return "survived"

    for i, item in enumerate(items):
        nextitem = items[i + 1] if i + 1 < len(items) else None
        # Re-initializes _request automatically for re-runs
        reports = runtestprotocol(item, log=False, nextitem=nextitem)
        for report in reports:
            if report.when in ("setup", "call") and report.failed:
                return "killed"
    return "survived"
```

### Key details

- `runtestprotocol()` handles `item._initrequest()` for re-runs automatically
- `log=False` prevents terminal reporter from accumulating counts
- `nextitem` passed correctly within batch; last item gets `None` to tear down fixtures
- Module patching (compile + exec + restore) stays the same

### Pytest version check

Since `runtestprotocol` is from `_pytest.runner` (internal API), the plugin checks `pytest.__version__` at startup and rejects unsupported versions. Supported range: `pytest >= 7.0, < 9`.

### Protocol change

READY message now includes collected test IDs:

```json
{"type": "ready", "tests": ["test_foo.py::test_bar", ...]}
```

MUTANT and RESULT messages unchanged. SHUTDOWN message unchanged.

## Rust Worker Pool (`src/runner/pytest_plugin.rs`)

### `PersistentWorker` struct

```rust
struct PersistentWorker {
    _temp_dir: tempfile::TempDir,
    child: tokio::process::Child,
    reader: BufReader<tokio::io::ReadHalf<tokio::net::UnixStream>>,
    writer: tokio::io::WriteHalf<tokio::net::UnixStream>,
}
```

Methods:
- `spawn(timeout) -> Result<Self>` — create temp dir, write plugin, bind socket, spawn pytest with `-p no:xdist`, accept connection, wait for READY
- `send_mutant(mutant, source, tests) -> Result<MutantStatus>` — send MUTANT, read RESULT
- `shutdown()` — send SHUTDOWN, kill child

### `WorkerPool` struct

```rust
struct WorkerPool {
    tx: tokio::sync::mpsc::UnboundedSender<PersistentWorker>,
    rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<PersistentWorker>>,
}
```

- N workers created in `start()`, all put into the channel
- `run_mutant()` does: `rx.lock().recv()` to borrow → use → `tx.send()` to return
- `stop()` drains channel, sends SHUTDOWN to each worker

### `PytestPluginRunner`

```rust
pub struct PytestPluginRunner {
    timeout: Duration,
    pool: tokio::sync::OnceCell<WorkerPool>,
}
```

- `start(num_workers)` initializes the `OnceCell` with a new `WorkerPool`
- `run_mutant()` delegates to pool (or errors if not started)
- `stop()` shuts down all workers

## Integration (`src/lib.rs`)

In `run_mutants()`:

```rust
let runner = runner::build_runner(&config.backend, config.timeout);

// Start persistent workers before the parallel loop.
ctx.runtime.block_on(runner.start(num_workers))?;

// ... existing rayon parallel loop (unchanged) ...

// Stop workers after the loop.
let _stop = ctx.runtime.block_on(runner.stop());
```

## xdist Prevention

Workers are spawned with `-p no:xdist` to explicitly disable the pytest-xdist plugin. This prevents xdist from spawning its own worker subprocesses regardless of project configuration.

## Error Handling

- **Worker crash during mutant execution**: socket read/write fails, `run_mutant` returns `Err(Error::Runner(...))`. Existing `AnyRunner::Plugin` fallback activates: sets `plugin_failed` atomic flag, all subsequent mutants use `SubprocessRunner`.
- **Worker spawn failure at startup**: `start()` returns `Err(Error::Runner(...))`. `AnyRunner` catches this and falls back to subprocess for the entire run.
- **Pytest version mismatch**: plugin sends error status in READY message, Rust treats as `Error::Runner`, triggers subprocess fallback.
- **Per-mutant timeout**: wraps socket send/recv same as current. Timed-out worker is killed and removed from pool.

## Files Changed

| File | Change |
|------|--------|
| `src/runner.rs` | Add `start`/`stop` to `Runner` trait with default no-ops; add `start`/`stop` to `AnyRunner` |
| `src/runner/pytest_plugin.rs` | Rewrite: `PersistentWorker`, `WorkerPool` with mpsc channel, lifecycle methods |
| `src/plugin/_fest_plugin.py` | Rewrite: `pytest_runtestloop` event loop, `runtestprotocol` per item, pytest version check |
| `src/lib.rs` | Call `runner.start(num_workers)` before and `runner.stop()` after parallel loop |
| `src/runner/subprocess.rs` | No changes (default lifecycle no-ops) |
| `README.md` | Pin supported pytest version range (`>= 7.0, < 9`) |

## Tests

### Python plugin (`_fest_plugin.py`)
- Not unit-testable in isolation (requires live pytest). Covered by integration tests.

### Rust (`pytest_plugin.rs`)
- `PersistentWorker::spawn` / `shutdown` lifecycle with mock Python script
- Socket protocol: mock plugin sends READY with tests, receives MUTANT, sends RESULT
- Worker pool: borrow/return cycle, concurrent access
- Timeout handling on persistent connection
- Worker crash detection (process exit mid-conversation)

### Integration
- End-to-end: real pytest project, persistent workers, multiple mutants through same worker
- Fallback: plugin failure triggers subprocess mode
- xdist disabled: verify no xdist worker processes spawned
