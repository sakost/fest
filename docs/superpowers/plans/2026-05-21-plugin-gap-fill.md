# Plugin Gap-Fill (Plan G2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close seven binding-shaped coverage gaps in the pytest-plugin backend and replace `MutationDiff::ConstantBind` + `MutationDiff::ClassAttr` with a single generalized `StatementBind` variant; split the `Killed/Survived/Error` mutant outcome model into `Killed/Survived/Skipped/Error`.

**Architecture:** A small generalization in `src/mutation/diff.rs` (one new IR variant, three new walker helpers) absorbs `AnnAssign` / `AugAssign` / multi-target assigns / nested classes / control-flow-block bodies. A small extension in `src/plugin_index.rs` resolves `from x import *` against scanned `__all__` literals (with runtime fallback). The plugin's `MutationApplier` swaps two parallel appliers for one unified `_apply_statement_bind` that execs the full statement against the target namespace.

**Tech Stack:** Rust 2024 edition (1.93 toolchain), `ruff_python_ast` 0.11.6 for AST work, `serde` for IPC payloads, Python 3.11+ for the plugin runtime, pytest as the test driver.

**Spec:** [`docs/superpowers/specs/2026-05-21-plugin-gap-fill-design.md`](../specs/2026-05-21-plugin-gap-fill-design.md)

---

## File map

