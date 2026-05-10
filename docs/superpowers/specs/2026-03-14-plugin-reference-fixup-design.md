# Plugin Backend Reference Fixup — Plan G

**Date:** 2026-05-10 (supersedes 2026-03-14 draft)
**Issue:** [#8](https://github.com/sakost/fest/issues/8)
**Status:** Draft

## Problem

The pytest-plugin backend is the fast path for fest: it keeps pytest's collection cost amortized across many mutants by mutating module state in-process between test runs. But mutations are applied by running the mutated module source into the target module's `__dict__`, which has three correctness gaps:

1. **`from module import name` bindings are not updated.** Consumers captured the *old* object at import time. Re-binding `target.__dict__["name"]` does not propagate. Real-world projects use `from` imports almost universally; this gap drops mutation score from ~90% (subprocess backend) to ~10% (current plugin backend).
2. **Module-level constants** (`MAX = 100`, `DEBUG = True`) are subject to the same problem, and the prior draft's `gc.get_referrers()` approach explicitly skips interned values — which means common literals (`100`, `True`, `False`, `""`, small ints) are silently un-tested in the plugin backend.
3. **Re-running the module body re-fires class-body side effects** — `__init_subclass__`, `__set_name__`, registry decorators. Projects using Django, SQLAlchemy, Pydantic, attrs, or any registry pattern will see incorrect or duplicated registrations after each mutant.

## Solution: Plan G

Replace the prior `gc.get_referrers()`-based approach with a **structured-diff IR** computed on the Rust side, dispatched to per-kind appliers in the plugin, recorded into a **patch journal** for exact rollback. No re-running of full module bodies. No GC walk.

Four mechanisms, applied per change kind:

| Change kind | Strategy | Identity preserved |
|---|---|---|
| Function body mutation | `__code__` swap on the existing function object | Yes — all unknown holders see the mutation |
| Module-level constant rebind (`MAX = 100` → `MAX = 101`) | Reverse-import index → rebind every consumer dict slot by name | N/A (primitive) |
| Class method body mutation | `__code__` swap on the unwrapped function (handles `staticmethod`, `classmethod`, `property`) | Yes |
| Module attribute rebind (decorator removed, class re-defined, etc.) | Reverse-import index → rebind consumer slots | No — rebind covers all *indexed* slots, but references held in lists / frame locals / closures are missed |

The reverse-import index is the key new primitive: a name-addressed mapping built once after pytest collection, queried per mutation. It replaces `gc.get_referrers()`'s content-addressed walk, eliminating both the interned-singleton blind spot and the per-mutant interpreter-wide scan.

## Architecture

```
fest (Rust)                              plugin (Python)
───────────                              ──────────────
mutator → MutationDiff IR                ReverseImportIndex (built once)
   │                                          │
   ▼                                          ▼
IPC: {"type": "mutant", "diff": [...]} ──→ MutationApplier
                                              │
                                              ├─ FunctionPatch
                                              ├─ ConstantRebind
                                              ├─ ClassMethodPatch
                                              ├─ ClassAttrRebind
                                              └─ ModuleAttrRebind
                                              │
                                              ▼
                                          PatchJournal (append-only undo log)
                                              │
                                              ▼
                                          run_tests(test_ids)
                                              │
                                              ▼
                                          Journal.rollback() (in finally)
                                              │
                                              ▼
                                          send result
```

## Components

### Rust side: `MutationDiff` IR

Computed in `src/mutation/diff.rs`. Walks the original and mutated AST in lockstep; for each top-level statement that differs, emits one of:

```rust
pub enum MutationDiff {
    /// A function body changed. `new_source` is the **raw `def` block
    /// without decorators** — decorators are stripped on the Rust side.
    /// The plugin compiles the def, then `__code__`-swaps onto the
    /// existing function, drilling through `functools.wraps` chains via
    /// `__wrapped__` if the target is a decorator-wrapped callable.
    /// Decorator changes themselves are routed through `ModuleAttr`,
    /// not `FunctionBody`.
    ///
    /// **Nested functions:** if `qualname` contains a dot
    /// (e.g. `"outer.inner"`), the target is a closure body. The plugin
    /// resolves `outer` via `qualname[:rfind('.')]`, then walks
    /// `outer.__code__.co_consts` to find the `CodeType` whose
    /// `co_name` matches the leaf component. It builds a new code
    /// object via `outer.__code__.replace(co_consts=…)` with the
    /// inner code swapped, then `__code__`-swaps `outer`. Subsequent
    /// calls to `outer()` produce a fresh nested function with the
    /// mutated body. **Cell-count mismatches** (mutations that change
    /// the set of free variables) still fall back to identity-breaking
    /// rebind — this is genuinely rare for the operator-style mutators
    /// fest applies.
    FunctionBody { qualname: String, new_source: String },

    /// A module-level `NAME = expr` binding changed value.
    /// `new_expr` is the source text of the right-hand side, evaluated
    /// in the target module's namespace at apply time.
    ConstantBind { name: String, new_expr: String },

    /// A method on a class changed body. `class_qualname` is the
    /// dotted path within the module (handles nested classes).
    ClassMethod {
        class_qualname: String,
        method_name: String,
        new_source: String,
    },

    /// A class-level attribute (non-method) changed.
    ClassAttr {
        class_qualname: String,
        name: String,
        new_expr: String,
    },

    /// A module-level binding that needs full statement-mode
    /// compilation — e.g., decorator removal on a function (the new
    /// source is a `def` block without the decorator) or a class
    /// re-definition. The plugin compiles `new_source` in `"exec"`
    /// mode, runs it in a fresh namespace seeded with the target
    /// module's globals, and pulls out the binding named `name`.
    ///
    /// **Note:** for class re-definitions, the class body is re-run,
    /// which re-fires `__init_subclass__` and `__set_name__` hooks.
    /// This is a documented limitation; it only affects
    /// decorator-on-class mutations, which are rare.
    ModuleAttr { name: String, new_source: String },
}
```

The IR is what crosses the IPC boundary. The plugin never sees full mutated module source. This eliminates the metaclass / `__init_subclass__` re-fire problem at the source: we never re-execute a class body.

Each fest `Mutation` may yield 1 or more `MutationDiff` entries (typically 1; a change spanning multiple top-level statements yields multiple).

### Python side: `ReverseImportIndex`

Built once after pytest collection completes (in `pytest_runtestloop`). Two layers:

**Layer 1 — runtime introspection.** For every module in `sys.modules`, walk its `__dict__`. For each value `v`:

```python
src_mod = getattr(v, "__module__", None)
src_name = getattr(v, "__qualname__", None) or getattr(v, "__name__", None)
if src_mod and src_name and src_mod != mod.__name__:
    index[(src_mod, src_name)].append((mod.__dict__, key))
```

This catches `from X import Y` and `from X import Y as Z` (because `v.__qualname__` is still `"Y"` regardless of the alias). It works for functions, classes, and any object that exposes `__module__` / `__qualname__`.

**Layer 2 — Rust-side AST scan, shipped to plugin in handshake.** fest already depends on `ruff_python_ast`. Before spawning pytest, fest walks every project `.py` file once and emits a flat list of import bindings:

```rust
struct ImportBinding {
    consumer_module: String,   // e.g. "myproj.consumers.foo"
    consumer_key: String,      // local name in consumer (alias if any)
    target_module: String,     // resolved absolute module name
    target_name: String,       // imported name in the target
}
```

The list is sent in the `ready` handshake message:

```json
{ "type": "ready_ack",
  "import_bindings": [ { "consumer_module": "...", ... }, ... ],
  "reload_warnings": [ { "file": "...", "line": 42 } ] }
```

The plugin merges layer 1 (built from `sys.modules` at runtime) with layer 2 (received over IPC) into a single dict keyed by `(target_module, target_name)`. At apply time, `consumer_module` is resolved to a `__dict__` via `sys.modules`. If the consumer module isn't loaded (dead code), the entry is silently skipped.

**Why Rust-side rather than in the plugin:**

- `ruff_python_ast` is already a fest dependency — no new deps for the plugin
- ~10× faster than Python `ast.parse` on large codebases
- Same parser used for the `MutationDiff` derivation → no behavioral drift between layers
- Plugin stays pure-Python, no compiled-extension distribution headache

This layer covers raw constants whose values lack `__module__` (ints, strings, bools). The two layers compose: layer 1 is precise per-object; layer 2 is precise per-name. Layer 2 is authoritative when both produce entries (it knows aliases without ambiguity).

The index is keyed by `(target_module_name, target_attr_name)`. Lookup is O(consumers of that name) — typically <10 even in large projects.

### Rust side: `importlib.reload` detection

During the same AST walk that produces import bindings, the Rust side also collects calls that compromise plugin-backend accuracy:

- `importlib.reload(...)` — reloads target's source, undoing the mutation
- `importlib.import_module(...)` and `__import__(...)` — dynamic imports invisible to layer 2
- `exec(...)` / `compile(...)` calls in user code — out-of-band code execution

Hits are reported in the `ready_ack` handshake's `reload_warnings` list. The plugin logs a one-time warning at startup of the form:

```
fest: detected importlib.reload at myproj/tests/test_x.py:42 — plugin
backend cannot guarantee accuracy for tests that reload mutated modules.
Consider --backend=subprocess for these tests.
```

This is **upfront feedback**, not a runtime trap: the user sees it before the run starts and can switch backends or adjust tests. We do not monkey-patch `importlib.reload`; doing so would break legitimate test setups.

### Python side: `MutationApplier`

One method per `MutationDiff` variant. Each applies the change *and* appends an inverse to the journal.

**`FunctionBody`:**

```python
def apply_function_body(self, change, journal):
    if "." in change.qualname:
        # Nested function: patch parent's co_consts in place.
        parent_qualname, leaf = change.qualname.rsplit(".", 1)
        parent = self._resolve_qualname(parent_qualname)
        parent = _drill_to_function(parent)
        new_inner = self._compile_function(change.new_source, parent)
        old_consts = parent.__code__.co_consts
        new_consts = tuple(
            new_inner.__code__ if (isinstance(c, types.CodeType)
                                   and c.co_name == leaf)
            else c
            for c in old_consts
        )
        old_parent_code = parent.__code__
        parent.__code__ = old_parent_code.replace(co_consts=new_consts)
        journal.append(_restore_code, parent, old_parent_code)
        return

    wrapped = self._resolve_qualname(change.qualname)
    target_func = _drill_to_function(wrapped)  # follow __wrapped__ chains
    new_func = self._compile_function(change.new_source, target_func)
    try:
        old_code = target_func.__code__
        old_defaults = target_func.__defaults__
        old_kwdefaults = target_func.__kwdefaults__
        old_annotations = dict(target_func.__annotations__)
        old_func_dict = dict(target_func.__dict__)

        target_func.__code__ = new_func.__code__
        target_func.__defaults__ = new_func.__defaults__
        target_func.__kwdefaults__ = new_func.__kwdefaults__
        target_func.__annotations__ = dict(new_func.__annotations__)
        target_func.__dict__.clear()
        target_func.__dict__.update(new_func.__dict__)

        journal.append(_restore_function, target_func, old_code,
                       old_defaults, old_kwdefaults, old_annotations,
                       old_func_dict)
    except ValueError:
        # Closure-cell count mismatch. Fall back to ModuleAttr-style
        # rebind: replace the function object outright via the
        # reverse-import index.
        self.apply_module_attr_rebind(change.qualname, new_func, journal)
```

`_compile_function` compiles the new source as a single `def`, runs it in a fresh namespace seeded with the target function's `__globals__`, and pulls the resulting function out. This preserves the closure cell *names* (not values — values are still bound at runtime by the original closure).

**`ConstantRebind`:**

```python
def apply_constant_rebind(self, change, journal):
    new_value = self._eval_expression(
        change.new_expr, self.target_module.__dict__,
    )
    old_value = self.target_module.__dict__.get(change.name, _MISSING)
    self.target_module.__dict__[change.name] = new_value
    journal.append(_restore_dict_slot, self.target_module.__dict__,
                   change.name, old_value)

    for consumer_dict, consumer_key in self.index.lookup(
        self.target_module.__name__, change.name,
    ):
        old = consumer_dict.get(consumer_key, _MISSING)
        consumer_dict[consumer_key] = new_value
        journal.append(_restore_dict_slot, consumer_dict, consumer_key, old)
```

`_eval_expression` compiles `new_expr` in `"eval"` mode and evaluates it against the target module's globals. The `_MISSING` sentinel distinguishes "key was absent" from "key was None"; the restore function deletes the key when restoring `_MISSING`.

**`ClassMethod`:**

```python
def apply_class_method(self, change, journal):
    cls = self._resolve_qualname(change.class_qualname)
    descriptor = cls.__dict__[change.method_name]
    target_func = self._unwrap_descriptor(descriptor)
    if target_func is None:
        # property with fget=None or other unusual case — route through
        # ClassAttrRebind: the descriptor itself is replaced rather than
        # patched in place.
        self.apply_class_attr_rebind(
            ClassAttrRebind(
                class_qualname=change.class_qualname,
                name=change.method_name,
                new_value_factory=lambda: self._compile_descriptor(change),
            ),
            journal,
        )
        return
    new_func = self._compile_function(change.new_source, target_func)
    self._swap_code(target_func, new_func, journal)
```

`_unwrap_descriptor` returns the function for `staticmethod` / `classmethod` (via `__func__`), the appropriate `fget` / `fset` / `fdel` for `property` (the IR carries which one was mutated as part of `method_name`, e.g. `"prop.fget"`), or the descriptor itself for plain functions. Mutations on `property` accessors are emitted by Rust as separate `ClassMethod` entries with method names `"<prop>.fget"` etc.

**`ClassAttrRebind`:**

Like `ConstantRebind` but the slot is `cls.__dict__[name]`. Reverse-import index entries for `from module import MyClass` only point at `MyClass`, not at `MyClass.attr`, so we rebind only the class's slot. Direct re-imports of class attributes are out of scope. Documented limitation.

**`ModuleAttrRebind`:**

Same shape as `ConstantRebind` but the new value is computed by running a small wrapper that produces it (e.g., for class re-definition or decorator changes). The wrapper is the **only** code re-run; it does not include other module-level statements, so registry side effects do not re-fire.

### Python side: `PatchJournal`

```python
class PatchJournal:
    def __init__(self):
        self._entries: list[tuple[Callable, tuple]] = []

    def append(self, undo_fn, *args):
        self._entries.append((undo_fn, args))

    def rollback(self):
        errors = []
        for undo_fn, args in reversed(self._entries):
            try:
                undo_fn(*args)
            except Exception as exc:  # noqa: BLE001
                errors.append(exc)
        self._entries.clear()
        return errors  # returned, not raised — caller decides
```

Append-only. `rollback()` runs in reverse, swallowing exceptions but returning them so the caller can log. Partial-rollback failure is not catastrophic: each entry undoes one change in isolation, so a single failure leaves *that one* change in place but doesn't compromise the others.

## Data flow

Single mutant lifecycle in the plugin:

```python
def _handle_mutant(self, msg):
    diff = msg["diff"]  # list of MutationDiff serialised as JSON
    journal = PatchJournal()
    try:
        for change in diff:
            self.applier.apply(change, journal)
        status = self._run_tests(msg["tests"])
        return {"type": "result", "status": status}
    except CompileError as exc:
        return {"type": "result", "status": "error",
                "error_message": f"compile error: {exc}"}
    except Exception as exc:  # noqa: BLE001
        return {"type": "result", "status": "error",
                "error_message": f"runtime error: {exc}"}
    finally:
        errors = journal.rollback()
        for err in errors:
            log.warning("rollback step failed: %s", err)
```

No `dict(vars(mod))` snapshot. No GC walk. No re-running of full module body.

## Error handling

| Failure | Behavior |
|---|---|
| `compile()` raises on a mutated function/expression | Result `error`, message `"compile error: ..."`. Journal is empty (nothing applied), no rollback work. |
| `__code__` assignment raises `ValueError` (closure-cell mismatch) | Per-applier fallback: replace the function object via reverse-import-index rebind. Journal records the fallback path. |
| Reverse-import-index lookup misses (consumer not indexed) | Mutation still affects the target module's own dict; the missing consumer is uncovered. Logged at debug level. **This is the only accuracy gap remaining**, and it is strictly smaller than the prior spec's gaps. |
| Test framework raises mid-test | Captured by `runtestprotocol` as failure → mutation killed (correct behavior). |
| Rollback step raises | Caught, returned in error list, logged as warning. Remaining rollback entries still run. |

## Thread safety

CPython provides no safe way to terminate a Python thread from outside — `_async_raise(thread_id, SystemExit)` via ctypes is undefined behavior in C extensions, and there is no `Thread.terminate()`. So Plan G does not kill user threads. Instead it offers two layers:

**Detection (always on).** Before applying each mutant's diff, the plugin checks `threading.active_count()`. If more than the main thread is alive, it emits a one-time warning per fest run:

```
fest: detected N active threads at mutant boundary; tests using
threads must clean them up in teardown for accurate plugin-backend
results, or use --backend=subprocess. The first occurrence is at
<test_nodeid>.
```

The mutant is still applied. Race conditions between the user's thread reading module state and the mutation/rollback cycle are accepted as a known limitation for users who chose the plugin backend with active threads.

**Cooperative cleanup (opt-in fixture).** The plugin exposes a session-scoped pytest fixture, `fest_thread_cleanup`, that the user can request to register cleanup callbacks:

```python
def test_with_workers(fest_thread_cleanup):
    pool = ThreadPoolExecutor(max_workers=4)
    fest_thread_cleanup(pool.shutdown, wait=True)
    ...  # use pool
```

The plugin invokes registered callbacks before each mutation is applied and clears the registry after rollback. Callbacks that raise are logged but do not abort the mutant. This is purely opt-in — tests that don't request the fixture see only the detection-and-warn behavior.

**Future option** (not in this spec): per-mutant subprocess fallback when `active_count() > 1`. The infrastructure already exists in the subprocess backend; routing one mutant through it is a small extension. Defer until we see real-world demand.

## Performance

| Operation | Cost | Frequency |
|---|---|---|
| Build `ReverseImportIndex` (runtime layer) | O(loaded modules × avg dict size) | Once per fest run |
| Build `ReverseImportIndex` (AST layer) | O(project `.py` files × avg file size) | Once per fest run |
| `apply()` per change | O(consumers of name) for rebinds, O(1) for `__code__` swap | Once per mutant per change |
| `rollback()` | O(journal entries) | Once per mutant |
| `runtestprotocol` | dominated by user test | Once per mutant per test |

Index build cost on a 1000-file project: estimated 200-500ms (Python `ast.parse` is the dominant cost). Per-mutant cost: dominated by test execution; index lookup is O(<10 consumers per name) and runs in microseconds.

If profiling later shows the AST scan is on the critical path for huge codebases, it can move to a Rust cdylib helper using `ruff_python_ast` — fest already depends on it, and the per-file parse drops by ~10×. This is a follow-up optimization, not part of the initial implementation.

## Known limitations

- **CPython-only.** `__code__` assignment is a CPython implementation detail. PyPy supports `func.__code__ = ...` but the bytecode formats differ — code compiled by CPython will not run on PyPy. fest currently spawns whatever Python the user has on PATH and the plugin backend assumes CPython. PyPy users fall back to the subprocess backend transparently.
- **`from module import MyClass; MyClass.attr` rebinding** for class-level attribute mutations: the index doesn't track attribute-level access. Rare in practice; documented.
- **Dynamic imports** (`importlib.import_module`, `__import__`): the AST layer doesn't track *what* gets imported (only that a dynamic call exists), so dynamically imported names are not in the reverse-import index. The Rust-side AST scan emits a `reload_warnings` entry for each occurrence so the user sees the affected file at startup. The runtime layer catches the binding *if* the dynamic import has already executed by pytest collection time.
- **`importlib.reload(target)` from user code**: would reload the original (unmutated) source mid-test, undoing the mutation. The Rust-side AST scan flags occurrences in `reload_warnings`. Not monkey-patched at runtime, since reload is a legitimate operation in some test setups.
- **Closure-cell *count* mismatch**: when a mutation changes which outer names a nested function captures (i.e., `co_freevars` size changes), `__code__.replace()` cannot patch in place. Falls back to identity-breaking rebind. This requires a mutator that adds or removes a reference to an enclosing-scope name, which fest's operator-style mutators do not produce. Body-only mutations on nested functions (the common case) are now patched in place via parent-`co_consts` swap and **do** preserve identity.
- **C-extension internals**: not introspectable; same as before.
- **Thread safety**: handled by detection + optional cooperative cleanup, not by killing threads (CPython provides no safe thread termination — see the `Thread safety` section below).

## Files modified

- `src/mutation/mutator.rs` — produce `MutationDiff` alongside `Mutation`
- `src/mutation/diff.rs` — **new** — `MutationDiff` enum + AST-walk derivation
- `src/plugin_index.rs` — **new** — Rust-side `ImportBinding` scan over project source using `ruff_python_ast`; collects `reload_warnings` for `importlib.reload` / dynamic imports
- `src/runner/pytest_plugin.rs` — IPC payload carries `MutationDiff`; `ready_ack` carries `import_bindings` and `reload_warnings`
- `src/plugin/_fest_plugin.py` — full rewrite of `_handle_mutant`; new `ReverseImportIndex` (consumes Rust handshake), `MutationApplier`, `PatchJournal` classes; `fest_thread_cleanup` fixture; thread-detection warning
- `tests/fixtures/from_imports/` — **new** — fixture project exercising `from X import` patterns
- `tests/fixtures/registry_classes/` — **new** — fixture exercising `__init_subclass__` / `__set_name__`
- `tests/fixtures/nested_closures/` — **new** — fixture exercising nested-function mutations

## Testing

**Plugin unit tests (Python):**

- `from module import func` → mutate func body → consumer test sees mutation
- `from module import CONST` for primitive values (`int`, `bool`, `None`, `str`) → consumer sees mutation via AST-layer rebind
- `from module import CONST as ALIAS` → alias slot rebound, original `CONST` slot untouched in consumer (it doesn't exist)
- Function with closure-cell mismatch → fallback path used, consumer sees mutation
- Class method mutations: plain, `staticmethod`, `classmethod`, `property.fget` / `fset` → instance behavior changes
- Class with `__init_subclass__` → hook does not re-fire across mutants (registry size stable)
- Class with `__set_name__` descriptor → descriptor not re-bound across mutants
- `PatchJournal.rollback()` correctness: apply, rollback, apply same name again → no leaked state
- `PatchJournal.rollback()` resilience: undo step raises → remaining entries still run, errors returned
- `ReverseImportIndex` precision: AST-layer entry for `from X import Y as Z` points at the right consumer slot
- Nested function body mutation (`outer.inner`) → consumer of `outer` sees new `inner` body on next call; identity of `outer` preserved
- Active-thread detection: spawn a thread before mutant, verify warning emitted (only first occurrence)
- `fest_thread_cleanup` fixture: registered callback runs before mutation apply, exceptions in callbacks are logged not propagated

**Rust unit tests:**

- `MutationDiff` IR derivation from old/new AST: verify each variant emitted, verify byte-range mutation maps to correct IR variant
- IPC serialization roundtrip (JSON → IR → JSON)
- Fall-through cases: byte-range mutation that crosses IR boundaries (multi-statement) yields multiple diff entries
- `ImportBinding` scan: `from X import Y`, `from X import Y as Z`, `from .relative import Y`, multi-name imports — all produce correct entries
- `reload_warnings` scan: detects `importlib.reload`, `importlib.import_module`, `__import__` calls with file/line accuracy

**Integration tests:**

- Run plugin backend on a `from`-import-heavy project (target: a stripped-down Flask-like fixture). Compare mutation score against subprocess backend. **Acceptance: parity within 1% on non-edge-case mutations.**
- Run plugin backend on a registry-pattern project (declarative-base fixture). Verify registry size and identity stable across mutants.
- Run `just test` — full existing suite must continue to pass.

**Coverage gate:** existing 95% line-coverage gate applies. New Python plugin code must be covered by the unit tests above.
