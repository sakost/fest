# Plugin Backend Reference Fixup

**Date:** 2026-03-14
**Issue:** [#8](https://github.com/sakost/fest/issues/8)
**Status:** Draft

## Problem

The plugin backend applies mutations by `exec()`-ing mutated source into a module's namespace. This creates **new** function/class/constant objects, but other modules that captured references via `from module import func` still hold the **old** objects. Mutations are invisible to those tests, producing ~10% mutation score vs ~90% with the subprocess backend.

## Solution: Hybrid `__code__` Patching + `gc.get_referrers()` Fixup

Two mechanisms, applied per changed name after `exec()`:

### Fast path: `__code__` swap (functions)

When both the old and new object for a name are `types.FunctionType`, swap the code and metadata **on the original object** so every existing reference sees the mutation:

```python
old_func.__code__ = new_func.__code__
old_func.__defaults__ = new_func.__defaults__
old_func.__kwdefaults__ = new_func.__kwdefaults__
```

Then put the **old** (now patched) object back in the module namespace, preserving identity.

This covers the vast majority of mutations (operators, return values, comparisons — all live inside function bodies).

### Slow path: `gc.get_referrers()` (everything else)

For non-function changes (constants, class-level attributes, new/deleted names), use CPython's GC to find all containers referencing the old object and replace in-place:

```python
import gc

def _fixup_references(old_obj: object, new_obj: object) -> None:
    if old_obj is new_obj:
        return
    for referrer in gc.get_referrers(old_obj):
        if isinstance(referrer, dict):
            for key, val in list(referrer.items()):
                if val is old_obj:
                    referrer[key] = new_obj
        elif isinstance(referrer, list):
            for i, val in enumerate(referrer):
                if val is old_obj:
                    referrer[i] = new_obj
        elif isinstance(referrer, set):
            referrer.discard(old_obj)
            referrer.add(new_obj)
        # Tuples, frames, cells — immutable or unsafe to patch, skip.
```

### Class handling

When both old and new are classes:

1. Walk methods: for each method present in both old and new class, apply `__code__` swap on the underlying function.
2. For changed/added/removed class-level attributes, call `_fixup_references()` for each.

## Updated `_handle_mutant` Flow

```
1. saved_dict = dict(vars(module))
2. Run compiled mutated source in module namespace
3. Diff saved_dict vs vars(module) — collect changed names
4. For each changed name:
   a. Both are FunctionType → __code__ swap, restore old object identity
   b. Both are type (classes) → __code__ swap on matching methods,
      gc.get_referrers() for changed class-level attributes
   c. Otherwise → gc.get_referrers(old_obj) to patch all mutable containers
5. Run tests
6. Restore:
   a. Reverse __code__ swaps (restore original code/defaults)
   b. Reverse gc.get_referrers() patches (swap new_obj back to old_obj)
   c. vars(module).clear(); vars(module).update(saved_dict)
```

## Restoration

Restoration must be exact — no leaked mutations between mutants. Strategy:

- **`__code__` swaps:** Save original `__code__`, `__defaults__`, `__kwdefaults__` before patching. Restore in the `finally` block.
- **`gc.get_referrers()` patches:** Call `_fixup_references(new_obj, old_obj)` (reverse direction) before restoring the module dict.
- **Module dict:** `clear()` + `update(saved_dict)` as today.

## Performance

| Path | Cost | When |
|------|------|------|
| `__code__` swap | O(changed_functions) | ~95%+ of mutations |
| `gc.get_referrers()` | O(gc_tracked_objects) per changed name | Constants, class attributes |
| Class method swap | O(methods_in_class) | Class body mutations |

The `gc.get_referrers()` call walks CPython's GC-tracked object list, which scales with total live objects. This is acceptable because:
- It only fires for non-function mutations (rare).
- Even in large projects, the GC walk completes in milliseconds.
- It runs at most once per changed non-function name per mutant.

## Scope

### In scope
- Function `__code__`/`__defaults__`/`__kwdefaults__` swapping
- Class method `__code__` swapping
- `gc.get_referrers()` fixup for dicts, lists, sets
- Full restoration after each mutant

### Out of scope
- Tuple contents (immutable — cannot be patched in-place)
- Frame locals (unsafe to modify)
- C extension internals (not introspectable)
- Objects not tracked by GC (some C types)

These edge cases are extremely unlikely in real test code and do not affect correctness in practice.

## Files Modified

- `src/plugin/_fest_plugin.py` — new `_fixup_references()`, `_swap_code()` helpers; updated `_handle_mutant()`

## Testing

- Unit test: function mutation with `from module import func` — verify test sees mutation
- Unit test: constant mutation with `from module import CONST` — verify test sees mutation
- Unit test: class method mutation — verify test sees mutation
- Unit test: restoration — verify no leaked state between mutants
- Integration: run fest on a project using `from` imports, compare plugin vs subprocess scores