| File | Action | Responsibility |
|---|---|---|
| `src/mutation/mutant.rs` | modify | Extend `MutantStatus` enum with `Skipped { reason }` + add `SkipReason` enum. |
| `src/mutation/diff.rs` | modify | Replace `ConstantBind` + `ClassAttr` with `StatementBind { names, stmt_source, scope }`; add `BindScope` enum; add `descend_class`, `walk_control_flow`, `extract_assign_names` helpers; extend `derive_for_top_level` arms. |
| `src/plugin_index.rs` | modify | Add `pending_star_imports: Vec<PendingStarImport>` field; add `resolve_star_imports`; detect `__all__` literals in `scan_source`. |
| `src/plugin/_fest_plugin.py` | modify | Replace `_apply_constant_rebind` + `_apply_class_attr` with `_apply_statement_bind`; add `_resolve_pending_star_import` to `ReverseImportIndex`; add SyntaxError → error / other-exception → killed classification; consume `pending_star_imports` from ready_ack. |
| `src/runner/pytest_plugin.rs` | modify | Add `PROTOCOL_VERSION` constant + handshake check; emit `pending_star_imports` in ready_ack; route empty-diff into `MutantStatus::Skipped { UnsupportedStatement }`. |
| `src/runner/subprocess.rs` | modify | Adapt to new `MutantStatus::Skipped` variant in match arms (subprocess backend doesn't emit Skipped but compiles need it). |
| `src/report.rs` + `src/report/text.rs` + `src/report/json.rs` + `src/report/html.rs` | modify | Render `Skipped` count + per-reason breakdown; update score formula. |
| `bench/compare.sh` | modify | Extract `Skipped:` count from fest output. |
| `tests/fixtures/control_flow_bindings/` | create | New end-to-end fixture: AnnAssign, tuple unpack inside `if`, nested classes, star imports. |
| `tests/plugin/test_mutation_applier.py` | modify | Add tests for `_apply_statement_bind` (module + class scope, multi-name, AugAssign, SyntaxError, raise → killed). |
| `tests/plugin/test_reverse_import_index.py` | modify | Add tests for `_resolve_pending_star_import` (with __all__, without, import-fails). |

---

## Task 1: Add `MutantStatus::Skipped` variant + `SkipReason` enum

**Why first:** every later task needs to emit or pattern-match `Skipped`. Doing it first means the rest of the work compiles continuously.

**Files:**
- Modify: `src/mutation/mutant.rs:62-75`
- Modify: `src/runner/subprocess.rs:143-165` (match arms)
- Modify: `src/session.rs:519-531` (status string serialization)
- Modify: `src/report/text.rs`, `src/report.rs` test cases

- [ ] **Step 1: Add the failing test in `src/mutation/mutant.rs`** (add to existing `#[cfg(test)] mod tests`)

```rust
#[test]
fn skipped_status_with_reason_serializes_to_json() {
    use serde_json::json;
    let status = MutantStatus::Skipped {
        reason: SkipReason::ConditionMutation,
    };
    let v = serde_json::to_value(&status).unwrap();
    assert_eq!(v, json!({"Skipped": {"reason": "condition_mutation"}}));
}

#[test]
fn skipped_status_round_trips_through_serde() {
    let status = MutantStatus::Skipped { reason: SkipReason::UnmappableTarget };
    let json = serde_json::to_string(&status).unwrap();
    let back: MutantStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(back, status);
}
```

- [ ] **Step 2: Run tests, expect FAIL** — `SkipReason` doesn't exist yet.

```bash
cargo test --all-features -p fest mutant::tests::skipped_status
```

Expected: `error[E0433]: failed to resolve: use of undeclared type 'SkipReason'`.

- [ ] **Step 3: Implement enum + variant**

In `src/mutation/mutant.rs`, just above the `MutantStatus` enum (line 62), add:

```rust
/// Why a mutant was skipped (not run through tests). Reported alongside the
/// mutation score but excluded from the denominator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// AST target was not a bare `Name` (e.g. `d[k] = v`, `obj.x = v`).
    UnmappableTarget,
    /// Mutation landed on the test expression of `if`/`while`/`for` — out of scope.
    ConditionMutation,
    /// Statement kind not matched by any `derive_for_top_level` arm.
    UnsupportedStatement,
    /// Class qualname referenced by a class-scoped `StatementBind` did not resolve.
    MissingClassScope,
}
```

Add `Deserialize` to the `MutantStatus` derive (it currently only has `Serialize`), and add the variant:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutantStatus {
    Killed,
    Survived,
    Timeout,
    NoCoverage,
    /// fest couldn't represent or deliver this mutation. Not a tooling failure.
    Skipped { reason: SkipReason },
    Error(String),
}
```

- [ ] **Step 4: Make existing match arms exhaustive**

```bash
cargo check --all-features 2>&1 | grep -E "non-exhaustive|missing match"
```

Expected: errors in `src/session.rs` and `src/report/text.rs`. Add `MutantStatus::Skipped { .. }` arms to every match. Default behavior in reports: count separately, exclude from killed/survived; in session, treat as a completed mutant (similar to `Survived`). Concretely:

In `src/session.rs:519-531` (the status string mapping), add:
```rust
MutantStatus::Skipped { .. } => "skipped",
```
And in the reverse-direction parser (line 530):
```rust
"skipped" => MutantStatus::Skipped { reason: SkipReason::UnsupportedStatement },
```
*(Default reason on parse is fine — the parser is for session resumption and we don't persist the reason yet. Add a TODO comment? No — accept the lossy parse; this is rare path.)*

In `src/session.rs:250` (the "is completed" check):
```rust
MutantStatus::Killed | MutantStatus::Survived | MutantStatus::Skipped { .. }
```

- [ ] **Step 5: Run tests, expect PASS**

```bash
cargo test --all-features mutant::tests::skipped_status
cargo check --all-features
```

Expected: both pass.

- [ ] **Step 6: Commit**

```bash
git add src/mutation/mutant.rs src/session.rs src/report/text.rs src/runner/
git commit -m "feat(outcome): add MutantStatus::Skipped variant with SkipReason"
```

---

## Task 2: Add `BindScope` + `StatementBind` IR variant (no derivation yet)

Add the new IR types alongside the existing ones. No derive_diff changes yet — that comes in Tasks 3–8. Keep `ConstantBind` and `ClassAttr` for now; they're deleted later in the same sequence that migrates their callers.

**Files:**
- Modify: `src/mutation/diff.rs:1-55`

- [ ] **Step 1: Add the failing test in `src/mutation/diff.rs`** (existing `#[cfg(test)] mod tests`)

```rust
#[test]
fn statement_bind_serializes_with_scope_tag() {
    use serde_json::json;
    let diff = MutationDiff::StatementBind {
        names: vec!["X".to_owned(), "Y".to_owned()],
        stmt_source: "X, Y = 1, 2".to_owned(),
        scope: BindScope::Module,
    };
    let v = serde_json::to_value(&diff).unwrap();
    assert_eq!(v, json!({
        "kind": "statement_bind",
        "names": ["X", "Y"],
        "stmt_source": "X, Y = 1, 2",
        "scope": {"kind": "module"},
    }));
}

#[test]
fn statement_bind_class_scope_includes_qualname() {
    use serde_json::json;
    let diff = MutationDiff::StatementBind {
        names: vec!["counter".to_owned()],
        stmt_source: "counter: int = 5".to_owned(),
        scope: BindScope::Class { qualname: "Outer.Inner".to_owned() },
    };
    let v = serde_json::to_value(&diff).unwrap();
    assert_eq!(v["scope"], json!({"kind": "class", "qualname": "Outer.Inner"}));
}
```

- [ ] **Step 2: Run tests, expect FAIL**

```bash
cargo test --all-features mutation::diff::tests::statement_bind
```

Expected: `BindScope` / `StatementBind` undefined.

- [ ] **Step 3: Add `BindScope` enum and `StatementBind` variant in `src/mutation/diff.rs`**

Above the `MutationDiff` enum:

```rust
/// Scope in which a `StatementBind` should be exec'd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BindScope {
    /// Bind into the target module's `__dict__`.
    Module,
    /// Bind into a class's namespace. `qualname` is dotted (e.g. `Outer.Inner`).
    Class { qualname: String },
}
```

Inside `MutationDiff`, immediately after `ConstantBind { ... }`, add:

```rust
    /// Module- or class-scope statement-level binding change.
    /// Subsumes `Stmt::Assign`, `Stmt::AnnAssign`, and `Stmt::AugAssign`.
    StatementBind {
        /// Names assigned by this statement. May have one entry (bare assign)
        /// or many (tuple unpack, chained `a = b = ...`).
        names: Vec<String>,
        /// Full mutated statement source; exec'd against the target namespace.
        stmt_source: String,
        /// Target namespace.
        scope: BindScope,
    },
```

- [ ] **Step 4: Run tests, expect PASS**

```bash
cargo test --all-features mutation::diff::tests::statement_bind
```

- [ ] **Step 5: Commit**

```bash
git add src/mutation/diff.rs
git commit -m "feat(diff): add StatementBind variant and BindScope enum"
```

---

## Task 3: Add `extract_assign_names` helper + migrate `Stmt::Assign` to `StatementBind`

Replace the existing `Stmt::Assign` arm so simple `X = 5` mutations now produce `StatementBind` instead of `ConstantBind`. Tuple/chained targets are NOT yet supported — that's Task 7. This keeps the change small and tests precise.

**Files:**
- Modify: `src/mutation/diff.rs:188-205` (existing `Stmt::Assign` arm)
- Modify: `src/plugin/_fest_plugin.py:218-221, 309-322` (dispatch + applier)
- Modify: `src/runner/pytest_plugin.rs` test fixtures that reference `constant_bind`

- [ ] **Step 1: Add failing test for bare `X = 5` emitting `StatementBind`**

In `src/mutation/diff.rs` test module:

```rust
#[test]
fn bare_assign_emits_statement_bind_module_scope() {
    let original = "X = 5\n";
    let mutated  = "X = 6\n";
    let module = parse_module_helper(original);
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 1,
        column: 5,
        byte_offset: 4,            // position of "5"
        byte_length: 1,
        original_text: "5".into(),
        mutated_text: "6".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    assert_eq!(diff, vec![MutationDiff::StatementBind {
        names: vec!["X".to_owned()],
        stmt_source: "X = 6".to_owned(),
        scope: BindScope::Module,
    }]);
}
```

*(Use whatever `parse_module_helper` shape already exists in the diff.rs test module — there are existing tests that show the helper pattern.)*

- [ ] **Step 2: Run, expect FAIL** — still emits `ConstantBind`.

```bash
cargo test --all-features mutation::diff::tests::bare_assign_emits_statement_bind
```

- [ ] **Step 3: Add `extract_assign_names` and rewrite the `Stmt::Assign` arm**

Above `derive_for_top_level` in `src/mutation/diff.rs`:

```rust
/// Extract bound names from an assign-shaped statement.
///
/// Returns an empty `Vec` when any target is non-`Name` (subscript, attribute,
/// tuple containing a non-Name, starred unpack). Callers treat empty as a
/// signal to fall through derivation (the mutation is unmappable).
#[allow(clippy::single_match, reason = "more arms added in Task 7")]
fn extract_assign_names(stmt: &Stmt) -> Vec<String> {
    use ruff_python_ast::Expr;
    let mut out = Vec::new();
    match stmt {
        Stmt::Assign(assign) => {
            // Single bare-Name target only for now. Tuple/chained handled in Task 7.
            let Some(Expr::Name(n)) = assign.targets.first() else { return Vec::new() };
            if assign.targets.len() != 1 { return Vec::new(); }
            out.push(n.id.to_string());
        }
        _ => {}
    }
    out
}
```

Replace the `Stmt::Assign(assign) => { ... ConstantBind ... }` arm in `derive_for_top_level` (`src/mutation/diff.rs:190-205`):

```rust
        Stmt::Assign(_) => {
            let names = extract_assign_names(stmt);
            if names.is_empty() { return None; }
            let stmt_end = mutated_stmt_end(stmt.range().end().to_usize(), mutant);
            let stmt_start = stmt.range().start().to_usize();
            let stmt_source = mutated_source.get(stmt_start..stmt_end).unwrap_or("").to_owned();
            Some(MutationDiff::StatementBind {
                names,
                stmt_source,
                scope: BindScope::Module,
            })
        }
```

- [ ] **Step 4: Update the existing `constant_bind` plugin test fixtures**

In `src/runner/pytest_plugin.rs` tests (search for `"constant_bind"`), replace the JSON shape with `"statement_bind"` + new fields. *Specific lines depend on what the build surfaces.*

- [ ] **Step 5: Update the plugin applier (Python side)**

In `src/plugin/_fest_plugin.py:218-221`, change the dispatch:

```python
self._dispatch = {
    "function_body": self._apply_function_body,
    "statement_bind": self._apply_statement_bind,   # was "constant_bind"
    "class_method":  self._apply_class_method,
    "class_attr":    self._apply_class_attr,        # still used by Task 8 until deleted
    "module_attr":   self._apply_module_attr,
}
```

Replace `_apply_constant_rebind` (lines 309-322) with the new method:

```python
def _apply_statement_bind(
    self, change: dict[str, Any], journal: PatchJournal,
) -> None:
    scope = change["scope"]
    if scope["kind"] == "module":
        target_dict = self.target_module.__dict__
        # Journal pre-state for every name BEFORE exec'ing the mutation.
        for name in change["names"]:
            prior = target_dict.get(name, _MISSING)
            journal.append(_restore_dict_slot, target_dict, name, prior)
        compiled = compile(change["stmt_source"], "<fest statement>", "exec")
        _PY_EXEC(compiled, target_dict)
        # Propagate every name through the reverse-import index.
        for name in change["names"]:
            new_value = target_dict[name]
            for consumer_dict, consumer_key in self.index.lookup(
                self.target_module.__name__, name,
            ):
                old_consumer = consumer_dict.get(consumer_key, _MISSING)
                consumer_dict[consumer_key] = new_value
                journal.append(
                    _restore_dict_slot, consumer_dict, consumer_key, old_consumer,
                )
    else:
        # Class scope: handled in Task 9. For now, raise so it's visible.
        raise NotImplementedError("class-scope StatementBind: see Task 9")
```

- [ ] **Step 6: Update existing tests**

In `tests/plugin/test_mutation_applier.py`, find tests for `_apply_constant_rebind` and:
- Rename them to `test_statement_bind_*`
- Change `"kind": "constant_bind"` → `"kind": "statement_bind"`, `"name": "X"` → `"names": ["X"]`, `"new_expr": "6"` → `"stmt_source": "X = 6", "scope": {"kind": "module"}`

- [ ] **Step 7: Delete `ConstantBind` variant**

From `src/mutation/diff.rs:18-24`, remove the `ConstantBind { name, new_expr }` arm.

```bash
cargo check --all-features 2>&1 | grep -E "ConstantBind"
```

Expected: zero hits.

- [ ] **Step 8: Run tests, expect PASS**

```bash
cargo test --all-features mutation::diff
just test-plugin
```

- [ ] **Step 9: Commit**

```bash
git add src/mutation/diff.rs src/plugin/_fest_plugin.py src/runner/pytest_plugin.rs tests/plugin/test_mutation_applier.py
git commit -m "refactor(diff): migrate Stmt::Assign to StatementBind, delete ConstantBind"
```

---

## Task 4: Add `Stmt::AnnAssign` arm

**Files:**
- Modify: `src/mutation/diff.rs` (test + extract_assign_names + derive_for_top_level)

- [ ] **Step 1: Failing test**

```rust
#[test]
fn module_scope_annotated_assign_yields_statement_bind() {
    let src = "X: int = 5\n";
    let mutated = "X: int = 6\n";
    let module = parse_module_helper(src);
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 1, column: 9,
        byte_offset: 9, byte_length: 1,
        original_text: "5".into(), mutated_text: "6".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    assert_eq!(diff, vec![MutationDiff::StatementBind {
        names: vec!["X".to_owned()],
        stmt_source: "X: int = 6".to_owned(),
        scope: BindScope::Module,
    }]);
}
```

- [ ] **Step 2: Run, expect FAIL** — `AnnAssign` not matched.

- [ ] **Step 3: Extend `extract_assign_names` and `derive_for_top_level`**

In `extract_assign_names`:

```rust
        Stmt::AnnAssign(ann) => {
            if let Expr::Name(n) = &*ann.target {
                // Only count as bound if the statement actually assigns
                // (annotation-only like `X: int` has value = None).
                if ann.value.is_some() {
                    out.push(n.id.to_string());
                }
            }
        }
```

In `derive_for_top_level` (add a new arm next to `Stmt::Assign`):

```rust
        Stmt::AnnAssign(ann) => {
            if ann.value.is_none() { return None; }   // annotation only, no value to mutate
            let names = extract_assign_names(stmt);
            if names.is_empty() { return None; }
            let stmt_end = mutated_stmt_end(stmt.range().end().to_usize(), mutant);
            let stmt_start = stmt.range().start().to_usize();
            let stmt_source = mutated_source.get(stmt_start..stmt_end).unwrap_or("").to_owned();
            Some(MutationDiff::StatementBind {
                names, stmt_source, scope: BindScope::Module,
            })
        }
```

- [ ] **Step 4: Run, expect PASS** + commit.

```bash
cargo test --all-features mutation::diff::tests::module_scope_annotated_assign
git add src/mutation/diff.rs
git commit -m "feat(diff): handle Stmt::AnnAssign at module scope"
```

---

## Task 5: Add `Stmt::AugAssign` arm

**Files:**
- Modify: `src/mutation/diff.rs`
- Modify: `tests/plugin/test_mutation_applier.py` (add `test_statement_bind_aug_assign_uses_current_value`)

- [ ] **Step 1: Failing test in `diff.rs`**

```rust
#[test]
fn aug_assign_yields_statement_bind_with_full_stmt() {
    let src = "COUNTER = 0\nCOUNTER += 1\n";
    let mutated = "COUNTER = 0\nCOUNTER += 2\n";
    let module = parse_module_helper(src);
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 2, column: 11,
        byte_offset: 23, byte_length: 1,        // position of `1` in `+= 1`
        original_text: "1".into(), mutated_text: "2".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    assert_eq!(diff, vec![MutationDiff::StatementBind {
        names: vec!["COUNTER".to_owned()],
        stmt_source: "COUNTER += 2".to_owned(),
        scope: BindScope::Module,
    }]);
}
```

- [ ] **Step 2: Failing test in `tests/plugin/test_mutation_applier.py`**

```python
def test_statement_bind_aug_assign_uses_current_value(applier_setup):
    applier, module, journal = applier_setup
    module.COUNTER = 5
    change = {
        "kind": "statement_bind",
        "names": ["COUNTER"],
        "stmt_source": "COUNTER += 2",
        "scope": {"kind": "module"},
    }
    applier._apply_statement_bind(change, journal)
    assert module.COUNTER == 7         # 5 + 2, reading current namespace
    journal.rollback()
    assert module.COUNTER == 5
```

- [ ] **Step 3: Run both, expect FAIL.**

- [ ] **Step 4: Implement**

In `extract_assign_names`:

```rust
        Stmt::AugAssign(aug) => {
            if let Expr::Name(n) = &*aug.target {
                out.push(n.id.to_string());
            }
        }
```

In `derive_for_top_level`:

```rust
        Stmt::AugAssign(_) => {
            let names = extract_assign_names(stmt);
            if names.is_empty() { return None; }
            let stmt_end = mutated_stmt_end(stmt.range().end().to_usize(), mutant);
            let stmt_start = stmt.range().start().to_usize();
            let stmt_source = mutated_source.get(stmt_start..stmt_end).unwrap_or("").to_owned();
            Some(MutationDiff::StatementBind {
                names, stmt_source, scope: BindScope::Module,
            })
        }
```

The plugin side already exec's the full statement against the namespace, so AugAssign works for free — that's the point of using `exec` rather than `eval`-and-bind.

- [ ] **Step 5: Run, expect PASS** + commit.

```bash
cargo test --all-features mutation::diff
just test-plugin
git add src/mutation/diff.rs tests/plugin/test_mutation_applier.py
git commit -m "feat(diff): handle Stmt::AugAssign and verify exec semantics"
```

---

## Task 6: Reject non-bindable targets explicitly

Make subscript / attribute target mutations produce empty diffs (which are later routed to `Skipped { UnmappableTarget }` in Task 14). This is mostly a test-only task because `extract_assign_names` already returns empty for non-`Name` targets.

**Files:**
- Modify: `src/mutation/diff.rs` (tests only)

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn subscript_target_yields_empty_diff() {
    let src = "d = {}\nd['k'] = 1\n";
    let module = parse_module_helper(src);
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 2, column: 9,
        byte_offset: 16, byte_length: 1,
        original_text: "1".into(), mutated_text: "2".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, "d = {}\nd['k'] = 2\n");
    assert!(diff.is_empty(), "expected unmappable subscript to yield empty diff");
}

