# Design: Cosmic-Ray Feature Parity

Date: 2026-03-05

Goal: Bring fest to feature parity with cosmic-ray's mutation operators and
mutation management capabilities, plus per-file configuration.

## 1. New Mutation Operators

### 1.1 break_continue

Swap `break` ↔ `continue` in loops. Each `break` generates one mutant
(`continue`) and vice versa.

Config key: `break_continue` (default: `true`).

### 1.2 unary_op

Replace unary operators:

- `-x` → `+x`, `-x` → `~x`
- `+x` → `-x`, `+x` → `~x`
- `~x` → `-x`, `~x` → `+x`
- Remove `-x` → `x`, `~x` → `x`
- `not` removal is already handled by `boolean_op` — not duplicated here.
- `+x` removal is skipped (equivalent to `x`).

Config key: `unary_op` (default: `true`).

### 1.3 zero_iteration_loop

Replace `for x in <expr>` with `for x in []`. Tests whether loop bodies
are exercised by the test suite. Only targets `for` loops, not `while`.

Config key: `zero_iteration_loop` (default: `true`).

### 1.4 variable_replace

Replace variable references on the RHS of assignments with a
deterministic pseudo-random constant in the range [-100, 100].

Requires `--seed` (default: 0). The per-site value is:
`hash(seed, file_path, byte_offset, "variable_replace") % 201 - 100`.

Config key: `variable_replace` (default: `false` — opt-in).

### 1.5 variable_insert

Inject a variable into arithmetic expressions by combining with a random
operator (`+`, `-`, `*`). Same seeded RNG approach. The operator is
selected by `hash(seed, file_path, byte_offset, "variable_insert") % 3`.

Config key: `variable_insert` (default: `false` — opt-in).

## 2. Seed Support

### CLI

```
fest run --seed 42
```

### Config

```toml
[fest]
seed = 42  # optional, default: 0
```

### Determinism

Each mutation site gets a stable value derived from:
`hash(global_seed, file_path, byte_offset, mutator_name)` using a fast
non-cryptographic hash (e.g., `ahash`).

Properties:
- Same seed + same code = identical mutants every run.
- Changes in one file do not affect mutants in other files.
- The seed is recorded in session DB and printed in reports.

### Report output

```
fest mutation testing report (seed: 42)
```

JSON and HTML reports include the seed as a top-level field.

## 3. Session Management

### Lifecycle

1. `fest run` — default ephemeral mode (no persistence, current behavior).
2. `fest run --session fest.db` — creates SQLite DB on first run. Stores
   all generated mutants as `pending`. Updates rows as results arrive.
3. Ctrl+C — current mutants finish; partial results are in the DB.
4. `fest run --session fest.db` — resumes: skips completed mutants, runs
   only `pending`. Coverage is re-collected.
5. `fest run --session fest.db --reset` — clears results, regenerates
   mutants.

### Schema

```sql
CREATE TABLE mutants (
    id INTEGER PRIMARY KEY,
    file_path TEXT NOT NULL,
    line INTEGER NOT NULL,
    column INTEGER NOT NULL,
    byte_offset INTEGER NOT NULL,
    byte_length INTEGER NOT NULL,
    original_text TEXT NOT NULL,
    mutated_text TEXT NOT NULL,
    mutator_name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    duration_ms INTEGER,
    error_message TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Stores: seed, last_run_at, fest_version, config_hash
```

### Incremental mode

`fest run --session fest.db --incremental` — after loading the session,
reset to `pending` any mutant whose source file has a newer mtime than
the session's `last_run_at` timestamp. Unchanged files keep their results.

## 4. Filtering

### 4.1 Operator filter

CLI: `--filter-operators '<pattern>[,<pattern>...]'`

Config:
```toml
[fest.filters]
operators = ["!variable_.*", "!remove_decorator"]
```

- Prefix `!` = exclude. No prefix = include.
- If any include patterns exist, only matching operators run.
- Exclude patterns take priority over include patterns.
- Patterns are regex-matched against operator names.

### 4.2 Path filter

CLI: `--filter-paths '<glob>'`

Restricts which already-discovered files get mutated. Stacks with
`source`/`exclude`. Useful for focusing a session on a subdirectory.

### 4.3 Line-level suppression (pragma)

```python
SECRET_KEY = "hardcoded"  # pragma: no mutate
x = 1 + 2  # pragma: no mutate(arithmetic_op)
```

- `# pragma: no mutate` — skip all mutations on this line.
- `# pragma: no mutate(<operator>)` — skip a specific operator.
- Checked during mutant generation by reading the source line.

### Filter evaluation order

operator filter → path filter → pragma → coverage filter (existing).
All applied during mutant generation, before test execution.

## 5. Per-file Configuration

### Config syntax

```toml
[fest]
source = ["src/**/*.py"]
timeout = 30

[fest.mutators]
arithmetic_op = true
variable_replace = false

[[fest.per-file]]
pattern = "src/generated/**"
mutators = { arithmetic_op = false, comparison_op = false }

[[fest.per-file]]
pattern = "src/auth/**"
timeout = 60
mutators = { variable_replace = true }

[[fest.per-file]]
pattern = "src/utils/compat.py"
skip = true
```

### Semantics

- Each `[[fest.per-file]]` block has a required `pattern` (glob) and
  optional overrides: `mutators`, `timeout`, `filters.operators`, `skip`.
- Overrides are **merged** with globals (not replaced). Setting
  `mutators = { arithmetic_op = false }` disables only that operator.
- Multiple blocks can match the same file. Evaluated top-to-bottom,
  **last match wins** per field (same as ruff).
- `skip = true` = generate zero mutants for this file.

### Implementation

During mutant generation, each discovered file resolves its effective
config by walking the `per-file` list, producing a `ResolvedFileConfig`
with merged mutator flags, timeout, and filter overrides.

## 6. Deferred

- **Custom operator plugins** — deferred to a future design.

## Priority

| Feature | Priority |
|---------|----------|
| `break_continue`, `unary_op`, `zero_iteration_loop` operators | High |
| `variable_replace`, `variable_insert` operators + seed | Medium |
| Session management (SQLite, stop/resume, `--reset`) | High |
| Incremental mode (`--incremental`) | Medium |
| Filtering (operator regex, path glob, pragma) | High |
| Per-file configuration (`[[fest.per-file]]`) | Medium |
