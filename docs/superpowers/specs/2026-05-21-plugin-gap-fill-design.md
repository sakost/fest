# Plugin Backend Gap-Fill — Plan G2

**Date:** 2026-05-21
**Builds on:** [Plan G](2026-03-14-plugin-reference-fixup-design.md)
**Status:** Draft

## Problem

Plan G shipped the reverse-import index and the five-variant `MutationDiff` IR, lifting the pytest-plugin backend from ~10% to near-parity with subprocess accuracy. Eight known gaps remain. This spec closes seven of them — all of the "binding-shaped" gaps that Plan G's IR can absorb without re-executing module top-level code.

The gaps in scope:

1. **`Stmt::AnnAssign`** at module and class scope (`X: int = 5`, `class C: x: int = 5`) — currently falls through `derive_for_top_level`, producing an empty diff and an error.
2. **`Stmt::AugAssign`** at module scope (`COUNTER += 1`) — falls through.
3. **Tuple / list unpacking** (`a, b = 1, 2`) — only `targets.first()` was matched; multi-target assigns return empty diff.
4. **Chained assigns** (`a = b = 5`) — same root cause as tuple unpack.
5. **Nested class methods** (`class Outer: class Inner: def m(): ...`) — `derive_for_top_level` only descends one class level; mutations on `Inner.m` fall through.
6. **Mutations on bindable targets inside module-scope control-flow blocks** (`if PY311: X = 1` — mutate `1`) — `derive_for_top_level` only matches `Stmt::FunctionDef` / `Assign` / `ClassDef` at the top level.
7. **`from x import *` and aliased re-exports** — the reverse-import index currently records the `*` binding but never resolves it, so consumers of star-imported names don't receive mutation propagation.

Explicitly **out of scope** (deferred to future specs):