#[test]
fn attribute_target_yields_empty_diff() {
    let src = "class C: pass\nobj = C()\nobj.x = 1\n";
    let module = parse_module_helper(src);
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 3, column: 9,
        byte_offset: 33, byte_length: 1,    // recompute as needed
        original_text: "1".into(), mutated_text: "2".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, "class C: pass\nobj = C()\nobj.x = 2\n");
    assert!(diff.is_empty(), "expected unmappable attribute to yield empty diff");
}
```

- [ ] **Step 2: Run, expect PASS already** (because `extract_assign_names` already filters non-`Name`). If they fail, fix `extract_assign_names`.

- [ ] **Step 3: Commit**

```bash
git add src/mutation/diff.rs
git commit -m "test(diff): confirm subscript/attribute targets produce empty diffs"
```

---

## Task 7: Tuple unpack + chained targets

**Files:**
- Modify: `src/mutation/diff.rs` (extract_assign_names tuple/list/chained branches)
- Modify: `tests/plugin/test_mutation_applier.py` (multi-name applier test)

- [ ] **Step 1: Failing tests in `diff.rs`**

```rust
#[test]
fn tuple_unpack_yields_multi_name_bind() {
    let src = "a, b = 1, 2\n";
    let mutated = "a, b = 1, 3\n";
    let module = parse_module_helper(src);
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 1, column: 10,
        byte_offset: 10, byte_length: 1,
        original_text: "2".into(), mutated_text: "3".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    assert_eq!(diff, vec![MutationDiff::StatementBind {
        names: vec!["a".to_owned(), "b".to_owned()],
        stmt_source: "a, b = 1, 3".to_owned(),
        scope: BindScope::Module,
    }]);
}

#[test]
fn chained_assign_yields_multi_name_bind() {
    let src = "a = b = 5\n";
    let mutated = "a = b = 6\n";
    let module = parse_module_helper(src);
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 1, column: 8,
        byte_offset: 8, byte_length: 1,
        original_text: "5".into(), mutated_text: "6".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    assert_eq!(diff, vec![MutationDiff::StatementBind {
        names: vec!["a".to_owned(), "b".to_owned()],
        stmt_source: "a = b = 6".to_owned(),
        scope: BindScope::Module,
    }]);
}

#[test]
fn tuple_with_starred_target_yields_empty_diff() {
    let src = "*a, b = 1, 2, 3\n";
    let module = parse_module_helper(src);
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 1, column: 14,
        byte_offset: 14, byte_length: 1,
        original_text: "3".into(), mutated_text: "4".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, "*a, b = 1, 2, 4\n");
    assert!(diff.is_empty(), "starred unpack target is unmappable");
}
```

- [ ] **Step 2: Failing applier test**

```python
def test_statement_bind_multi_name(applier_setup):
    applier, module, journal = applier_setup
    module.a = 99   # pre-state to verify journal restores
    change = {
        "kind": "statement_bind",
        "names": ["a", "b"],
        "stmt_source": "a, b = 1, 3",
        "scope": {"kind": "module"},
    }
    applier._apply_statement_bind(change, journal)
    assert module.a == 1 and module.b == 3
    journal.rollback()
    assert module.a == 99
    assert not hasattr(module, "b")    # b was _MISSING pre-mutation
```

- [ ] **Step 3: Run, expect FAIL.**

- [ ] **Step 4: Extend `extract_assign_names` (tuple/list + chained targets)**

Rewrite `Stmt::Assign(assign)` arm to walk every target:

```rust
        Stmt::Assign(assign) => {
            // Multi-target chains: a = b = 5 → assign.targets = [a, b]
            // Tuple/list unpack: a, b = ... → assign.targets[0] = Tuple([a, b])
            // Mixing both is allowed: a = (b, c) = ... — handled by recursion.
            for target in &assign.targets {
                if !collect_names_from_target(target, &mut out) {
                    return Vec::new();   // bail on first non-bindable target
                }
            }
        }
```

Add a helper above `extract_assign_names`:

```rust
/// Collect bare names from a Python assignment target. Returns `false` if
/// the target contains any non-bindable form (subscript, attribute, starred).
fn collect_names_from_target(expr: &Expr, out: &mut Vec<String>) -> bool {
    use ruff_python_ast::Expr;
    match expr {
        Expr::Name(n) => { out.push(n.id.to_string()); true }
        Expr::Tuple(t) => t.elts.iter().all(|e| collect_names_from_target(e, out)),
        Expr::List(l)  => l.elts.iter().all(|e| collect_names_from_target(e, out)),
        // Subscript, Attribute, Starred, anything else: not bindable.
        _ => false,
    }
}
```

- [ ] **Step 5: Run, expect PASS** + commit.

```bash
cargo test --all-features mutation::diff
just test-plugin
git add src/mutation/diff.rs tests/plugin/test_mutation_applier.py
git commit -m "feat(diff): support tuple unpack and chained assign targets"
```

---

## Task 8: Migrate `ClassAttr` to `StatementBind` (class-scope IR)

Rewrite the `Stmt::ClassDef` arm to emit `StatementBind { scope: Class { qualname } }` for class-level assigns. Delete the `ClassAttr` variant.

**Files:**
- Modify: `src/mutation/diff.rs:206-256` (Stmt::ClassDef arm + delete ClassAttr)
- Modify: `src/plugin/_fest_plugin.py` (dispatch: remove `class_attr`)

- [ ] **Step 1: Failing test**

```rust
#[test]
fn class_scope_assign_yields_statement_bind_class() {
    let src = "class C:\n    X = 5\n";
    let mutated = "class C:\n    X = 6\n";
    let module = parse_module_helper(src);
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 2, column: 9,
        byte_offset: 17, byte_length: 1,    // adjust per layout
        original_text: "5".into(), mutated_text: "6".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    assert_eq!(diff, vec![MutationDiff::StatementBind {
        names: vec!["X".to_owned()],
        stmt_source: "X = 6".to_owned(),
        scope: BindScope::Class { qualname: "C".to_owned() },
    }]);
}

#[test]
fn class_scope_annotated_assign_yields_statement_bind_class() {
    let src = "class C:\n    X: int = 5\n";
    let mutated = "class C:\n    X: int = 6\n";
    let module = parse_module_helper(src);
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 2, column: 14,
        byte_offset: 22, byte_length: 1,    // adjust
        original_text: "5".into(), mutated_text: "6".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    assert_eq!(diff, vec![MutationDiff::StatementBind {
        names: vec!["X".to_owned()],
        stmt_source: "X: int = 6".to_owned(),
        scope: BindScope::Class { qualname: "C".to_owned() },
    }]);
}
```

- [ ] **Step 2: Run, expect FAIL** — still emits `ClassAttr` or nothing for AnnAssign.

- [ ] **Step 3: Rewrite the `Stmt::ClassDef` arm**

In `derive_for_top_level`, find the `Stmt::ClassDef(class) => {...}` arm (around lines 206-256) and replace it:

```rust
        Stmt::ClassDef(class) => derive_for_class_body(class, mutant, mutated_source, ""),
```

Add helper function next to `descend_function`:

```rust
fn derive_for_class_body(
    class: &ruff_python_ast::StmtClassDef,
    mutant: &Mutant,
    mutated_source: &str,
    qualname_prefix: &str,
) -> Option<MutationDiff> {
    let class_qualname = if qualname_prefix.is_empty() {
        class.name.id.to_string()
    } else {
        format!("{}.{}", qualname_prefix, class.name.id)
    };
    for body_stmt in &class.body {
        let r = body_stmt.range();
        if !(r.start().to_usize() <= mutant.byte_offset
            && mutant.byte_offset < r.end().to_usize()) {
            continue;
        }
        match body_stmt {
            Stmt::FunctionDef(method) => {
                // ClassMethod path (unchanged Plan G behavior).
                let suffix = property_suffix(method);
                let method_name = if suffix.is_empty() {
                    method.name.id.to_string()
                } else {
                    format!("{}.{}", method.name.id, suffix)
                };
                let stmt_end = mutated_stmt_end(r.end().to_usize(), mutant);
                let new_source =
                    strip_decorators(mutated_source.get(r.start().to_usize()..stmt_end).unwrap_or(""));
                return Some(MutationDiff::ClassMethod {
                    class_qualname,
                    method_name,
                    new_source,
                });
            }
            Stmt::Assign(_) | Stmt::AnnAssign(_) | Stmt::AugAssign(_) => {
                let names = extract_assign_names(body_stmt);
                if names.is_empty() { return None; }
                if let Stmt::AnnAssign(ann) = body_stmt {
                    if ann.value.is_none() { return None; }
                }
                let stmt_end = mutated_stmt_end(r.end().to_usize(), mutant);
                let stmt_source = mutated_source
                    .get(r.start().to_usize()..stmt_end).unwrap_or("").to_owned();
                return Some(MutationDiff::StatementBind {
                    names, stmt_source,
                    scope: BindScope::Class { qualname: class_qualname.clone() },
                });
            }
            Stmt::ClassDef(_) => {
                // Nested class: handled in Task 9.
                return None;
            }
            _ => return None,
        }
    }
    None
}
```

- [ ] **Step 4: Delete `ClassAttr` variant**

From `src/mutation/diff.rs:37-45`, remove the `ClassAttr { class_qualname, name, new_expr }` arm. From `src/plugin/_fest_plugin.py:221`, remove `"class_attr": self._apply_class_attr,`. From the same file, delete the `_apply_class_attr` method (lines 360-367).

- [ ] **Step 5: Migrate existing ClassAttr applier tests**

In `tests/plugin/test_mutation_applier.py`, find tests with `"kind": "class_attr"` and:
- Rename: `test_apply_class_attr_*` → `test_statement_bind_class_scope_*`
- Update payload: `{"kind": "class_attr", "class_qualname": "C", "name": "X", "new_expr": "6"}` → `{"kind": "statement_bind", "names": ["X"], "stmt_source": "X = 6", "scope": {"kind": "class", "qualname": "C"}}`

- [ ] **Step 6: Run all tests, expect FAIL** for plugin class-scope tests (because applier doesn't handle class scope yet — that's Task 9).

- [ ] **Step 7: Commit just the Rust + IR + dispatch wiring**

```bash
cargo test --all-features mutation::diff
git add src/mutation/diff.rs src/plugin/_fest_plugin.py tests/plugin/test_mutation_applier.py
git commit -m "refactor(diff): migrate ClassAttr to StatementBind class scope"
```

---

## Task 9: Plugin applier — class-scope `StatementBind`

Implement the class-scope branch of `_apply_statement_bind` so the Task 8 plugin tests pass.

**Files:**
- Modify: `src/plugin/_fest_plugin.py` (`_apply_statement_bind` class branch)

- [ ] **Step 1: Replace the `NotImplementedError` raise from Task 3**

Find `_apply_statement_bind` and replace the `else` branch:

```python
    else:
        # Class scope.
        target_class = self._resolve_qualname(scope["qualname"])
        if target_class is None or not isinstance(target_class, type):
            raise _SkippedMutation(reason="missing_class_scope")
        module_globals = self.target_module.__dict__
        # Class body executes with module dict as globals, fresh class
        # namespace as locals. We seed locals with current class attrs so
        # AugAssign / annotated assigns can read prior values.
        class_ns: dict[str, Any] = dict(target_class.__dict__)
        # Journal pre-state BEFORE the mutation.
        for name in change["names"]:
            prior = target_class.__dict__.get(name, _MISSING)
            journal.append(_restore_class_attr, target_class, name, prior)
        compiled = compile(change["stmt_source"], "<fest statement>", "exec")
        _PY_EXEC(compiled, module_globals, class_ns)
        # Apply each named binding back onto the class. setattr fires
        # __set_name__ / __init_subclass__ side effects — that's intentional
        # and matches the original class-body semantics.
        for name in change["names"]:
            if name in class_ns:
                setattr(target_class, name, class_ns[name])
        # Class-scope mutations do not propagate via reverse-import index:
        # consumers bind the class itself, not its attributes by name.
```

Add the `_SkippedMutation` exception class near the top of the module:

```python
class _SkippedMutation(Exception):
    """Raised by an applier when a mutation should be marked Skipped.

    Carries a ``reason`` string consumed by the dispatch loop.
    """
    def __init__(self, reason: str) -> None:
        super().__init__(reason)
        self.reason = reason
```

In the dispatch loop (`_handle_mutant` around line 587), catch this and emit `skipped` in the result:

```python
try:
    applier.dispatch(diff_entry, journal)
except _SkippedMutation as exc:
    journal.rollback()
    return {"status": "skipped", "reason": exc.reason}
except SyntaxError as exc:
    journal.rollback()
    return {"status": "error", "error": f"generated_syntax_error: {exc}"}
except Exception as exc:
    # Mutated code raised at exec — treat as killed (the mutation broke import,
    # which is the test signal).
    journal.rollback()
    return {"status": "killed", "killed_by": f"exec_raised: {type(exc).__name__}"}
```

- [ ] **Step 2: Run plugin tests, expect PASS** for class-scope cases.

```bash
just test-plugin
```

- [ ] **Step 3: Commit**

```bash
git add src/plugin/_fest_plugin.py
git commit -m "feat(plugin): apply class-scope StatementBind via exec into class ns"
```

---

## Task 10: Nested classes (`descend_class` for arbitrary depth)

**Files:**
- Modify: `src/mutation/diff.rs` (extend `derive_for_class_body` to recurse on nested ClassDef)

- [ ] **Step 1: Failing test**

```rust
#[test]
fn nested_class_method_uses_dotted_qualname() {
    let src = "class Outer:\n    class Inner:\n        def m(self):\n            return 1\n";
    let mutated = "class Outer:\n    class Inner:\n        def m(self):\n            return 2\n";
    let module = parse_module_helper(src);
    // Position of "1" inside the method body.
    let byte_offset = src.find("return 1").unwrap() + "return ".len();
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 4, column: 20,
        byte_offset, byte_length: 1,
        original_text: "1".into(), mutated_text: "2".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    match diff.as_slice() {
        [MutationDiff::ClassMethod { class_qualname, method_name, .. }] => {
            assert_eq!(class_qualname, "Outer.Inner");
            assert_eq!(method_name, "m");
        }
        other => panic!("expected ClassMethod, got {other:?}"),
    }
}