- Mutations to the *condition* of `if` / `while` / `for` / `try` — requires re-executing module top-level code under mutation, which replays side effects (logging setup, registry registrations, file I/O). The complexity and safety tradeoffs are large enough to deserve their own spec.
- Closure cell-layout migration when free-var lists change shape.
- C-extension consumers (Cython, `functools.lru_cache` C wrappers, anything holding a reference outside Python's import system).

## Solution overview

Three layered changes, no new modules:

1. **IR shrinks then grows**: `MutationDiff::ConstantBind { name, new_expr }` and `MutationDiff::ClassAttr { class_qualname, name, new_expr }` are deleted. Replaced by a single new variant:

   ```rust
   StatementBind {
       names: Vec<String>,
       stmt_source: String,
       scope: BindScope,
   }
   pub enum BindScope { Module, Class { qualname: String } }
   ```

   This variant absorbs `Stmt::Assign` (including tuple/multi-target), `Stmt::AnnAssign`, and `Stmt::AugAssign`. The Python plugin's apply path becomes a single `_PY_EXEC` against the target namespace, instead of two parallel `setattr`-style appliers.

2. **AST walker grows three small helpers**: `descend_class` (mirrors existing `descend_function` for arbitrarily nested classes), `walk_control_flow` (recurses into `If` / `Try` / `For` / `While` body branches), and `extract_assign_names` (lists names targeted by an assign-shaped statement; returns empty for non-`Name` targets like subscripts/attributes, which then produce empty diffs).

3. **Star-import resolution** moves from "ignored" to a two-step pipeline: at scan time, `plugin_index::resolve_star_imports` looks up the source module's static `__all__` literal and synthesizes named bindings; if `__all__` is dynamic or the source module is outside the project scan, the binding is marked `StarImportPending` and resolved by the plugin at worker startup via runtime import.

A fourth, orthogonal change tightens the outcome taxonomy from three states to four, splitting `error` into `skipped` (fest can't represent this mutation) and `error` (something actually broke). This applies retroactively to all of Plan G's "empty diff" paths too — they all become `skipped` instead of `error`.

## Architecture

The change touches three layers Plan G already established. No new files.

```
fest (Rust)                                  plugin (Python)
───────────                                  ──────────────
mutator → MutationDiff IR                    ReverseImportIndex
   │   (now: StatementBind absorbs              │   (now: resolves StarImport
   │    ConstantBind + ClassAttr +              │    at runtime if pending)
   │    AnnAssign + AugAssign + multi)          │
   ▼                                            ▼
plugin_index::resolve_star_imports        MutationApplier
   ├─ static __all__   → synthesize          │  (now: _apply_statement_bind
   │                     Named bindings      │   replaces both _apply_constant_bind
   └─ dynamic / unscanned → mark Pending     │   and _apply_class_attr)
                                                │
                                                ├─ FunctionPatch          (unchanged)
                                                ├─ StatementBindPatch     (NEW)
                                                ├─ ClassMethodPatch       (unchanged)
                                                ├─ ModuleAttrPatch        (unchanged)
                                                └─ PatchJournal           (unchanged)
```

## Components

### IR types (`src/mutation/diff.rs`)

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BindScope {
    Module,
    Class { qualname: String },  // dotted: "Outer" or "Outer.Inner"
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MutationDiff {
    FunctionBody  { qualname: String, new_source: String },
    StatementBind { names: Vec<String>, stmt_source: String, scope: BindScope },
    ClassMethod   { class_qualname: String, method_name: String, new_source: String },
    ModuleAttr    { name: String, new_source: String },
    // ConstantBind and ClassAttr deleted.
}
```

### AST walker extensions (`src/mutation/diff.rs`)

Three new functions:

```rust
fn descend_class(
    class_def: &StmtClassDef,
    mutated_source: &str,
    mutant: &Mutant,
    qualname_prefix: &str,
) -> Option<MutationDiff>;
// Mirrors descend_function. Builds dotted class_qualname for nested classes.

fn walk_control_flow(
    stmt: &Stmt,
    mutated_source: &str,
    mutant: &Mutant,
) -> Option<MutationDiff>;
// For Stmt::If: recurse into body and orelse.
// For Stmt::Try: recurse into body, handlers (ExceptHandler.body), orelse, finalbody.
// For Stmt::For: recurse into body and orelse.
// For Stmt::While: recurse into body and orelse.
// Each branch reuses derive_for_top_level matching.

fn extract_assign_names(stmt: &Stmt) -> Vec<String>;
// Stmt::Assign:    walk each target. Bare Name → push. Tuple/List of Names → push each.
//                  Subscript/Attribute/Starred/anything else → return empty Vec.
// Stmt::AnnAssign: target must be Name → push, else empty.
// Stmt::AugAssign: target must be Name → push, else empty.
// Other:           empty.
```

`derive_for_top_level` gains arms for `Stmt::AnnAssign` and `Stmt::AugAssign`, and delegates `Stmt::If` / `Try` / `For` / `While` to `walk_control_flow`. The existing `Stmt::Assign` arm is rewritten to use `extract_assign_names` and emit `StatementBind { names: ..., stmt_source: ..., scope: Module }`. The existing `Stmt::ClassDef` arm is rewritten to use `descend_class` and emit `StatementBind { scope: Class { qualname } }` for class-level assigns (the old `ClassAttr` path), or `ClassMethod` for methods, or recurse into nested classes.

### Import-index extension (`src/plugin_index.rs`)

```rust
pub struct ModuleScanResult {
    pub bindings: Vec<ImportBinding>,
    pub all_names: Option<Vec<String>>,  // NEW — static __all__ literal if present
    // ... existing fields
}

impl PluginIndex {
    pub fn resolve_star_imports(&mut self) {
        // For each StarImport binding, look up source module's all_names.
        // If present: synthesize one Named binding per name, drop the StarImport.
        // If absent: convert to StarImportPending (defer to runtime).
    }
}
```

`scan_source` is extended to detect `__all__ = [literal, ...]` at module top level (and inside `Stmt::If` to handle conditional `__all__` only when both branches are literal lists — anything else stays dynamic). The detection accepts list, tuple, and set literals of string literals; anything else falls back to dynamic.

Aliased re-exports (`from x import y as z`) should already work via `resolve_import_from`. The verification test in `tests/test_plugin_index.rs` confirms this; if it fails, the fix lands as part of this spec.

### Plugin applier (`src/plugin/_fest_plugin.py`)

`_apply_constant_bind` and `_apply_class_attr` are deleted. New method:

```python
def _apply_statement_bind(self, diff, journal):
    scope = diff["scope"]
    if scope["kind"] == "module":
        target_dict = sys.modules[diff["module"]].__dict__
        exec_globals = target_dict
        exec_locals  = target_dict
        for name in diff["names"]:
            prior = target_dict.get(name, _MISSING)
            journal.append(_restore_dict_slot, target_dict, name, prior)
    else:  # class
        target_class = self._resolve_class(diff["module"], scope["qualname"])
        if target_class is None:
            raise SkippedMutation(reason="missing_class_scope")
        exec_globals = sys.modules[diff["module"]].__dict__
        exec_locals  = {}  # populated by exec, then merged into target_class
        # Journal via _restore_class_attr — target_class.__dict__ is a
        # mappingproxy, so _restore_dict_slot cannot write to it.
        for name in diff["names"]:
            prior = target_class.__dict__.get(name, _MISSING)
            journal.append(_restore_class_attr, target_class, name, prior)

    try:
        _PY_EXEC(diff["stmt_source"], exec_globals, exec_locals)
    except SyntaxError as exc:
        raise ToolingError(reason="generated_syntax_error", cause=exc)
    # Other exceptions propagate up; the dispatcher classifies them as `killed`
    # (mutated code raises at import — that IS the test signal).

    if scope["kind"] == "class":
        # Merge exec_locals into the class proper (cannot exec directly into a
        # mappingproxy). setattr triggers __set_name__ etc., which we accept.
        for name, value in exec_locals.items():
            setattr(target_class, name, value)

    if scope["kind"] == "module":
        for name in diff["names"]:
            new_value = exec_globals[name]
            self.reverse_index.propagate(
                diff["module"], name, new_value, journal=journal
            )
    # Class-scope changes do not propagate via the reverse-import index
    # (consumers don't bind class attributes by name; they hold a reference
    # to the class itself, which is unchanged).
```

`ReverseImportIndex.build_runtime_layer` gains a branch for `StarImportPending`:

```python
def _resolve_pending_star_import(self, binding):
    try:
        mod = importlib.import_module(binding.source_module)
    except Exception:
        return []  # silently skip — propagation just won't reach this consumer
    names = getattr(mod, "__all__", None)
    if names is None:
        names = [n for n in vars(mod) if not n.startswith("_")]
    return [
        ImportBinding(
            source_module=binding.source_module,
            source_name=n,
            target_module=binding.target_module,
            local_name=n,
            kind="named",
        )
        for n in names
    ]
```

### Outcome taxonomy (`src/runner` + report writers)

The `MutantOutcome` enum gains a fourth variant:

```rust
pub enum MutantOutcome {
    Killed,
    Survived,
    Skipped { reason: SkipReason },  // NEW
    Error   { message: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    UnmappableTarget,         // subscript/attribute assignment target
    ConditionMutation,        // mutation in if/while/for test, not body
    UnsupportedStatement,     // statement type derive_for_top_level doesn't match
    MissingClassScope,        // class for Class-scoped StatementBind doesn't exist
}

// Note: star-import unresolvability and propagation-target-gone are NOT
// SkipReasons. They're propagation-side warnings — the mutant still applies
// to its source module. If incomplete propagation lets a mutant survive,
// the outcome is `Survived`, not `Skipped`. Warnings are logged separately.
```

Mutation score formula: `killed / (killed + survived)`. `skipped` and `error` are reported alongside but excluded from the denominator.

Report writers updated:
- `src/report/text.rs`: prints `Skipped: <count>` line; verbose mode breaks down by `SkipReason`.
- `src/report/json.rs`: aggregate gains `skipped` and `errors` counts plus a `skip_reasons: {<reason>: <count>}` map. Per-mutant entries can have `outcome: "skipped"` with a `reason` field.
- `src/report/html.rs`: new "Skipped" badge (desaturated yellow), one section per reason in the detail view.
- `bench/compare.sh`: extracts `Skipped` from output and prints in the summary table.

## Data flow

### Tuple-assign mutation inside an `if` block

Source: `if sys.version_info >= (3, 11): MAX_RETRIES, BACKOFF = 5, 0.5`
Mutant: arithmetic operator flips `0.5` → `0.0`.

```
Rust side
─────────
1. derive_diff iterates original_ast.body, finds Stmt::If containing offset.
2. walk_control_flow recurses into If.body, finds Stmt::Assign containing offset.
3. derive_for_top_level matches Stmt::Assign:
   - extract_assign_names → ["MAX_RETRIES", "BACKOFF"]
   - stmt_source = mutated_source[stmt.range()]
                 = "MAX_RETRIES, BACKOFF = 5, 0.0"
   - emits StatementBind { names, stmt_source, scope: Module }
4. IPC payload:
   {"kind": "statement_bind", "module": "config",
    "names": ["MAX_RETRIES", "BACKOFF"],
    "stmt_source": "MAX_RETRIES, BACKOFF = 5, 0.0",
    "scope": {"kind": "module"}}

Plugin side
───────────
5. _apply_statement_bind:
   - journal pre-state: MAX_RETRIES=5, BACKOFF=0.5
   - _PY_EXEC("MAX_RETRIES, BACKOFF = 5, 0.0", config.__dict__)
   - propagate "MAX_RETRIES" and "BACKOFF" through reverse-import index
6. pytest runs; test asserting BACKOFF > 0 fails → killed.
7. journal.rollback restores both names.
```

### Star-import resolution at scan + worker startup

Source: `api.py: from .models import *`; `models.py: __all__ = ["User", "Group"]`.

```
Rust scan (one-time)
────────────────────
1. scan_source(api.py): finds ImportFrom with Alias::*.
   Emits ImportBinding { kind: StarImport, source: "pkg.models", target: "pkg.api" }.
2. scan_source(models.py): finds Stmt::Assign(__all__ = ["User", "Group"]).
   ModuleScanResult.all_names = Some(vec!["User", "Group"]).
3. resolve_star_imports():
   - StarImport binding's source has all_names → synthesize:
       ImportBinding { kind: Named, source: "pkg.models", source_name: "User",
                       target: "pkg.api", local_name: "User" }
       ... and same for "Group"
   - Drop the StarImport.

Worker startup
──────────────
4. Handshake sends full ImportBinding list. Worker ingests as if they were
   explicit `from .models import User, Group`. No new code paths in apply.

Dynamic-__all__ fallback
────────────────────────
   If models.py defines __all__ = [n for n in dir() ...]:
   - all_names = None → StarImport stays as StarImportPending.
   - Worker startup runtime-resolves via importlib + getattr(mod, "__all__")
     or vars() fallback.
   - If runtime import raises: bindings synthesized = [], propagation silently
     skips this consumer. Mutants to star-imported names will appear as
     `survived` in this consumer (no way to deliver them).
```

## Error handling

### Outcome classification table

| Cause                                                           | Outcome    |
|-----------------------------------------------------------------|------------|
| `extract_assign_names` returns empty (subscript / attribute)    | `Skipped { UnmappableTarget }` |
| Mutation lands in `If.test` / `While.test`                      | `Skipped { ConditionMutation }` |
| Statement type not matched by `derive_for_top_level`            | `Skipped { UnsupportedStatement }` |
| Star-import source unscanned AND import fails at runtime        | warning logged; this propagation site is skipped; mutant outcome unchanged (may `Survive` if no other consumer kills it) |
| Class-scope `StatementBind` target class doesn't exist          | `Skipped { MissingClassScope }` |
| `_PY_EXEC` raises **non-`SyntaxError`** (ZeroDivisionError etc) | **`Killed`** — mutated code can't import, that IS the test signal |
| `_PY_EXEC` raises `SyntaxError`                                 | `Error { generated_syntax_error }` — fest bug |
| Journal rollback fails mid-restore                              | `Error { rollback_failed }` |
| Worker process crashes                                          | `Error { worker_crashed }` |
| IPC handshake / version mismatch                                | `Error` (existing path) |
| Reverse-index propagation target module unloaded mid-test       | warning logged; this propagation site is skipped; mutant outcome unchanged |

### Operator-facing summary format

```
Mutants generated: 1247
  Killed:    1089  (87.3%)
  Survived:   142
  Skipped:     14  (out-of-scope mutations — see --verbose for breakdown)
  Errors:       2  (tooling failures — please file a bug)

Mutation Score: 88.4%   [1089 / (1089 + 142)]
```

Verbose mode appends:

```
Skipped breakdown:
  condition_mutation:    11
  unmappable_target:      2
  missing_class_scope:    1
```

### Backward compatibility

This is a hard cut. `PROTOCOL_VERSION` in `pytest_plugin.rs` is bumped; plugin rejects mismatched handshakes. JSON report format gains `skipped` aggregate and `outcome: "skipped"` value — this is a breaking change for any external consumer of `--report json`, called out in CHANGELOG. fest is pre-1.0; the cut is allowed.

## Testing

### Rust unit tests (`src/mutation/diff.rs`)

| Test                                                              | Asserts                                                                  |
|-------------------------------------------------------------------|--------------------------------------------------------------------------|
| `module_scope_annotated_assign_yields_statement_bind`             | `X: int = 5` → `StatementBind { names: ["X"], scope: Module }`           |
| `class_scope_annotated_assign_yields_statement_bind_class`        | `class C: X: int = 5` → `scope: Class("C")`                              |
| `aug_assign_yields_statement_bind_with_full_stmt`                 | `COUNTER += 1` mutated → `stmt_source` is the full augmented assignment  |
| `tuple_unpack_yields_multi_name_bind`                             | `a, b = 1, 2` → `names: ["a", "b"]`                                      |
| `chained_assign_yields_multi_name_bind`                           | `a = b = 5` → multi-name list                                            |
| `subscript_target_yields_empty_diff`                              | `d[k] = v` → empty `Vec<MutationDiff>`                                   |
| `attribute_target_yields_empty_diff`                              | `obj.x = v` → empty `Vec<MutationDiff>`                                  |
| `nested_class_method_uses_dotted_qualname`                        | `class Outer: class Inner: def m()` → `class_qualname: "Outer.Inner"`    |
| `mutation_inside_if_body_recurses`                                | `if PY311: X = 1` (mutate `1`) → `StatementBind`                         |
| `mutation_inside_if_orelse_recurses`                              | `if cond: X=1\nelse: X=2` (mutate `2`)                                   |
| `mutation_inside_try_body_recurses`                               | `try: X=1 ...` (mutate `1`)                                              |
| `mutation_inside_try_handler_recurses`                            | `try: ... except E: X=1` (mutate `1`)                                    |
| `mutation_inside_for_body_recurses`                               | `for i in r: X=i` (mutate something in body)                             |
| `mutation_inside_while_body_recurses`                             | `while c: X=1` (mutate `1`)                                              |
| `mutation_in_if_test_yields_empty_diff`                           | mutation on the `if`'s condition → empty                                 |
| `existing_constant_bind_tests_migrated_to_statement_bind`         | all old `ConstantBind` cases pass with new variant                       |
| `existing_class_attr_tests_migrated_to_statement_bind`            | all old `ClassAttr` cases pass with new variant + `scope: Class`         |

### Rust unit tests (`src/plugin_index.rs`)

| Test                                                              | Asserts                                                                  |
|-------------------------------------------------------------------|--------------------------------------------------------------------------|
| `static_all_resolves_star_import_to_named`                        | Synthesize one Named binding per name in `__all__`                       |
| `missing_all_attribute_leaves_pending`                            | No `__all__` → `StarImportPending`                                       |
| `dynamic_all_leaves_pending`                                      | Non-literal `__all__` (list-comp, function call) → `StarImportPending`   |
| `star_import_to_unscanned_module_leaves_pending`                  | `from numpy import *` (numpy not in scan) → `StarImportPending`          |
| `aliased_reexport_resolves_correctly`                             | `from x import y as z` → `ImportBinding { source_name: "y", local_name: "z" }` |

### Python plugin tests (`tests/plugin/test_mutation_applier.py`)

| Test                                                                   | Asserts                                                                  |
|------------------------------------------------------------------------|--------------------------------------------------------------------------|
| `test_statement_bind_module_scope_single_name`                         | analogue of old `_apply_constant_bind` happy path                        |
| `test_statement_bind_module_scope_multi_name`                          | tuple unpack applies + journals both + rollback restores both            |
| `test_statement_bind_class_scope`                                      | exec runs against class dict; module dict unchanged                      |
| `test_statement_bind_with_missing_pre_state`                           | name was `_MISSING` before; after rollback, name not in target_dict      |
| `test_statement_bind_aug_assign_uses_current_value`                    | namespace has `X=5`; `_PY_EXEC("X += 2", ns)` → `X==7`; rollback → `X=5` |
| `test_statement_bind_exec_raises_non_syntaxerror_is_killed`            | `X = 1/0` → applier reports killed, journal rolls back cleanly           |
| `test_statement_bind_exec_raises_syntaxerror_is_error`                 | malformed source → applier reports error, journal rolls back             |

### Python plugin tests (`tests/plugin/test_reverse_import_index.py`)

| Test                                                                   | Asserts                                                                  |
|------------------------------------------------------------------------|--------------------------------------------------------------------------|
| `test_star_import_pending_resolves_at_runtime_with_all`                | runtime import + static `__all__` → bindings synthesized                 |
| `test_star_import_pending_resolves_at_runtime_without_all`             | no `__all__` → `vars(mod)` minus underscore-prefixed                     |
| `test_star_import_pending_runtime_resolution_failure_is_skip`          | import raises → no bindings, no crash, silent skip                       |

### Integration fixture (`tests/fixtures/control_flow_bindings/`)

```
control_flow_bindings/
├── pyproject.toml
├── conftest.py
├── src/
│   ├── config.py          # if PY311: MAX_RETRIES, BACKOFF = 5, 0.5
│   ├── annotated.py       # COUNTER: int = 0;  HEADERS: tuple = ("x",)
│   ├── nested_classes.py  # class Outer: class Inner: def m(): return 1
│   └── starred/
│       ├── __init__.py
│       ├── models.py      # __all__ = ["User"];  class User: ...
│       └── api.py         # from .models import *;  def whoami(): return User()
└── tests/
    ├── test_config.py
    ├── test_annotated.py
    ├── test_nested.py
    └── test_starred.py
```

Integration test (`#[ignore]`, gated on pytest availability) runs the plugin backend over this fixture and asserts: mutation score ≥ threshold, `error` count is zero, `skipped` count matches the expected condition-mutation count (zero by fixture design).

### Report-format tests

- `src/report/text.rs`: `Skipped` line appears iff count > 0; per-reason breakdown in verbose mode.
- `src/report/json.rs`: aggregate has `skipped` field; per-mutant entries with `outcome: "skipped"` have a `reason` field.
- `src/report/html.rs`: rendered HTML contains the skipped badge and per-reason detail.

### Excluded from scope

- Cosmic-ray output compatibility — not promised.
- Performance regression — `bench-mutation` (criterion) catches drift; no dedicated test.
- Cross-Python-version AST stability — `ruff_python_ast` 0.11.6 is pinned.

## Open questions

None. All branches above are decided.

## Out-of-scope work (future specs)

- Condition mutations in `if` / `while` / `for` / `try` via `ModuleReimport` IR — requires side-effect classification and journaling.
- Closure cell-layout migration.
- C-extension consumer rebinding.