#[test]
fn nested_class_attr_uses_dotted_qualname() {
    let src = "class Outer:\n    class Inner:\n        X = 5\n";
    let mutated = "class Outer:\n    class Inner:\n        X = 6\n";
    let module = parse_module_helper(src);
    let byte_offset = src.find("X = 5").unwrap() + "X = ".len();
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 3, column: 13,
        byte_offset, byte_length: 1,
        original_text: "5".into(), mutated_text: "6".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    match diff.as_slice() {
        [MutationDiff::StatementBind { names, scope: BindScope::Class { qualname }, .. }] => {
            assert_eq!(qualname, "Outer.Inner");
            assert_eq!(names, &vec!["X".to_owned()]);
        }
        other => panic!("expected StatementBind with class scope Outer.Inner, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run, expect FAIL.**

- [ ] **Step 3: Make `derive_for_class_body` recurse on nested ClassDef**

Replace the `Stmt::ClassDef(_) => { return None; }` arm with:

```rust
            Stmt::ClassDef(nested) => {
                return derive_for_class_body(nested, mutant, mutated_source, &class_qualname);
            }
```

- [ ] **Step 4: Run, expect PASS** + commit.

```bash
cargo test --all-features mutation::diff
git add src/mutation/diff.rs
git commit -m "feat(diff): support arbitrarily nested class bodies"
```

---

## Task 11: Control-flow recursion (`If`/`Try`/`For`/`While`)

**Files:**
- Modify: `src/mutation/diff.rs` (new `walk_control_flow`, integrate into `derive_diff`)

- [ ] **Step 1: Failing tests** (one per branch — keep them lean)

```rust
#[test]
fn mutation_inside_if_body_recurses() {
    let src = "import sys\nif sys.version_info >= (3, 11):\n    X = 5\n";
    let mutated = "import sys\nif sys.version_info >= (3, 11):\n    X = 6\n";
    let module = parse_module_helper(src);
    let off = src.find("X = 5").unwrap() + "X = ".len();
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 3, column: 9,
        byte_offset: off, byte_length: 1,
        original_text: "5".into(), mutated_text: "6".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    assert_eq!(diff, vec![MutationDiff::StatementBind {
        names: vec!["X".to_owned()],
        stmt_source: "X = 6".to_owned(),
        scope: BindScope::Module,
    }]);
}

#[test]
fn mutation_inside_if_orelse_recurses() {
    let src = "if False:\n    X = 1\nelse:\n    X = 2\n";
    let mutated = "if False:\n    X = 1\nelse:\n    X = 3\n";
    let module = parse_module_helper(src);
    let off = src.rfind("X = 2").unwrap() + "X = ".len();
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 4, column: 9,
        byte_offset: off, byte_length: 1,
        original_text: "2".into(), mutated_text: "3".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    assert!(matches!(diff.as_slice(), [MutationDiff::StatementBind { .. }]));
}

#[test]
fn mutation_inside_try_handler_recurses() {
    let src = "try:\n    X = 1\nexcept Exception:\n    X = 2\n";
    let mutated = "try:\n    X = 1\nexcept Exception:\n    X = 3\n";
    let module = parse_module_helper(src);
    let off = src.rfind("X = 2").unwrap() + "X = ".len();
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 4, column: 9,
        byte_offset: off, byte_length: 1,
        original_text: "2".into(), mutated_text: "3".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    assert!(matches!(diff.as_slice(), [MutationDiff::StatementBind { .. }]));
}

#[test]
fn mutation_inside_for_body_recurses() {
    let src = "for i in range(3):\n    X = 1\n";
    let mutated = "for i in range(3):\n    X = 2\n";
    let module = parse_module_helper(src);
    let off = src.find("X = 1").unwrap() + "X = ".len();
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 2, column: 9,
        byte_offset: off, byte_length: 1,
        original_text: "1".into(), mutated_text: "2".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    assert!(matches!(diff.as_slice(), [MutationDiff::StatementBind { .. }]));
}

#[test]
fn mutation_in_if_test_yields_empty_diff() {
    let src = "if 1 < 2:\n    X = 1\n";
    let mutated = "if 1 < 3:\n    X = 1\n";
    let module = parse_module_helper(src);
    let off = src.find("1 < 2").unwrap() + 4;     // position of "2"
    let mutant = Mutant {
        file_path: PathBuf::from("m.py"),
        line: 1, column: 5,
        byte_offset: off, byte_length: 1,
        original_text: "2".into(), mutated_text: "3".into(),
        mutator_name: "constant".into(),
    };
    let diff = derive_diff(&mutant, &module, &module, mutated);
    assert!(diff.is_empty(), "condition mutation is out-of-scope");
}
```

- [ ] **Step 2: Run, expect FAIL** for the four "recurses" tests; the `if_test_yields_empty_diff` may already pass.

- [ ] **Step 3: Implement `walk_control_flow`**

Add helper to `src/mutation/diff.rs`:

```rust
/// Recurse into the body branches of a control-flow statement. If the
/// mutant offset is inside any branch and that branch's statement is
/// matched by `derive_for_top_level`, return the corresponding diff.
fn walk_control_flow(
    stmt: &Stmt,
    mutated_source: &str,
    mutant: &Mutant,
) -> Option<MutationDiff> {
    let bodies: Vec<&Vec<Stmt>> = match stmt {
        Stmt::If(s)    => {
            let mut v = vec![&s.body];
            // elif clauses live inside elif_else_clauses on ruff 0.11.6.
            for clause in &s.elif_else_clauses {
                v.push(&clause.body);
            }
            v
        }
        Stmt::While(s) => vec![&s.body, &s.orelse],
        Stmt::For(s)   => vec![&s.body, &s.orelse],
        Stmt::Try(s)   => {
            let mut v = vec![&s.body, &s.orelse, &s.finalbody];
            for h in &s.handlers {
                if let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = h {
                    v.push(&handler.body);
                }
            }
            v
        }
        _ => return None,
    };
    for body in bodies {
        for inner in body {
            let r = inner.range();
            if !(r.start().to_usize() <= mutant.byte_offset
                && mutant.byte_offset < r.end().to_usize()) {
                continue;
            }
            // Reuse top-level matching against the inner statement.
            if let Some(d) = derive_for_top_level(inner, mutated_source, mutant) {
                return Some(d);
            }
            // Recurse further if the inner is itself control flow.
            if matches!(inner, Stmt::If(_) | Stmt::While(_) | Stmt::For(_) | Stmt::Try(_)) {
                if let Some(d) = walk_control_flow(inner, mutated_source, mutant) {
                    return Some(d);
                }
            }
        }
    }
    None
}
```

In `derive_diff` (around line 72-95), add a branch after the existing top-level walk:

```rust
    for stmt in &original_ast.body {
        let range = stmt.range();
        let start = range.start().to_usize();
        let end = range.end().to_usize();
        if start <= mutation_start && mutation_start < end {
            // Existing matching.
            if let Some(diff) = derive_for_top_level(stmt, mutated_source, mutant) {
                return vec![diff];
            }
            // New: recurse into control-flow bodies.
            if matches!(stmt, Stmt::If(_) | Stmt::While(_) | Stmt::For(_) | Stmt::Try(_)) {
                if let Some(diff) = walk_control_flow(stmt, mutated_source, mutant) {
                    return vec![diff];
                }
            }
        }
    }
```

- [ ] **Step 4: Run, expect PASS** + commit.

```bash
cargo test --all-features mutation::diff
git add src/mutation/diff.rs
git commit -m "feat(diff): recurse into if/try/for/while bodies for binding mutations"
```

---

## Task 12: Detect `__all__` literal in `scan_source`

**Files:**
- Modify: `src/plugin_index.rs:35-42` (add fields), `:46-94` (scan_source)

- [ ] **Step 1: Failing test**

```rust
#[test]
fn scan_source_detects_static_all_literal() {
    let src = r#"
__all__ = ["User", "Group"]
class User: pass
class Group: pass
class _Private: pass
"#;
    let index = scan_source(src, "pkg.models", std::path::Path::new("pkg/models.py"));
    assert_eq!(
        index.module_exports.get("pkg.models"),
        Some(&vec!["User".to_owned(), "Group".to_owned()]),
    );
}

#[test]
fn scan_source_skips_dynamic_all() {
    let src = "__all__ = [n for n in dir() if not n.startswith('_')]\n";
    let index = scan_source(src, "pkg.x", std::path::Path::new("pkg/x.py"));
    assert!(index.module_exports.get("pkg.x").is_none(),
        "dynamic __all__ should not produce static exports");
}
```

- [ ] **Step 2: Run, expect FAIL** — field doesn't exist.

- [ ] **Step 3: Add `module_exports` field + detection logic**

In `src/plugin_index.rs:35-42`, extend `PluginIndex`:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginIndex {
    pub import_bindings: Vec<ImportBinding>,
    pub reload_warnings: Vec<ReloadWarning>,
    /// Module → static `__all__` literal contents, when expressible as a
    /// list/tuple of string literals. Modules with dynamic `__all__` or no
    /// `__all__` are absent.
    #[serde(default)]
    pub module_exports: std::collections::HashMap<String, Vec<String>>,
    /// Star-imports awaiting runtime resolution (source module had no
    /// static `__all__` we could parse).
    #[serde(default)]
    pub pending_star_imports: Vec<PendingStarImport>,
}

/// A `from X import *` binding that couldn't be statically resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingStarImport {
    /// Dotted name of the consumer (the file containing `from X import *`).
    pub consumer_module: String,
    /// Resolved source module name.
    pub target_module: String,
}
```

In `scan_source`, after the existing per-statement loop, walk top-level statements once to detect `__all__`:

```rust
    if let Some(names) = extract_static_all(&ast) {
        out.module_exports.insert(consumer_module.to_owned(), names);
    }
```

Add helper:

```rust
fn extract_static_all(ast: &ModModule) -> Option<Vec<String>> {
    use ruff_python_ast::{Expr, ExprList, ExprTuple, Stmt};
    for stmt in &ast.body {
        let Stmt::Assign(assign) = stmt else { continue };
        let [Expr::Name(target)] = assign.targets.as_slice() else { continue };
        if target.id.as_str() != "__all__" { continue; }
        let elts: &[Expr] = match &*assign.value {
            Expr::List(ExprList { elts, .. }) | Expr::Tuple(ExprTuple { elts, .. }) => elts,
            _ => continue,
        };
        let mut names = Vec::with_capacity(elts.len());
        for elt in elts {
            let Expr::StringLiteral(s) = elt else { return None };
            names.push(s.value.to_str().to_owned());
        }
        return Some(names);
    }
    None
}
```

- [ ] **Step 4: Run, expect PASS** + commit.

```bash
cargo test --all-features plugin_index::tests::scan_source_detects_static_all
git add src/plugin_index.rs
git commit -m "feat(plugin-index): extract static __all__ literal during scan"
```

---

## Task 13: Detect `from X import *` and resolve via `__all__`

**Files:**
- Modify: `src/plugin_index.rs` — extend import collection to recognize `import *`, add `resolve_star_imports` method on `PluginIndex`

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn star_import_resolves_via_static_all() {
    let api = "from .models import *\n";
    let models = "__all__ = [\"User\", \"Group\"]\nclass User: pass\nclass Group: pass\nclass _P: pass\n";
    let mut index = scan_source(api, "pkg.api", std::path::Path::new("pkg/api.py"));
    let mods = scan_source(models, "pkg.models", std::path::Path::new("pkg/models.py"));
    index.merge(mods);
    index.resolve_star_imports();
    let names: Vec<String> = index.import_bindings.iter()
        .filter(|b| b.consumer_module == "pkg.api" && b.target_module == "pkg.models")
        .map(|b| b.target_name.clone())
        .collect();
    assert_eq!(names, vec!["User".to_owned(), "Group".to_owned()]);
    assert!(index.pending_star_imports.is_empty(),
        "static resolution should leave no pending entries");
}

#[test]
fn star_import_without_all_falls_through_to_pending() {
    let api = "from .models import *\n";
    let models = "class Open: pass\nclass _Hidden: pass\n";  // no __all__
    let mut index = scan_source(api, "pkg.api", std::path::Path::new("pkg/api.py"));
    let mods = scan_source(models, "pkg.models", std::path::Path::new("pkg/models.py"));
    index.merge(mods);
    index.resolve_star_imports();
    assert!(!index.pending_star_imports.is_empty(),
        "missing __all__ should leave a pending entry for runtime resolution");
}
```

- [ ] **Step 2: Run, expect FAIL** — `merge` and `resolve_star_imports` don't exist.

- [ ] **Step 3: Implement**

In `src/plugin_index.rs`, add to `impl PluginIndex`:

```rust
    /// Merge another index into this one.
    pub fn merge(&mut self, other: PluginIndex) {
        self.import_bindings.extend(other.import_bindings);
        self.reload_warnings.extend(other.reload_warnings);
        self.module_exports.extend(other.module_exports);
        self.pending_star_imports.extend(other.pending_star_imports);
    }

    /// Walk every recorded `from X import *` placeholder and either
    /// synthesize one named [`ImportBinding`] per name in `X.__all__`
    /// (when the scan captured it), or leave it as a [`PendingStarImport`]
    /// for the plugin to resolve at runtime.
    pub fn resolve_star_imports(&mut self) {
        // Separate placeholders from real bindings without borrowing `self`
        // twice. Two owned Vecs, then write back at the end.
        let owned = std::mem::take(&mut self.import_bindings);
        let mut placeholders: Vec<ImportBinding> = Vec::new();
        let mut kept: Vec<ImportBinding> = Vec::with_capacity(owned.len());
        for b in owned {
            if b.target_name == "*" {
                placeholders.push(b);
            } else {
                kept.push(b);
            }
        }
        self.import_bindings = kept;
        for ph in placeholders {
            if let Some(names) = self.module_exports.get(&ph.target_module).cloned() {
                for n in names {
                    self.import_bindings.push(ImportBinding {
                        consumer_module: ph.consumer_module.clone(),
                        consumer_key: n.clone(),
                        target_module: ph.target_module.clone(),
                        target_name: n,
                    });
                }
            } else {
                self.pending_star_imports.push(PendingStarImport {
                    consumer_module: ph.consumer_module,
                    target_module: ph.target_module,
                });
            }
        }
    }
```

Extend the import collector to emit a placeholder for `import *`. Find the existing `collect_from_stmt` or equivalent (search `Stmt::ImportFrom` in `src/plugin_index.rs`) and add a branch when the `Alias::*` form is present — emit one `ImportBinding { target_name: "*", consumer_key: "*", ... }` as the placeholder.

- [ ] **Step 4: Run tests, expect PASS** + commit.

```bash
cargo test --all-features plugin_index::tests::star_import
git add src/plugin_index.rs
git commit -m "feat(plugin-index): resolve from-X-import-star via static __all__"
```

---

## Task 14: Runtime fallback for pending star imports

Wire `pending_star_imports` through the ready_ack handshake; resolve them in the Python plugin at worker startup.

**Files:**
- Modify: `src/runner/pytest_plugin.rs` (build_ready_ack_message)
- Modify: `src/plugin/_fest_plugin.py` (`ReverseImportIndex.ingest_ast_layer`, new `_resolve_pending_star_import`)
- Modify: `src/runner/pytest_plugin.rs` — call `index.resolve_star_imports()` after `scan_project`

- [ ] **Step 1: Add `resolve_star_imports` to `scan_project`** in `src/plugin_index.rs:195+`:

```rust
pub fn scan_project(root: &std::path::Path) -> std::io::Result<PluginIndex> {
    // ... existing walk_dir logic that fills `index` ...
    let mut index = /* existing result */;
    index.resolve_star_imports();
    Ok(index)
}
```

- [ ] **Step 2: Wire `pending_star_imports` into ready_ack**

In `src/runner/pytest_plugin.rs:864-873`:

```rust
fn build_ready_ack_message(index: &crate::plugin_index::PluginIndex) -> String {
    let msg = serde_json::json!({
        "type": "ready_ack",
        "import_bindings": index.import_bindings,
        "reload_warnings": index.reload_warnings,
        "pending_star_imports": index.pending_star_imports,
    });
    msg.to_string()
}
```

- [ ] **Step 3: Add failing plugin test**

In `tests/plugin/test_reverse_import_index.py`:

```python
def test_pending_star_import_resolves_with_runtime_all(tmp_path, monkeypatch):
    """When models.py exposes __all__ at runtime, pending star imports
    synthesize one binding per name."""
    monkeypatch.syspath_prepend(str(tmp_path))
    (tmp_path / "pkg").mkdir()
    (tmp_path / "pkg" / "__init__.py").write_text("")
    (tmp_path / "pkg" / "models_dyn.py").write_text(
        "__all__ = ['A']\nA = 1\nB = 2\n",
    )
    (tmp_path / "pkg" / "api_dyn.py").write_text("from pkg.models_dyn import *\n")
    import importlib; importlib.invalidate_caches()
    import pkg.api_dyn  # noqa: F401  # forces consumer import
    idx = ReverseImportIndex()
    idx.ingest_pending_star_imports([
        {"consumer_module": "pkg.api_dyn", "target_module": "pkg.models_dyn"},
    ])
    assert idx.lookup("pkg.models_dyn", "A"), "A in __all__ should be bound"
    assert not idx.lookup("pkg.models_dyn", "B"), "B not in __all__"


def test_pending_star_import_falls_back_to_vars_when_all_missing(tmp_path, monkeypatch):
    monkeypatch.syspath_prepend(str(tmp_path))
    (tmp_path / "pkg2").mkdir()
    (tmp_path / "pkg2" / "__init__.py").write_text("")
    (tmp_path / "pkg2" / "noall.py").write_text("Open = 1\n_Hidden = 2\n")
    (tmp_path / "pkg2" / "api.py").write_text("from pkg2.noall import *\n")
    import importlib; importlib.invalidate_caches()
    import pkg2.api  # noqa: F401
    idx = ReverseImportIndex()
    idx.ingest_pending_star_imports([
        {"consumer_module": "pkg2.api", "target_module": "pkg2.noall"},
    ])
    assert idx.lookup("pkg2.noall", "Open")
    assert not idx.lookup("pkg2.noall", "_Hidden"), \
        "underscore-prefixed names should be filtered"


def test_pending_star_import_silent_skip_on_import_failure():
    idx = ReverseImportIndex()
    # Module doesn't exist; should not raise.
    idx.ingest_pending_star_imports([
        {"consumer_module": "missing_consumer", "target_module": "no.such.module"},
    ])
    assert not idx.lookup("no.such.module", "anything")
```

- [ ] **Step 4: Run tests, expect FAIL** — method doesn't exist.

- [ ] **Step 5: Implement `ingest_pending_star_imports`**

In `src/plugin/_fest_plugin.py`, add to `ReverseImportIndex`:

```python
    def ingest_pending_star_imports(self, pending: list[dict[str, str]]) -> None:
        """Resolve `from X import *` bindings that the Rust scan deferred to
        runtime. Imports each source module, reads its `__all__` (or falls
        back to non-underscore names from `vars()`), and registers each name
        as a binding.

        Failures to import the source are silently skipped — propagation just
        won't reach that consumer.
        """
        import importlib
        for entry in pending:
            consumer_name = entry.get("consumer_module", "")
            target_name = entry.get("target_module", "")
            consumer_mod = sys.modules.get(consumer_name)
            if consumer_mod is None:
                continue
            try:
                source_mod = importlib.import_module(target_name)
            except Exception:
                continue
            names = getattr(source_mod, "__all__", None)
            if names is None:
                names = [n for n in vars(source_mod) if not n.startswith("_")]
            for name in names:
                self.add(target_name, name, consumer_mod.__dict__, name)
```

In `pytest_runtestloop` (or wherever `ingest_ast_layer` is called — around line 484), also call:

```python
rev_index.ingest_pending_star_imports(ack.get("pending_star_imports", []))
```

- [ ] **Step 6: Run, expect PASS** + commit.

```bash
just test-plugin
cargo test --all-features
git add src/plugin/_fest_plugin.py src/plugin_index.rs src/runner/pytest_plugin.rs tests/plugin/test_reverse_import_index.py
git commit -m "feat(plugin): resolve pending star imports at worker startup"
```

---

## Task 15: Route empty-diff into `MutantStatus::Skipped`

Currently a mutant with empty `diff` is reported as `error`. Change the runner so each `SkipReason` from derive_diff failures and applier-raised `_SkippedMutation` exceptions surfaces as `MutantStatus::Skipped { reason }`.

**Files:**
- Modify: `src/mutation/diff.rs:72-95` (`derive_diff` returns either a Vec OR a `SkipReason`)
- Modify: `src/runner/pytest_plugin.rs:265-275` (handle the new shape)
- Modify: `src/plugin/_fest_plugin.py` (already emits `{"status": "skipped", "reason": ...}` from Task 9; parse it)

- [ ] **Step 1: Failing runner test**

In `src/runner/pytest_plugin.rs` test module:

```rust
#[test]
fn parse_status_handles_skipped_with_reason() {
    let json = serde_json::json!({"status": "skipped", "reason": "condition_mutation"});
    let status = parse_status(&json).unwrap();
    assert_eq!(status, MutantStatus::Skipped {
        reason: crate::mutation::mutant::SkipReason::ConditionMutation,
    });
}
```

- [ ] **Step 2: Run, expect FAIL.**

- [ ] **Step 3: Extend `parse_status`** (around line 875-900 in `pytest_plugin.rs`):

```rust
fn parse_status(value: &serde_json::Value) -> Result<MutantStatus, Error> {
    let status = value.get("status").and_then(|v| v.as_str()).ok_or_else(|| ...)?;
    match status {
        "killed"   => Ok(MutantStatus::Killed),
        "survived" => Ok(MutantStatus::Survived),
        "skipped"  => {
            let reason_str = value.get("reason").and_then(|v| v.as_str()).unwrap_or("unsupported_statement");
            let reason: SkipReason = serde_json::from_value(serde_json::Value::String(reason_str.to_owned()))
                .unwrap_or(SkipReason::UnsupportedStatement);
            Ok(MutantStatus::Skipped { reason })
        }
        "error"    => Ok(MutantStatus::Error(/* extract message */)),
        other      => Err(...),
    }
}
```

- [ ] **Step 4: Route empty-diff to skip in Runner.run_mutant**

In `src/runner/pytest_plugin.rs:265-280`, when `derive_diff` returns empty, short-circuit:

```rust
let diff = crate::mutation::derive_diff(mutant, &original_ast, &mutated_ast, &mutated_source);
if diff.is_empty() {
    return Ok(MutantStatus::Skipped {
        reason: SkipReason::UnsupportedStatement,
    });
}
let msg = build_mutant_message(mutant, &mutated_source, tests, &diff);
```

- [ ] **Step 5: Run, expect PASS**

```bash
cargo test --all-features runner::pytest_plugin::tests::parse_status
```

- [ ] **Step 6: Commit**

```bash
git add src/runner/pytest_plugin.rs
git commit -m "feat(runner): route empty-diff and applier-skip to MutantStatus::Skipped"
```

---

## Task 16: Refine `SkipReason` classification at derivation time

Right now every empty-diff is `UnsupportedStatement`. Distinguish `UnmappableTarget` (Assign with non-Name targets) from `UnsupportedStatement` (statement kind not handled) from `ConditionMutation` (mutation in an `if`/`while`/`for` test position).

**Files:**
- Modify: `src/mutation/diff.rs` — change `derive_diff` return type to `Result<Vec<MutationDiff>, SkipReason>`

- [ ] **Step 1: Update tests**

```rust
#[test]
fn condition_mutation_returns_skip_reason() {
    let src = "if 1 < 2:\n    X = 1\n";
    let mutated = "if 1 < 3:\n    X = 1\n";
    let module = parse_module_helper(src);
    let off = src.find("1 < 2").unwrap() + 4;
    let mutant = Mutant { /* mutate the `2` */ ..mk_mutant("2", "3", off) };
    let result = derive_diff(&mutant, &module, &module, mutated);
    assert_eq!(result, Err(SkipReason::ConditionMutation));
}

#[test]
fn subscript_target_returns_unmappable() {
    let src = "d = {}\nd['k'] = 1\n";
    let module = parse_module_helper(src);
    let off = src.find("= 1").unwrap() + 2;
    let mutant = Mutant { /* ... */ ..mk_mutant("1", "2", off) };
    let result = derive_diff(&mutant, &module, &module, "d = {}\nd['k'] = 2\n");
    assert_eq!(result, Err(SkipReason::UnmappableTarget));
}
```

- [ ] **Step 2: Run, expect FAIL**

- [ ] **Step 3: Refactor `derive_diff` to return `Result<Vec<_>, SkipReason>`**

```rust
pub fn derive_diff(
    mutant: &Mutant,
    original_ast: &ModModule,
    _mutated_ast: &ModModule,
    mutated_source: &str,
) -> Result<Vec<MutationDiff>, SkipReason> {
    // remove_decorator first (unchanged)
    if mutant.mutator_name == "remove_decorator" {
        if let Some(d) = derive_decorator_removal(mutant, original_ast, mutated_source) {
            return Ok(vec![d]);
        }
    }
    let off = mutant.byte_offset;
    for stmt in &original_ast.body {
        let r = stmt.range();
        if !(r.start().to_usize() <= off && off < r.end().to_usize()) { continue; }
        if let Some(d) = derive_for_top_level(stmt, mutated_source, mutant) {
            return Ok(vec![d]);
        }
        if matches!(stmt, Stmt::If(_) | Stmt::While(_) | Stmt::For(_) | Stmt::Try(_)) {
            // Check if offset is in the condition position (not body).
            if is_in_condition(stmt, off) {
                return Err(SkipReason::ConditionMutation);
            }
            if let Some(d) = walk_control_flow(stmt, mutated_source, mutant) {
                return Ok(vec![d]);
            }
        }
        // Differentiate unmappable target vs unsupported.
        if matches!(stmt, Stmt::Assign(_) | Stmt::AnnAssign(_) | Stmt::AugAssign(_)) {
            return Err(SkipReason::UnmappableTarget);
        }
        return Err(SkipReason::UnsupportedStatement);
    }
    Err(SkipReason::UnsupportedStatement)
}

fn is_in_condition(stmt: &Stmt, off: usize) -> bool {
    let test_range = match stmt {
        Stmt::If(s)    => s.test.range(),
        Stmt::While(s) => s.test.range(),
        Stmt::For(s)   => {
            // For "test" is the iter expression.
            s.iter.range()
        }
        _ => return false,
    };
    test_range.start().to_usize() <= off && off < test_range.end().to_usize()
}
```

- [ ] **Step 4: Update callers**

`src/runner/pytest_plugin.rs` runner — change `if diff.is_empty()` to:

```rust
let diff = match crate::mutation::derive_diff(mutant, &original_ast, &mutated_ast, &mutated_source) {
    Ok(d) => d,
    Err(reason) => return Ok(MutantStatus::Skipped { reason }),
};
```

- [ ] **Step 5: Run all tests** + commit.

```bash
cargo test --all-features
git add src/mutation/diff.rs src/runner/pytest_plugin.rs
git commit -m "feat(diff): classify skip reasons (unmappable/condition/unsupported)"
```

---

## Task 17: Update reports (text/json/html) to show Skipped

**Files:**
- Modify: `src/report.rs` (aggregate counters)
- Modify: `src/report/text.rs` (skipped line + verbose breakdown)
- Modify: `src/report/json.rs` (`skipped`, `skip_reasons` fields)
- Modify: `src/report/html.rs` (skipped badge + section)

- [ ] **Step 1: Failing test in `src/report/text.rs` (or wherever aggregate report tests live)**

```rust
#[test]
fn text_report_shows_skipped_count_and_breakdown() {
    let report = SummaryReport {
        total_generated: 10,
        killed: 7,
        survived: 1,
        skipped: 2,
        errors: 0,
        skip_reasons: HashMap::from([
            ("condition_mutation".into(), 1),
            ("unmappable_target".into(), 1),
        ]),
        // ...
    };
    let mut out = Vec::new();
    write_summary(&report, &mut out, false /* colored */).unwrap();
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("Skipped:    2"));
    assert!(s.contains("condition_mutation:  1"));
    // Score: 7 / (7 + 1) = 87.5%
    assert!(s.contains("87.5%"));
}
```

- [ ] **Step 2: Run, expect FAIL**

- [ ] **Step 3: Add `skipped` and `skip_reasons` fields to the report struct**

Find the `SummaryReport`/`Report` struct in `src/report.rs` and add:

```rust
    pub skipped: usize,
    pub skip_reasons: std::collections::HashMap<String, usize>,
```

Update the aggregator (probably in `src/session.rs` or `src/lib.rs` where results are tallied) to count `Skipped { reason }` separately and increment `skip_reasons[reason_str]`.

Update score formula: `killed as f64 / (killed + survived) as f64 * 100.0`. Where `killed + survived == 0`, score is `N/A` or `100%` depending on existing convention — match what's already there.

- [ ] **Step 4: Render skipped line in text report**

In `src/report/text.rs`, after the Survived line (around line 150):

```rust
if report.skipped > 0 {
    writeln!(output, "  Skipped:   {:>4}  (out-of-scope mutations)", report.skipped)?;
    if verbose {
        writeln!(output, "    Skipped breakdown:")?;
        let mut entries: Vec<_> = report.skip_reasons.iter().collect();
        entries.sort_by_key(|(k, _)| (*k).clone());
        for (reason, count) in entries {
            writeln!(output, "      {:24} {}", format!("{}:", reason), count)?;
        }
    }
}
```

- [ ] **Step 5: Update JSON report**

In `src/report/json.rs`, add `skipped` and `skip_reasons` to the serialized aggregate, and add `"outcome": "skipped"` + `"reason": <str>` to per-mutant entries when status is `Skipped`.

- [ ] **Step 6: Update HTML report**

In `src/report/html.rs`, add a skipped count chip and a per-reason detail section. Color: desaturated yellow (`#d4a017` or similar — match existing palette).

- [ ] **Step 7: Run all report tests, expect PASS** + commit.

```bash
cargo test --all-features report
git add src/report.rs src/report/text.rs src/report/json.rs src/report/html.rs src/session.rs
git commit -m "feat(report): show skipped count and per-reason breakdown in all formats"
```

---

## Task 18: Protocol version handshake

**Files:**
- Modify: `src/runner/pytest_plugin.rs` (new PROTOCOL_VERSION constant + ready handshake check)
- Modify: `src/plugin/_fest_plugin.py` (read protocol_version from ready_ack, refuse mismatch)

- [ ] **Step 1: Add constant + emit**

In `src/runner/pytest_plugin.rs`, near the top:

```rust
/// IPC protocol version. Bump whenever the JSON wire format changes in a
/// non-backwards-compatible way. Plugin refuses mismatched ready_ack.
pub const PROTOCOL_VERSION: u32 = 2;   // Plan G was implicit v1; this spec bumps to v2.
```

In `build_ready_ack_message`:

```rust
let msg = serde_json::json!({
    "type": "ready_ack",
    "protocol_version": PROTOCOL_VERSION,
    "import_bindings": index.import_bindings,
    "reload_warnings": index.reload_warnings,
    "pending_star_imports": index.pending_star_imports,
});
```

- [ ] **Step 2: Plugin checks the version**

In `src/plugin/_fest_plugin.py:484+`, after receiving `ack`:

```python
_PLUGIN_PROTOCOL_VERSION = 2
ack_ver = ack.get("protocol_version")
if ack_ver != _PLUGIN_PROTOCOL_VERSION:
    print(
        f"[fest plugin] protocol version mismatch: runner={ack_ver}, "
        f"plugin={_PLUGIN_PROTOCOL_VERSION}. Aborting.",
        file=sys.stderr,
    )
    return True   # signal "done, do not run tests" to pytest
```

- [ ] **Step 3: Add a test**

```rust
#[test]
fn ready_ack_includes_protocol_version() {
    let idx = PluginIndex::default();
    let msg = build_ready_ack_message(&idx);
    let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
    assert_eq!(v["protocol_version"], serde_json::json!(PROTOCOL_VERSION));
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test --all-features ready_ack_includes_protocol_version
just test-plugin
git add src/runner/pytest_plugin.rs src/plugin/_fest_plugin.py
git commit -m "feat(plugin-ipc): bump PROTOCOL_VERSION to 2 with handshake check"
```

---

## Task 19: Update `bench/compare.sh` to extract Skipped count

**Files:**
- Modify: `bench/compare.sh`

- [ ] **Step 1: Add extraction line** (search for the existing `FEST_KILLED=` line, around line 120-130):

```bash
FEST_SKIPPED=$(echo "$FEST_OUTPUT" | grep -oP 'Skipped:\s*\K[0-9]+' | head -1 || echo "0")
```

- [ ] **Step 2: Add column to the summary printout** (search for the table-print section near the end of the script):

```bash
printf "  fest:        killed=%s  survived=%s  skipped=%s  errors=%s  score=%s  time=%s\n" \
    "$FEST_KILLED" "$FEST_SURVIVED" "$FEST_SKIPPED" "$FEST_ERRORS" "$FEST_SCORE" \
    "$(format_time "$FEST_TIME")"
```

- [ ] **Step 3: Commit**

```bash
git add bench/compare.sh
git commit -m "feat(bench): extract and display Skipped count in compare summary"
```

---

## Task 20: Verify aliased re-exports

The spec calls out `from x import y as z` as needing verification — fix if it's broken. This is a quick test-only task that either passes immediately (current code is fine) or surfaces a bug that needs fixing inside `scan_source`'s `ImportFrom` collection.

**Files:**
- Modify: `src/plugin_index.rs` (test module)

- [ ] **Step 1: Add the verification test**

```rust
#[test]
fn aliased_reexport_records_target_and_local_names_separately() {
    let src = "from pkg.models import User as MyUser\n";
    let index = scan_source(src, "pkg.api", std::path::Path::new("pkg/api.py"));
    assert_eq!(index.import_bindings.len(), 1);
    let b = &index.import_bindings[0];
    assert_eq!(b.consumer_module, "pkg.api");
    assert_eq!(b.consumer_key, "MyUser",     "consumer_key must be the alias");
    assert_eq!(b.target_module, "pkg.models");
    assert_eq!(b.target_name,   "User",      "target_name must be the original");
}
```

- [ ] **Step 2: Run test**

```bash
cargo test --all-features plugin_index::tests::aliased_reexport
```

- [ ] **Step 3: If PASS** — commit just the test (it locks in current behavior).
**If FAIL** — fix `ImportFrom` collection in `scan_source` so the alias and original name are recorded distinctly, then commit both.

```bash
git add src/plugin_index.rs
git commit -m "test(plugin-index): lock in aliased from-import semantics"
```

---

## Task 21: Integration fixture `control_flow_bindings/`

**Files:**
- Create: `tests/fixtures/control_flow_bindings/pyproject.toml`
- Create: `tests/fixtures/control_flow_bindings/conftest.py`
- Create: `tests/fixtures/control_flow_bindings/src/__init__.py`
- Create: `tests/fixtures/control_flow_bindings/src/config.py`
- Create: `tests/fixtures/control_flow_bindings/src/annotated.py`
- Create: `tests/fixtures/control_flow_bindings/src/nested_classes.py`
- Create: `tests/fixtures/control_flow_bindings/src/starred/__init__.py`
- Create: `tests/fixtures/control_flow_bindings/src/starred/models.py`
- Create: `tests/fixtures/control_flow_bindings/src/starred/api.py`
- Create: `tests/fixtures/control_flow_bindings/tests/test_config.py`
- Create: `tests/fixtures/control_flow_bindings/tests/test_annotated.py`
- Create: `tests/fixtures/control_flow_bindings/tests/test_nested.py`
- Create: `tests/fixtures/control_flow_bindings/tests/test_starred.py`
- Modify: `src/runner/pytest_plugin.rs` tests — add `#[ignore]` integration test

- [ ] **Step 1: Create the fixture files**

`tests/fixtures/control_flow_bindings/pyproject.toml`:

```toml
[project]
name = "control-flow-bindings"
version = "0.0.0"
requires-python = ">=3.11"
```

`tests/fixtures/control_flow_bindings/conftest.py`:

```python
import sys, pathlib
sys.path.insert(0, str(pathlib.Path(__file__).parent / "src"))
```

`src/config.py`:

```python
import sys

if sys.version_info >= (3, 11):
    MAX_RETRIES = 5
    BACKOFF = 0.5
else:
    MAX_RETRIES = 3
    BACKOFF = 1.0
```

`src/annotated.py`:

```python
COUNTER: int = 0
HEADERS: tuple = ("content-type", "accept")

def reset() -> int:
    global COUNTER
    COUNTER = 0
    return COUNTER
```

`src/nested_classes.py`:

```python
class Outer:
    class Inner:
        VALUE = 42

        def compute(self) -> int:
            return self.VALUE * 2
```

`src/starred/__init__.py`: empty file.

`src/starred/models.py`:

```python
__all__ = ["User"]

class User:
    def name(self) -> str:
        return "alice"

class _Internal:
    """Should not be star-imported."""
```

`src/starred/api.py`:

```python
from .models import *

def whoami() -> str:
    return User().name()
```

`tests/test_config.py`:

```python
from config import MAX_RETRIES, BACKOFF

def test_max_retries_is_five():
    assert MAX_RETRIES == 5

def test_backoff_is_half_second():
    assert BACKOFF == 0.5

def test_backoff_positive():
    assert BACKOFF > 0
```

`tests/test_annotated.py`:

```python
import annotated

def test_counter_starts_at_zero():
    assert annotated.COUNTER == 0

def test_headers_includes_content_type():
    assert "content-type" in annotated.HEADERS
```

`tests/test_nested.py`:

```python
from nested_classes import Outer

def test_inner_value():
    assert Outer.Inner.VALUE == 42

def test_inner_compute_doubles():
    assert Outer.Inner().compute() == 84
```

`tests/test_starred.py`:

```python
from starred.api import whoami, User

def test_whoami_returns_alice():
    assert whoami() == "alice"

def test_user_exported():
    assert User().name() == "alice"
```

- [ ] **Step 2: Add the integration test in `src/runner/pytest_plugin.rs`**

Append to the existing test module:

```rust
#[test]
#[ignore = "integration test; requires pytest in environment"]
fn plugin_handles_control_flow_bindings_fixture() {
    if !pytest_available() { return; }
    let fixture = std::path::Path::new("tests/fixtures/control_flow_bindings");
    let result = run_fixture_with_plugin_backend(fixture);
    assert!(result.errors == 0, "no tooling errors expected: {:?}", result);
    assert!(result.score >= 70.0, "mutation score too low: {}", result.score);
}
```

*(If `run_fixture_with_plugin_backend` doesn't already exist, copy the pattern from `plugin_run_mutant_propagates_to_consumer_via_index`.)*

- [ ] **Step 3: Run the ignored test manually**

```bash
cargo test --all-features --release plugin_handles_control_flow_bindings_fixture -- --ignored --nocapture
```

Expected: PASS with no errors, score ≥ 70%.

- [ ] **Step 4: Run `just check-all`** to ensure nothing regressed.

```bash
just check-all
```

- [ ] **Step 5: Commit**

```bash
git add tests/fixtures/control_flow_bindings/ src/runner/pytest_plugin.rs
git commit -m "test(plugin): add control_flow_bindings integration fixture"
```

---

## Self-Review (run by the engineer after completing all tasks)

- [ ] **Spec coverage check** — for each gap in the spec's Problem section, point at the task that closes it:
  - AnnAssign module/class → Task 4 + Task 8
  - AugAssign module → Task 5
  - Tuple/chained assigns → Task 7
  - Nested classes → Task 10
  - Control-flow recursion → Task 11
  - Star imports static → Tasks 12–13
  - Star imports runtime fallback → Task 14
  - Aliased re-exports → Task 20
- [ ] **Outcome taxonomy** — `Skipped` variant present, score formula excludes it, reports render breakdown.
- [ ] **PROTOCOL_VERSION** bumped to 2 and the plugin enforces it (Task 18).
- [ ] **`ConstantBind` and `ClassAttr`** no longer exist anywhere — `grep -r "ConstantBind\|ClassAttr" src/ tests/` returns zero hits.
- [ ] **No `_apply_constant_rebind` or `_apply_class_attr` in plugin** — `grep "_apply_constant\|_apply_class_attr" src/plugin/` returns zero hits.
- [ ] `just check-all` passes.
- [ ] `cargo test --release plugin_handles_control_flow_bindings_fixture -- --ignored` passes.
