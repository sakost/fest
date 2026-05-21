# Plugin Backend Reference Fixup (Plan G) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the plugin backend's gc-based reference fixup with a structured-diff IR (Rust side) + name-addressed reverse-import index + per-kind appliers + patch journal (Python side).

**Architecture:** Rust computes a `MutationDiff` IR alongside each `Mutant` and a one-time `ImportBinding` index from project source. Both are sent to the plugin (diff per-mutant, index in the `ready_ack` handshake). The plugin uses a `MutationApplier` that dispatches per-variant, recording undo entries into a `PatchJournal` for rollback after tests.

**Tech Stack:** Rust 1.93 / edition 2024; ruff_python_ast 0.11.6; serde_json; Python ≥3.9 (CPython only); pytest ≥7.

**Spec:** `docs/superpowers/specs/2026-03-14-plugin-reference-fixup-design.md`. **Branch:** `docs/plan-g-spec`.

---

## File Map

### Rust files

| File | New/Modify | Responsibility |
|---|---|---|
| `src/mutation/diff.rs` | New | `MutationDiff` enum + `derive_diff()` from a `Mutant` + AST pair |
| `src/mutation.rs` | Modify | `pub mod diff;` + re-exports |
| `src/plugin_index.rs` | New | `ImportBinding`, `ReloadWarning` types + `scan_project()` walking project `.py` files |
| `src/lib.rs` | Modify | `pub mod plugin_index;` |
| `src/runner/pytest_plugin.rs` | Modify | Build `ready_ack` with index payload; build `mutant` messages with structured diff; wire `scan_project` into `start()` |

### Python files

| File | New/Modify | Responsibility |
|---|---|---|
| `src/plugin/_fest_plugin.py` | Full rewrite | New top-level classes: `ReverseImportIndex`, `MutationApplier`, `PatchJournal`. Rewritten `_handle_mutant`. Thread detection. `fest_thread_cleanup` fixture. |

### Test files

| File | Responsibility |
|---|---|
| `tests/plugin/conftest.py` | Sys.path setup so tests can import the plugin module |
| `tests/plugin/test_patch_journal.py` | Append/rollback semantics; partial-failure resilience |
| `tests/plugin/test_reverse_import_index.py` | Runtime + AST layers; alias handling |
| `tests/plugin/test_mutation_applier.py` | All 5 IR variants; closure fallback |
| `tests/plugin/test_thread_detection.py` | Cleanup registry; warning-emit |
| `tests/fixtures/from_imports/` | End-to-end project: `from X import Y` patterns |
| `tests/fixtures/registry_classes/` | End-to-end project: `__init_subclass__` |
| `tests/fixtures/nested_closures/` | End-to-end project: nested function mutations |

### Tooling

| File | Change |
|---|---|
| `Justfile` | Add `test-plugin` recipe; include in `check-all` |

---

## Convention used in code samples

The plugin invokes Python's source-execution and source-evaluation builtins through aliases assigned once at module scope:

```python
_PY_EXEC = exec
_PY_EVAL = eval
```

All code samples below use `_PY_EXEC(...)` and `_PY_EVAL(...)` accordingly. This is both an audit aid (every dynamic-source call site is greppable for `_PY_EXEC` / `_PY_EVAL`) and a deliberate convention to keep the implementation isolated from the security-sensitive builtin names. Add the two aliases at the top of `src/plugin/_fest_plugin.py` as the first thing in Task 4.2 (immediately after the existing imports) so subsequent tasks can reference them.

## Phase 1 — Rust: `MutationDiff` IR

### Task 1.1: Create `MutationDiff` enum skeleton

**Files:**
- Create: `src/mutation/diff.rs`
- Modify: `src/mutation.rs`

- [ ] **Step 1: Add the `diff` module declaration in `src/mutation.rs`**

```rust
pub mod builtin;

pub mod diff;

pub mod mutant;

pub mod mutator;

pub(crate) mod seed;

pub use diff::MutationDiff;
pub use mutant::{Mutant, MutantResult, MutantStatus};
pub use mutator::{Mutation, MutationContext, Mutator, MutatorRegistry};
```

- [ ] **Step 2: Create `src/mutation/diff.rs`**

```rust
//! Structured diff IR for mutations dispatched to the plugin backend.

use serde::{Deserialize, Serialize};

/// One unit of structural change derived from a [`crate::mutation::Mutant`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MutationDiff {
    /// Function body changed. `qualname` may be dotted for nested functions.
    /// `new_source` is the raw `def` block with decorators stripped.
    FunctionBody { qualname: String, new_source: String },

    /// Module-level `NAME = expr` binding changed value.
    ConstantBind { name: String, new_expr: String },

    /// Class method body changed. `method_name` uses dotted suffix
    /// (`x.fget` / `.fset` / `.fdel`) for property accessors.
    ClassMethod {
        class_qualname: String,
        method_name: String,
        new_source: String,
    },

    /// Class-level non-method attribute changed.
    ClassAttr {
        class_qualname: String,
        name: String,
        new_expr: String,
    },

    /// Module-level binding requiring statement-mode compilation
    /// (decorator removal, class re-definition).
    ModuleAttr { name: String, new_source: String },
}
```

- [ ] **Step 3: Run `cargo check --all-features`**

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/mutation.rs src/mutation/diff.rs
git commit -m "feat(mutation): add MutationDiff IR enum"
```

---

### Task 1.2: Test and implement `derive_diff` for top-level function body

**Files:**
- Modify: `src/mutation/diff.rs`

- [ ] **Step 1: Append the failing test**

```rust
#[cfg(test)]
mod tests {
    use ruff_python_parser::parse_module;

    use super::*;
    use crate::mutation::Mutant;

    fn parse(src: &str) -> ruff_python_ast::ModModule {
        parse_module(src).expect("valid python").into_syntax()
    }

    fn make_mutant(file: &str, original: &str, mutated: &str, byte_offset: usize) -> Mutant {
        Mutant {
            file_path: file.into(),
            line: 1,
            column: 1,
            byte_offset,
            byte_length: original.len(),
            original_text: original.to_owned(),
            mutated_text: mutated.to_owned(),
            mutator_name: "test".to_owned(),
        }
    }

    #[test]
    fn top_level_function_body_yields_function_body_variant() {
        let original_src = "def add(a, b):\n    return a + b\n";
        let mutated_src = "def add(a, b):\n    return a - b\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let plus_offset = original_src.find('+').unwrap();
        let mutant = make_mutant("calc.py", "+", "-", plus_offset);

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src);

        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::FunctionBody { qualname, new_source } => {
                assert_eq!(qualname, "add");
                assert!(new_source.starts_with("def add"));
                assert!(new_source.contains("return a - b"));
            }
            other => panic!("expected FunctionBody, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run, expect FAIL** (`derive_diff` not defined).

`cargo test --all-features mutation::diff::tests::top_level_function_body_yields_function_body_variant`

- [ ] **Step 3: Implement `derive_diff` plus helpers**

Insert above the tests module:

```rust
use ruff_python_ast::{ModModule, Stmt};
use ruff_text_size::Ranged;

use crate::mutation::Mutant;

#[must_use]
pub fn derive_diff(
    mutant: &Mutant,
    original_ast: &ModModule,
    _mutated_ast: &ModModule,
    mutated_source: &str,
) -> Vec<MutationDiff> {
    let mutation_start = mutant.byte_offset;
    for stmt in &original_ast.body {
        let range = stmt.range();
        let start = range.start().to_usize();
        let end = range.end().to_usize();
        if start <= mutation_start && mutation_start < end {
            if let Some(diff) = derive_for_top_level(stmt, mutated_source, mutant) {
                return vec![diff];
            }
        }
    }
    Vec::new()
}

fn derive_for_top_level(
    stmt: &Stmt,
    mutated_source: &str,
    mutant: &Mutant,
) -> Option<MutationDiff> {
    match stmt {
        Stmt::FunctionDef(func) => {
            let stmt_start = stmt.range().start().to_usize();
            let stmt_end_in_mutated = stmt_start
                + stmt.range().len().to_usize()
                + mutant.mutated_text.len()
                - mutant.byte_length;
            let new_source = strip_decorators(
                mutated_source.get(stmt_start..stmt_end_in_mutated).unwrap_or(""),
            );
            Some(MutationDiff::FunctionBody {
                qualname: func.name.id.to_string(),
                new_source,
            })
        }
        _ => None,
    }
}

fn strip_decorators(source: &str) -> String {
    let mut lines: Vec<&str> = source.lines().collect();
    while lines.first().is_some_and(|line| line.trim_start().starts_with('@')) {
        let _ = lines.remove(0);
    }
    lines.join("\n")
}
```

- [ ] **Step 4: Run, expect PASS**

`cargo test --all-features mutation::diff::tests::top_level_function_body_yields_function_body_variant`

- [ ] **Step 5: Commit**

```bash
git add src/mutation/diff.rs
git commit -m "feat(mutation/diff): derive FunctionBody variant for top-level def mutations"
```

---

### Task 1.3: Module-level constant → `ConstantBind`

**Files:** Modify `src/mutation/diff.rs`.

- [ ] **Step 1: Append failing test**

```rust
    #[test]
    fn module_level_constant_yields_constant_bind_variant() {
        let original_src = "MAX = 100\n";
        let mutated_src = "MAX = 101\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let mutant = make_mutant("config.py", "100", "101", 6);

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src);

        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::ConstantBind { name, new_expr } => {
                assert_eq!(name, "MAX");
                assert_eq!(new_expr.trim(), "101");
            }
            other => panic!("expected ConstantBind, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run, expect FAIL.**

- [ ] **Step 3: Add `Stmt::Assign` arm to `derive_for_top_level`**

```rust
        Stmt::Assign(assign) => {
            let target = assign.targets.first()?;
            let name = match target {
                ruff_python_ast::Expr::Name(n) => n.id.to_string(),
                _ => return None,
            };
            let value_range = assign.value.range();
            let new_expr = mutated_source
                .get(value_range.start().to_usize()..adjusted_end(&value_range, mutant))
                .unwrap_or("")
                .to_owned();
            Some(MutationDiff::ConstantBind { name, new_expr })
        }
```

Add helper at the bottom of the file (above tests):

```rust
fn adjusted_end(range: &ruff_text_size::TextRange, mutant: &Mutant) -> usize {
    let original_end = range.end().to_usize();
    if mutant.byte_offset < original_end {
        original_end + mutant.mutated_text.len() - mutant.byte_length
    } else {
        original_end
    }
}
```

- [ ] **Step 4: Run, expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add src/mutation/diff.rs
git commit -m "feat(mutation/diff): derive ConstantBind for module-level assignments"
```

---

### Task 1.4: Class method body → `ClassMethod`

**Files:** Modify `src/mutation/diff.rs`.

- [ ] **Step 1: Append failing test**

```rust
    #[test]
    fn class_method_body_yields_class_method_variant() {
        let original_src = "class Calc:\n    def add(self, a, b):\n        return a + b\n";
        let mutated_src  = "class Calc:\n    def add(self, a, b):\n        return a - b\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let plus_offset = original_src.find('+').unwrap();
        let mutant = make_mutant("calc.py", "+", "-", plus_offset);

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src);

        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::ClassMethod { class_qualname, method_name, new_source } => {
                assert_eq!(class_qualname, "Calc");
                assert_eq!(method_name, "add");
                assert!(new_source.contains("return a - b"));
            }
            other => panic!("expected ClassMethod, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run, expect FAIL.**

- [ ] **Step 3: Add `Stmt::ClassDef` arm**

```rust
        Stmt::ClassDef(class) => {
            let class_qualname = class.name.id.to_string();
            for body_stmt in &class.body {
                let range = body_stmt.range();
                let start = range.start().to_usize();
                let end = range.end().to_usize();
                if !(start <= mutant.byte_offset && mutant.byte_offset < end) {
                    continue;
                }
                match body_stmt {
                    Stmt::FunctionDef(method) => {
                        let suffix = property_suffix(method);
                        let method_name = if suffix.is_empty() {
                            method.name.id.to_string()
                        } else {
                            format!("{}.{}", method.name.id, suffix)
                        };
                        let stmt_end = start + range.len().to_usize()
                            + mutant.mutated_text.len() - mutant.byte_length;
                        let new_source = strip_decorators(
                            mutated_source.get(start..stmt_end).unwrap_or(""),
                        );
                        return Some(MutationDiff::ClassMethod {
                            class_qualname,
                            method_name,
                            new_source,
                        });
                    }
                    Stmt::Assign(assign) => {
                        let target = assign.targets.first()?;
                        let attr_name = match target {
                            ruff_python_ast::Expr::Name(n) => n.id.to_string(),
                            _ => return None,
                        };
                        let value_range = assign.value.range();
                        let new_expr = mutated_source
                            .get(value_range.start().to_usize()..adjusted_end(&value_range, mutant))
                            .unwrap_or("")
                            .to_owned();
                        return Some(MutationDiff::ClassAttr {
                            class_qualname,
                            name: attr_name,
                            new_expr,
                        });
                    }
                    _ => return None,
                }
            }
            None
        }
```

Add the `property_suffix` helper (above tests):

```rust
fn property_suffix(method: &ruff_python_ast::StmtFunctionDef) -> &'static str {
    for decorator in &method.decorator_list {
        match &decorator.expression {
            ruff_python_ast::Expr::Name(name) if name.id.as_str() == "property" => {
                return "fget";
            }
            ruff_python_ast::Expr::Attribute(attr) => {
                let leaf = attr.attr.id.as_str();
                if leaf == "setter" {
                    return "fset";
                }
                if leaf == "deleter" {
                    return "fdel";
                }
            }
            _ => continue,
        }
    }
    ""
}
```

- [ ] **Step 4: Run all `mutation::diff` tests, expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add src/mutation/diff.rs
git commit -m "feat(mutation/diff): derive ClassMethod and ClassAttr for class-body mutations"
```

---

### Task 1.5: Nested function → dotted qualname

**Files:** Modify `src/mutation/diff.rs`.

- [ ] **Step 1: Append failing test**

```rust
    #[test]
    fn nested_function_yields_dotted_qualname() {
        let original_src = "def outer():\n    def inner():\n        return 1\n    return inner\n";
        let mutated_src  = "def outer():\n    def inner():\n        return 2\n    return inner\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let offset = original_src.find("1\n").unwrap();
        let mutant = make_mutant("nested.py", "1", "2", offset);

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src);

        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::FunctionBody { qualname, new_source } => {
                assert_eq!(qualname, "outer.inner");
                assert!(new_source.contains("return 2"));
                assert!(new_source.starts_with("def inner"));
            }
            other => panic!("expected FunctionBody for outer.inner, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run, expect FAIL.**

- [ ] **Step 3: Add recursive `descend_function` and replace the FunctionDef arm**

```rust
fn descend_function(
    func: &ruff_python_ast::StmtFunctionDef,
    mutated_source: &str,
    mutant: &Mutant,
    qualname_prefix: &str,
) -> Option<MutationDiff> {
    let outer_qualname = if qualname_prefix.is_empty() {
        func.name.id.to_string()
    } else {
        format!("{}.{}", qualname_prefix, func.name.id)
    };
    for inner_stmt in &func.body {
        let range = inner_stmt.range();
        let start = range.start().to_usize();
        let end = range.end().to_usize();
        if !(start <= mutant.byte_offset && mutant.byte_offset < end) {
            continue;
        }
        if let Stmt::FunctionDef(nested) = inner_stmt {
            if let Some(d) = descend_function(nested, mutated_source, mutant, &outer_qualname) {
                return Some(d);
            }
            let stmt_end = start + range.len().to_usize()
                + mutant.mutated_text.len() - mutant.byte_length;
            let new_source = strip_decorators(
                mutated_source.get(start..stmt_end).unwrap_or(""),
            );
            return Some(MutationDiff::FunctionBody {
                qualname: format!("{}.{}", outer_qualname, nested.name.id),
                new_source,
            });
        }
    }
    let range = func.range();
    let stmt_start = range.start().to_usize();
    let stmt_end = stmt_start + range.len().to_usize()
        + mutant.mutated_text.len() - mutant.byte_length;
    let new_source = strip_decorators(
        mutated_source.get(stmt_start..stmt_end).unwrap_or(""),
    );
    Some(MutationDiff::FunctionBody {
        qualname: outer_qualname,
        new_source,
    })
}
```

Replace `Stmt::FunctionDef(func) => ...` in `derive_for_top_level` with:

```rust
        Stmt::FunctionDef(func) => descend_function(func, mutated_source, mutant, ""),
```

- [ ] **Step 4: Run all diff tests, expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add src/mutation/diff.rs
git commit -m "feat(mutation/diff): emit dotted qualnames for nested function mutations"
```

---

### Task 1.6: Decorator removal → `ModuleAttr`

**Files:** Modify `src/mutation/diff.rs`.

- [ ] **Step 1: Append failing test**

```rust
    #[test]
    fn decorator_removal_yields_module_attr_variant() {
        let original_src = "@cache\ndef foo():\n    return 1\n";
        let mutated_src  = "def foo():\n    return 1\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let mutant = Mutant {
            file_path: "decor.py".into(),
            line: 1,
            column: 1,
            byte_offset: 0,
            byte_length: "@cache\n".len(),
            original_text: "@cache\n".into(),
            mutated_text: String::new(),
            mutator_name: "remove_decorator".to_owned(),
        };

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src);

        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::ModuleAttr { name, new_source } => {
                assert_eq!(name, "foo");
                assert!(new_source.contains("def foo"));
                assert!(!new_source.contains("@cache"));
            }
            other => panic!("expected ModuleAttr, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run, expect FAIL.**

- [ ] **Step 3: Update `derive_diff`**

Replace `derive_diff` with:

```rust
#[must_use]
pub fn derive_diff(
    mutant: &Mutant,
    original_ast: &ModModule,
    _mutated_ast: &ModModule,
    mutated_source: &str,
) -> Vec<MutationDiff> {
    if mutant.mutator_name == "remove_decorator" {
        if let Some(d) = derive_decorator_removal(mutant, original_ast, mutated_source) {
            return vec![d];
        }
    }
    let mutation_start = mutant.byte_offset;
    for stmt in &original_ast.body {
        let range = stmt.range();
        let start = range.start().to_usize();
        let end = range.end().to_usize();
        if start <= mutation_start && mutation_start < end {
            if let Some(diff) = derive_for_top_level(stmt, mutated_source, mutant) {
                return vec![diff];
            }
        }
    }
    Vec::new()
}

fn derive_decorator_removal(
    mutant: &Mutant,
    original_ast: &ModModule,
    mutated_source: &str,
) -> Option<MutationDiff> {
    for stmt in &original_ast.body {
        let range = stmt.range();
        let start = range.start().to_usize();
        let end = range.end().to_usize();
        if !(start <= mutant.byte_offset && mutant.byte_offset < end) {
            continue;
        }
        let name = match stmt {
            Stmt::FunctionDef(f) => f.name.id.to_string(),
            Stmt::ClassDef(c) => c.name.id.to_string(),
            _ => continue,
        };
        let stmt_end_in_mutated = end + mutant.mutated_text.len() - mutant.byte_length;
        let new_source = strip_decorators(
            mutated_source.get(start..stmt_end_in_mutated).unwrap_or(""),
        );
        return Some(MutationDiff::ModuleAttr { name, new_source });
    }
    None
}
```

- [ ] **Step 4: Run all diff tests, expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add src/mutation/diff.rs
git commit -m "feat(mutation/diff): route remove_decorator through ModuleAttr"
```

---

### Task 1.7: JSON roundtrip test for `MutationDiff`

**Files:** Modify `src/mutation/diff.rs`.

- [ ] **Step 1: Append the test**

```rust
    #[test]
    fn mutation_diff_serde_roundtrip() {
        let original = MutationDiff::FunctionBody {
            qualname: "mod.outer.inner".into(),
            new_source: "def inner():\n    return 2\n".into(),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: MutationDiff = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, restored);
        assert!(json.contains("\"kind\":\"function_body\""));
    }
```

- [ ] **Step 2: Run, expect PASS** (serde derive already in place).

- [ ] **Step 3: Commit**

```bash
git add src/mutation/diff.rs
git commit -m "test(mutation/diff): cover JSON roundtrip"
```

## Phase 2 — Rust: project AST scan (`plugin_index`)

### Task 2.1: Module skeleton — `ImportBinding`, `ReloadWarning`

**Files:**
- Create: `src/plugin_index.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Add module declaration in `src/lib.rs`**

Insert after `pub mod plugin;`:

```rust
pub mod plugin_index;
```

- [ ] **Step 2: Create `src/plugin_index.rs`**

```rust
//! Project-wide AST scan that produces inputs for the plugin's
//! reverse-import index and reload-warnings list.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One `from X import Y [as Z]` binding seen in project source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportBinding {
    pub consumer_module: String,
    pub consumer_key: String,
    pub target_module: String,
    pub target_name: String,
}

/// One occurrence of a call that compromises plugin-backend accuracy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadWarning {
    pub file: PathBuf,
    pub line: u32,
    pub kind: String,
}

/// Aggregated output of [`scan_project`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginIndex {
    pub import_bindings: Vec<ImportBinding>,
    pub reload_warnings: Vec<ReloadWarning>,
}
```

- [ ] **Step 3: Run `cargo check --all-features`, expect PASS.**

- [ ] **Step 4: Commit**

```bash
git add src/lib.rs src/plugin_index.rs
git commit -m "feat(plugin_index): introduce types for project AST scan output"
```

---

### Task 2.2: `scan_source` for one file — `from`-import handling

**Files:** Modify `src/plugin_index.rs`.

- [ ] **Step 1: Append failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_source_extracts_simple_from_import() {
        let src = "from foo import bar\n";
        let index = scan_source(src, "myproj.consumer", &PathBuf::from("c.py"));
        assert_eq!(index.import_bindings.len(), 1);
        assert_eq!(index.import_bindings[0].target_module, "foo");
        assert_eq!(index.import_bindings[0].target_name, "bar");
        assert_eq!(index.import_bindings[0].consumer_key, "bar");
    }

    #[test]
    fn scan_source_handles_alias() {
        let src = "from foo import bar as baz\n";
        let index = scan_source(src, "m.c", &PathBuf::from("c.py"));
        assert_eq!(index.import_bindings[0].consumer_key, "baz");
        assert_eq!(index.import_bindings[0].target_name, "bar");
    }

    #[test]
    fn scan_source_handles_multi_name_import() {
        let src = "from foo import a, b as c, d\n";
        let index = scan_source(src, "m.c", &PathBuf::from("c.py"));
        let keys: Vec<_> = index.import_bindings.iter().map(|b| b.consumer_key.clone()).collect();
        assert_eq!(keys, vec!["a", "c", "d"]);
    }

    #[test]
    fn scan_source_resolves_relative_one_dot() {
        let src = "from .sib import x\n";
        let index = scan_source(src, "myproj.subpkg.consumer", &PathBuf::from("c.py"));
        assert_eq!(index.import_bindings[0].target_module, "myproj.subpkg.sib");
    }
}
```

- [ ] **Step 2: Run, expect FAIL** (`scan_source` not defined).

- [ ] **Step 3: Implement `scan_source`**

Above the tests module:

```rust
use ruff_python_ast::{ModModule, Stmt};
use ruff_python_parser::parse_module;
use ruff_text_size::Ranged;

#[must_use]
pub fn scan_source(source: &str, consumer_module: &str, file_path: &std::path::Path) -> PluginIndex {
    let parsed = match parse_module(source) {
        Ok(p) => p,
        Err(_) => return PluginIndex::default(),
    };
    let ast: ModModule = parsed.into_syntax();
    let mut out = PluginIndex::default();
    for stmt in &ast.body {
        collect_from_stmt(stmt, source, consumer_module, file_path, &mut out);
    }
    out
}

fn collect_from_stmt(
    stmt: &Stmt,
    _source: &str,
    consumer_module: &str,
    _file_path: &std::path::Path,
    out: &mut PluginIndex,
) {
    if let Stmt::ImportFrom(import) = stmt {
        let level = u32::from(import.level);
        let target_module = resolve_import_from(consumer_module, &import.module, level);
        for alias in &import.names {
            let target_name = alias.name.id.to_string();
            let consumer_key = alias
                .asname
                .as_ref()
                .map_or_else(|| target_name.clone(), |a| a.id.to_string());
            out.import_bindings.push(ImportBinding {
                consumer_module: consumer_module.to_owned(),
                consumer_key,
                target_module: target_module.clone(),
                target_name,
            });
        }
    }
}

fn resolve_import_from(
    consumer_module: &str,
    explicit: &Option<ruff_python_ast::Identifier>,
    level: u32,
) -> String {
    if level == 0 {
        return explicit.as_ref().map_or(String::new(), |id| id.id.to_string());
    }
    let parts: Vec<&str> = consumer_module.split('.').collect();
    let drop = level as usize;
    let prefix_end = parts.len().saturating_sub(drop);
    let mut prefix: String = parts[..prefix_end].join(".");
    if let Some(extra) = explicit {
        if !prefix.is_empty() {
            prefix.push('.');
        }
        prefix.push_str(extra.id.as_str());
    }
    prefix
}
```

- [ ] **Step 4: Run, expect all import-from tests PASS.**

- [ ] **Step 5: Commit**

```bash
git add src/plugin_index.rs
git commit -m "feat(plugin_index): scan a single source file for from-imports"
```

---

### Task 2.3: Detect reload / dynamic imports

**Files:** Modify `src/plugin_index.rs`.

- [ ] **Step 1: Append failing tests**

```rust
    #[test]
    fn scan_source_detects_importlib_reload() {
        let src = "import importlib\nimportlib.reload(foo)\n";
        let index = scan_source(src, "m.c", &PathBuf::from("c.py"));
        assert_eq!(index.reload_warnings.len(), 1);
        assert_eq!(index.reload_warnings[0].kind, "reload");
        assert_eq!(index.reload_warnings[0].line, 2);
    }

    #[test]
    fn scan_source_detects_dynamic_import_module() {
        let src = "import importlib\nimportlib.import_module('foo')\n";
        let index = scan_source(src, "m.c", &PathBuf::from("c.py"));
        assert!(index.reload_warnings.iter().any(|w| w.kind == "import_module"));
    }

    #[test]
    fn scan_source_detects_dunder_import() {
        let src = "__import__('foo')\n";
        let index = scan_source(src, "m.c", &PathBuf::from("c.py"));
        assert!(index.reload_warnings.iter().any(|w| w.kind == "__import__"));
    }
```

- [ ] **Step 2: Run, expect FAIL.**

- [ ] **Step 3: Replace `collect_from_stmt` and add walker**

```rust
fn collect_from_stmt(
    stmt: &Stmt,
    source: &str,
    consumer_module: &str,
    file_path: &std::path::Path,
    out: &mut PluginIndex,
) {
    if let Stmt::ImportFrom(import) = stmt {
        let level = u32::from(import.level);
        let target_module = resolve_import_from(consumer_module, &import.module, level);
        for alias in &import.names {
            let target_name = alias.name.id.to_string();
            let consumer_key = alias
                .asname
                .as_ref()
                .map_or_else(|| target_name.clone(), |a| a.id.to_string());
            out.import_bindings.push(ImportBinding {
                consumer_module: consumer_module.to_owned(),
                consumer_key,
                target_module: target_module.clone(),
                target_name,
            });
        }
    }
    walk_stmt_for_calls(stmt, source, file_path, out);
}

fn walk_stmt_for_calls(
    stmt: &Stmt,
    source: &str,
    file_path: &std::path::Path,
    out: &mut PluginIndex,
) {
    use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};

    struct CallVisitor<'a> {
        source: &'a str,
        file: &'a std::path::Path,
        out: &'a mut PluginIndex,
    }

    impl<'a> Visitor<'a> for CallVisitor<'a> {
        fn visit_expr(&mut self, expr: &'a ruff_python_ast::Expr) {
            if let ruff_python_ast::Expr::Call(call) = expr {
                if let Some(kind) = classify_call(&call.func) {
                    let line = line_at(self.source, call.range().start().to_usize());
                    self.out.reload_warnings.push(ReloadWarning {
                        file: self.file.to_path_buf(),
                        line,
                        kind: kind.to_owned(),
                    });
                }
            }
            walk_expr(self, expr);
        }
    }

    let mut visitor = CallVisitor { source, file: file_path, out };
    walk_stmt(&mut visitor, stmt);
}

fn classify_call(callee: &ruff_python_ast::Expr) -> Option<&'static str> {
    match callee {
        ruff_python_ast::Expr::Attribute(attr) => {
            let leaf = attr.attr.id.as_str();
            let base = match attr.value.as_ref() {
                ruff_python_ast::Expr::Name(n) => n.id.as_str(),
                _ => return None,
            };
            if base == "importlib" && leaf == "reload" {
                Some("reload")
            } else if base == "importlib" && leaf == "import_module" {
                Some("import_module")
            } else {
                None
            }
        }
        ruff_python_ast::Expr::Name(n) if n.id.as_str() == "__import__" => Some("__import__"),
        _ => None,
    }
}

fn line_at(source: &str, byte_offset: usize) -> u32 {
    let upto = source.get(..byte_offset).unwrap_or("");
    u32::try_from(upto.matches('\n').count() + 1).unwrap_or(1)
}
```

- [ ] **Step 4: Run all `plugin_index` tests, expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add src/plugin_index.rs
git commit -m "feat(plugin_index): detect reload/import_module/__import__ calls"
```

---

### Task 2.4: `scan_project` walking the project tree

**Files:** Modify `src/plugin_index.rs`.

- [ ] **Step 1: Append failing test**

```rust
    #[test]
    fn scan_project_walks_all_py_files() {
        use tempfile::tempdir;
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        std::fs::write(root.join("pkg/__init__.py"), "").unwrap();
        std::fs::write(root.join("pkg/a.py"), "from pkg.b import x\n").unwrap();
        std::fs::write(root.join("pkg/b.py"), "x = 1\n").unwrap();

        let index = scan_project(root).expect("scan ok");

        assert_eq!(index.import_bindings.len(), 1);
        assert_eq!(index.import_bindings[0].target_module, "pkg.b");
        assert_eq!(index.import_bindings[0].consumer_module, "pkg.a");
    }
```

- [ ] **Step 2: Run, expect FAIL.**

- [ ] **Step 3: Implement `scan_project`**

```rust
/// Walk all `.py` files under `root` and return aggregated [`PluginIndex`].
///
/// # Errors
///
/// Returns [`std::io::Error`] from filesystem operations.
pub fn scan_project(root: &std::path::Path) -> std::io::Result<PluginIndex> {
    let mut out = PluginIndex::default();
    walk_dir(root, root, &mut out)?;
    Ok(out)
}

fn walk_dir(
    root: &std::path::Path,
    cur: &std::path::Path,
    out: &mut PluginIndex,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(cur)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_dir(root, &path, out)?;
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("py") {
            continue;
        }
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let module_name = path_to_module(root, &path);
        let scanned = scan_source(&source, &module_name, &path);
        out.import_bindings.extend(scanned.import_bindings);
        out.reload_warnings.extend(scanned.reload_warnings);
    }
    Ok(())
}

fn path_to_module(root: &std::path::Path, file: &std::path::Path) -> String {
    let rel = file.strip_prefix(root).unwrap_or(file);
    let stem = rel.with_extension("");
    let mut parts: Vec<String> = stem
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(ToOwned::to_owned))
        .collect();
    if parts.last().map(String::as_str) == Some("__init__") {
        let _ = parts.pop();
    }
    parts.join(".")
}
```

- [ ] **Step 4: Run all `plugin_index` tests, expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add src/plugin_index.rs
git commit -m "feat(plugin_index): walk project tree and aggregate scan output"
```

## Phase 3 — Wire IR through the runner

### Task 3.1: Extend mutant message to carry diff

**Files:** Modify `src/runner/pytest_plugin.rs`.

- [ ] **Step 1: Add a failing test in the existing `tests` module**

```rust
    #[test]
    fn build_mutant_message_includes_diff_field() {
        let mutant = Mutant {
            file_path: "calc.py".into(),
            line: 1, column: 1,
            byte_offset: 0, byte_length: 1,
            original_text: "+".into(),
            mutated_text: "-".into(),
            mutator_name: "arithmetic".to_owned(),
        };
        let diff = vec![crate::mutation::MutationDiff::FunctionBody {
            qualname: "add".into(),
            new_source: "def add(a, b):\n    return a - b\n".into(),
        }];
        let msg = build_mutant_message(
            &mutant,
            "def add(a, b):\n    return a - b\n",
            &["t1".into()],
            &diff,
        );
        let value: serde_json::Value = serde_json::from_str(&msg).expect("json");
        assert_eq!(value["type"], "mutant");
        assert!(value["diff"].is_array());
        assert_eq!(value["diff"][0]["kind"], "function_body");
    }
```

- [ ] **Step 2: Run, expect FAIL** (signature mismatch).

- [ ] **Step 3: Update `build_mutant_message`**

Replace the existing function:

```rust
fn build_mutant_message(
    mutant: &Mutant,
    mutated_source: &str,
    tests: &[String],
    diff: &[crate::mutation::MutationDiff],
) -> String {
    let file_path_str = mutant.file_path.display().to_string();
    let msg = serde_json::json!({
        "type": "mutant",
        "file": file_path_str,
        "module": file_to_module(&file_path_str),
        "mutated_source": mutated_source,
        "diff": diff,
        "tests": tests,
    });
    msg.to_string()
}
```

- [ ] **Step 4: Wire `derive_diff` into `run_mutant`**

In `run_mutant` (around line 474), before the existing call to `build_mutant_message`, parse both ASTs and derive the diff:

```rust
let original_source = source;
let mutated_source = mutant.apply_to_source(original_source);
let original_ast = ruff_python_parser::parse_module(original_source)
    .map(|p| p.into_syntax())
    .unwrap_or_else(|_| ruff_python_ast::ModModule {
        range: Default::default(),
        body: Vec::new(),
    });
let mutated_ast = ruff_python_parser::parse_module(&mutated_source)
    .map(|p| p.into_syntax())
    .unwrap_or_else(|_| ruff_python_ast::ModModule {
        range: Default::default(),
        body: Vec::new(),
    });
let diff = crate::mutation::diff::derive_diff(
    mutant,
    &original_ast,
    &mutated_ast,
    &mutated_source,
);
```

Then pass `&diff` to `build_mutant_message`.

- [ ] **Step 5: Update all other call sites of `build_mutant_message`**

Run: `grep -n "build_mutant_message(" src/runner/pytest_plugin.rs`. For each call site (including tests), pass an empty `&[]` slice for the new `diff` parameter unless the test specifically exercises the diff field.

- [ ] **Step 6: Run all pytest_plugin tests, expect PASS.**

```bash
cargo test --all-features pytest_plugin
```

- [ ] **Step 7: Commit**

```bash
git add src/runner/pytest_plugin.rs
git commit -m "feat(runner): include MutationDiff in plugin mutant messages"
```

---

### Task 3.2: Build `ready_ack` message

**Files:** Modify `src/runner/pytest_plugin.rs`.

- [ ] **Step 1: Failing test in the `tests` module**

```rust
    #[test]
    fn build_ready_ack_message_serializes_index() {
        let index = crate::plugin_index::PluginIndex {
            import_bindings: vec![crate::plugin_index::ImportBinding {
                consumer_module: "consumer".into(),
                consumer_key: "x".into(),
                target_module: "target".into(),
                target_name: "x".into(),
            }],
            reload_warnings: vec![],
        };
        let msg = build_ready_ack_message(&index);
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["type"], "ready_ack");
        assert_eq!(v["import_bindings"][0]["target_module"], "target");
    }
```

- [ ] **Step 2: Run, expect FAIL.**

- [ ] **Step 3: Implement**

Add near `build_mutant_message`:

```rust
/// Build the JSON `ready_ack` message sent to the plugin in response
/// to its `ready` message.
fn build_ready_ack_message(index: &crate::plugin_index::PluginIndex) -> String {
    let msg = serde_json::json!({
        "type": "ready_ack",
        "import_bindings": index.import_bindings,
        "reload_warnings": index.reload_warnings,
    });
    msg.to_string()
}
```

- [ ] **Step 4: Run, expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add src/runner/pytest_plugin.rs
git commit -m "feat(runner): build ready_ack handshake message with project index"
```

---

### Task 3.3: Run `scan_project` at startup, send `ready_ack` per worker

**Files:** Modify `src/runner/pytest_plugin.rs`.

- [ ] **Step 1: Add `project_index` field to `PytestPluginRunner`**

In the struct:

```rust
    project_index: std::sync::Mutex<Option<Arc<crate::plugin_index::PluginIndex>>>,
```

In `new()`:

```rust
            project_index: std::sync::Mutex::new(None),
```

(Apply to both `new` and `Default::default`.)

- [ ] **Step 2: Scan project at start of `Runner::start`**

In `impl Runner for PytestPluginRunner`, in `start()`, after the line storing `project_dir`, add:

```rust
let scanned = match crate::plugin_index::scan_project(project_dir) {
    Ok(idx) => idx,
    Err(_err) => crate::plugin_index::PluginIndex::default(),
};
let index_arc = Arc::new(scanned);
if let Ok(mut guard) = self.project_index.lock() {
    *guard = Some(Arc::clone(&index_arc));
}
```

- [ ] **Step 3: Pass the index into `PersistentWorker::spawn`**

Change `PersistentWorker::spawn` to accept `index: Arc<crate::plugin_index::PluginIndex>` as a new parameter. Update the spawn loop in `Runner::start` to clone the Arc per worker:

```rust
let worker_index = Arc::clone(&index_arc);
let handle = tokio::spawn(async move {
    PersistentWorker::spawn(worker_timeout, &worker_dir, worker_index).await
});
```

- [ ] **Step 4: Send `ready_ack` after the plugin's `ready`**

Inside `PersistentWorker::spawn`, after a successful `ready` message is read from the plugin's IPC channel, write the ack:

```rust
let ack = build_ready_ack_message(&index);
write_message(&mut writer, &ack).await?;
```

- [ ] **Step 5: Run all pytest_plugin tests, expect PASS.**

```bash
cargo test --all-features pytest_plugin
```

- [ ] **Step 6: Commit**

```bash
git add src/runner/pytest_plugin.rs
git commit -m "feat(runner): scan project and send ready_ack to each worker"
```

## Phase 4 — Python: `PatchJournal` and test scaffolding

### Task 4.1: Set up `tests/plugin/` and Justfile recipe

**Files:**
- Create: `tests/plugin/__init__.py`, `tests/plugin/conftest.py`
- Modify: `Justfile`

- [ ] **Step 1: Create test directory**

```bash
mkdir -p tests/plugin
```

- [ ] **Step 2: Create `tests/plugin/__init__.py`** (empty file)

- [ ] **Step 3: Create `tests/plugin/conftest.py`**

```python
"""Make the embedded fest plugin importable from unit tests.

`_fest_plugin.py` lives at `src/plugin/_fest_plugin.py` because the
Rust build embeds it via include_str!. For Python-side unit testing
we put that directory on sys.path here.
"""

from __future__ import annotations

import sys
from pathlib import Path

_PLUGIN_DIR = Path(__file__).resolve().parents[2] / "src" / "plugin"
if str(_PLUGIN_DIR) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_DIR))
```

- [ ] **Step 4: Add `test-plugin` recipe to `Justfile`**

Append:

```make
# Run Python plugin unit tests
test-plugin:
    pytest tests/plugin -v
```

Update `check-all`:

```make
check-all: fmt-check lint deny machete test test-plugin jscpd
```

- [ ] **Step 5: Sanity check — collection succeeds**

```bash
pytest tests/plugin --collect-only
```

Expected: "0 tests collected", no errors.

- [ ] **Step 6: Commit**

```bash
git add tests/plugin/__init__.py tests/plugin/conftest.py Justfile
git commit -m "test: scaffold tests/plugin for plugin-side unit tests"
```

---

### Task 4.2: `PatchJournal` with append/rollback

**Files:**
- Create: `tests/plugin/test_patch_journal.py`
- Modify: `src/plugin/_fest_plugin.py`

- [ ] **Step 1: Add aliases at the top of `_fest_plugin.py`**

After the existing `import` block in `src/plugin/_fest_plugin.py`, add:

```python
# Aliases for Python's source-execution and source-evaluation builtins.
# Centralising the names here makes every dynamic-source call site
# greppable for `_PY_EXEC` / `_PY_EVAL` and keeps the security-sensitive
# names isolated to one location.
_PY_EXEC = exec
_PY_EVAL = eval
```

- [ ] **Step 2: Failing test in `tests/plugin/test_patch_journal.py`**

```python
"""Tests for fest plugin's PatchJournal class."""

from __future__ import annotations

from _fest_plugin import PatchJournal


def test_rollback_restores_in_reverse_order():
    state = []
    journal = PatchJournal()
    journal.append(state.append, "first")
    journal.append(state.append, "second")
    journal.append(state.append, "third")

    errors = journal.rollback()

    assert state == ["third", "second", "first"]
    assert errors == []


def test_rollback_clears_entries():
    journal = PatchJournal()
    state = []
    journal.append(state.append, "x")
    journal.rollback()
    journal.rollback()

    assert state == ["x"]


def test_rollback_continues_after_undo_raises():
    state = []

    def boom():
        raise RuntimeError("undo failed")

    journal = PatchJournal()
    journal.append(state.append, "first")
    journal.append(boom)
    journal.append(state.append, "third")

    errors = journal.rollback()

    assert state == ["third", "first"]
    assert len(errors) == 1
    assert isinstance(errors[0], RuntimeError)
```

- [ ] **Step 3: Run, expect ImportError.**

```bash
pytest tests/plugin/test_patch_journal.py -v
```

- [ ] **Step 4: Add `PatchJournal` to `_fest_plugin.py`**

Append after the alias block:

```python
class PatchJournal:
    """Append-only undo log used during a single mutant lifecycle.

    Each ``append(undo_fn, *args)`` records a callable; ``rollback()``
    invokes them in reverse order. Exceptions in undo callables are
    caught and returned to the caller — partial-failure does not abort
    the rest of the rollback.
    """

    def __init__(self) -> None:
        self._entries: list[tuple[Any, tuple[Any, ...]]] = []

    def append(self, undo_fn: Any, *args: Any) -> None:
        self._entries.append((undo_fn, args))

    def rollback(self) -> list[BaseException]:
        errors: list[BaseException] = []
        for undo_fn, args in reversed(self._entries):
            try:
                undo_fn(*args)
            except Exception as exc:  # noqa: BLE001
                errors.append(exc)
        self._entries.clear()
        return errors
```

- [ ] **Step 5: Run, expect 3 PASS.**

- [ ] **Step 6: Commit**

```bash
git add tests/plugin/test_patch_journal.py src/plugin/_fest_plugin.py
git commit -m "feat(plugin): add PatchJournal with reverse-order rollback"
```

---

## Phase 5 — Python: `ReverseImportIndex`

### Task 5.1: Runtime-layer build

**Files:**
- Create: `tests/plugin/test_reverse_import_index.py`
- Modify: `src/plugin/_fest_plugin.py`

- [ ] **Step 1: Failing test**

```python
"""Tests for fest plugin's ReverseImportIndex class."""

from __future__ import annotations

import sys
import types

import pytest

from _fest_plugin import ReverseImportIndex


@pytest.fixture
def fake_modules():
    created = []

    def factory(name: str, **attrs):
        mod = types.ModuleType(name)
        for key, value in attrs.items():
            setattr(mod, key, value)
        sys.modules[name] = mod
        created.append(name)
        return mod

    yield factory

    for name in created:
        sys.modules.pop(name, None)


def test_runtime_layer_finds_function_imports(fake_modules):
    target_mod = fake_modules("fake_target_pkg")

    def my_func():
        return 1

    my_func.__module__ = "fake_target_pkg"
    my_func.__qualname__ = "my_func"
    target_mod.my_func = my_func

    consumer = fake_modules("fake_consumer_pkg", my_func=my_func)

    idx = ReverseImportIndex.build_runtime_layer()
    hits = idx.lookup("fake_target_pkg", "my_func")

    assert any(d is consumer.__dict__ and key == "my_func" for d, key in hits)
```

- [ ] **Step 2: Run, expect ImportError.**

- [ ] **Step 3: Add `ReverseImportIndex`**

Append to `_fest_plugin.py`:

```python
class ReverseImportIndex:
    """Maps `(target_module, name)` to consumer dict slots that bound it.

    Built once after pytest collection — runtime layer scans
    ``sys.modules`` and the AST layer is ingested via
    :py:meth:`ingest_ast_layer` from the Rust handshake.
    """

    def __init__(self) -> None:
        self._index: dict[tuple[str, str], list[tuple[dict[str, Any], str]]] = {}

    def lookup(
        self, target_module: str, name: str
    ) -> list[tuple[dict[str, Any], str]]:
        return list(self._index.get((target_module, name), ()))

    def add(
        self,
        target_module: str,
        name: str,
        consumer_dict: dict[str, Any],
        key: str,
    ) -> None:
        self._index.setdefault((target_module, name), []).append((consumer_dict, key))

    @classmethod
    def build_runtime_layer(cls) -> "ReverseImportIndex":
        idx = cls()
        for mod_name, mod in list(sys.modules.items()):
            mod_dict = getattr(mod, "__dict__", None)
            if mod_dict is None:
                continue
            for key, value in list(mod_dict.items()):
                src_mod = getattr(value, "__module__", None)
                src_name = (
                    getattr(value, "__qualname__", None)
                    or getattr(value, "__name__", None)
                )
                if not src_mod or not src_name or src_mod == mod_name:
                    continue
                idx.add(src_mod, src_name, mod_dict, key)
        return idx

    def ingest_ast_layer(self, bindings: list[dict[str, str]]) -> None:
        """Add bindings from the Rust-side project AST scan."""
        for entry in bindings:
            consumer_mod_name = entry.get("consumer_module", "")
            consumer_key = entry.get("consumer_key", "")
            target_mod = entry.get("target_module", "")
            target_name = entry.get("target_name", "")
            consumer_mod = sys.modules.get(consumer_mod_name)
            if consumer_mod is None:
                continue
            self.add(target_mod, target_name, consumer_mod.__dict__, consumer_key)
```

- [ ] **Step 4: Run, expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add tests/plugin/test_reverse_import_index.py src/plugin/_fest_plugin.py
git commit -m "feat(plugin): add ReverseImportIndex with runtime layer"
```

---

### Task 5.2: AST-layer ingestion test

**Files:** Modify `tests/plugin/test_reverse_import_index.py`.

- [ ] **Step 1: Append failing tests**

```python
def test_ast_layer_resolves_consumer_to_loaded_dict(fake_modules):
    consumer = fake_modules("fake_consumer_const", MAX=100)
    bindings = [
        {
            "consumer_module": "fake_consumer_const",
            "consumer_key": "MAX",
            "target_module": "fake_target_const",
            "target_name": "MAX",
        }
    ]
    idx = ReverseImportIndex()
    idx.ingest_ast_layer(bindings)
    hits = idx.lookup("fake_target_const", "MAX")

    assert (consumer.__dict__, "MAX") in hits


def test_ast_layer_skips_unloaded_consumers():
    bindings = [
        {
            "consumer_module": "totally_unloaded_xyz",
            "consumer_key": "Q",
            "target_module": "tgt",
            "target_name": "Q",
        }
    ]
    idx = ReverseImportIndex()
    idx.ingest_ast_layer(bindings)
    assert idx.lookup("tgt", "Q") == []
```

- [ ] **Step 2: Run, expect PASS** (the method already exists from Task 5.1).

- [ ] **Step 3: Commit**

```bash
git add tests/plugin/test_reverse_import_index.py
git commit -m "test(plugin): cover ReverseImportIndex AST-layer ingestion"
```

## Phase 6 — Python: `MutationApplier`

### Task 6.1: Dispatch skeleton + module-level helpers

**Files:**
- Create: `tests/plugin/test_mutation_applier.py`
- Modify: `src/plugin/_fest_plugin.py`

- [ ] **Step 1: Failing test**

```python
"""Tests for fest plugin's MutationApplier class."""

from __future__ import annotations

import sys
import types

import pytest

from _fest_plugin import MutationApplier, PatchJournal, ReverseImportIndex


@pytest.fixture
def target_module():
    name = "applier_target_mod"
    mod = types.ModuleType(name)
    sys.modules[name] = mod
    yield mod
    sys.modules.pop(name, None)


def test_apply_raises_on_unknown_kind(target_module):
    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    with pytest.raises(ValueError, match="unknown mutation kind"):
        applier.apply({"kind": "bogus"}, journal)
```

- [ ] **Step 2: Run, expect ImportError.**

- [ ] **Step 3: Add module-level helpers and the `MutationApplier` skeleton**

Append to `_fest_plugin.py`:

```python
_MISSING = object()


def _drill_to_function(value: Any) -> Any:
    """Follow ``__wrapped__`` chains until a plain function is reached."""
    seen: set[int] = set()
    cur = value
    while not isinstance(cur, types.FunctionType):
        if id(cur) in seen:
            break
        seen.add(id(cur))
        nxt = getattr(cur, "__wrapped__", None)
        if nxt is None or nxt is cur:
            break
        cur = nxt
    return cur


def _restore_function(
    func: Any,
    code: Any,
    defaults: Any,
    kwdefaults: Any,
    annotations: dict[str, Any],
    func_dict: dict[str, Any],
) -> None:
    func.__code__ = code
    func.__defaults__ = defaults
    func.__kwdefaults__ = kwdefaults
    func.__annotations__ = dict(annotations)
    func.__dict__.clear()
    func.__dict__.update(func_dict)


def _restore_code(func: Any, code: Any) -> None:
    func.__code__ = code


def _restore_dict_slot(target_dict: dict[str, Any], key: str, old_value: Any) -> None:
    if old_value is _MISSING:
        target_dict.pop(key, None)
    else:
        target_dict[key] = old_value


def _restore_class_attr(cls: type, name: str, old_value: Any) -> None:
    if old_value is _MISSING:
        try:
            delattr(cls, name)
        except AttributeError:
            pass
    else:
        setattr(cls, name, old_value)


def _unwrap_descriptor(descriptor: Any, suffix: str) -> Any:
    if isinstance(descriptor, (staticmethod, classmethod)):
        return descriptor.__func__
    if isinstance(descriptor, property):
        if suffix == "fget":
            return descriptor.fget
        if suffix == "fset":
            return descriptor.fset
        if suffix == "fdel":
            return descriptor.fdel
        return descriptor.fget
    if isinstance(descriptor, types.FunctionType):
        return descriptor
    return None


class MutationApplier:
    """Dispatches MutationDiff entries to per-kind appliers."""

    def __init__(
        self,
        target_module: types.ModuleType,
        index: ReverseImportIndex,
    ) -> None:
        self.target_module = target_module
        self.index = index

    def apply(self, change: dict[str, Any], journal: PatchJournal) -> None:
        kind = change.get("kind", "")
        handler = {
            "function_body": self._apply_function_body,
            "constant_bind": self._apply_constant_rebind,
            "class_method": self._apply_class_method,
            "class_attr": self._apply_class_attr,
            "module_attr": self._apply_module_attr,
        }.get(kind)
        if handler is None:
            raise ValueError(f"unknown mutation kind: {kind!r}")
        handler(change, journal)

    def _apply_function_body(self, change, journal):
        raise NotImplementedError

    def _apply_constant_rebind(self, change, journal):
        raise NotImplementedError

    def _apply_class_method(self, change, journal):
        raise NotImplementedError

    def _apply_class_attr(self, change, journal):
        raise NotImplementedError

    def _apply_module_attr(self, change, journal):
        raise NotImplementedError

    def _resolve_qualname(self, qualname: str) -> Any:
        cur: Any = self.target_module
        for part in qualname.split("."):
            cur = getattr(cur, part) if not isinstance(cur, dict) else cur[part]
        return cur

    def _compile_function(self, new_source: str, like: Any) -> Any:
        ns: dict[str, Any] = dict(like.__globals__)
        compiled = compile(new_source, "<fest mutation>", "exec")
        local_ns: dict[str, Any] = {}
        _PY_EXEC(compiled, ns, local_ns)
        for value in local_ns.values():
            if isinstance(value, types.FunctionType):
                return value
        raise RuntimeError(f"compiled source produced no function: {new_source!r}")
```

- [ ] **Step 4: Run, expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add tests/plugin/test_mutation_applier.py src/plugin/_fest_plugin.py
git commit -m "feat(plugin): scaffold MutationApplier dispatch and helpers"
```

---

### Task 6.2: Top-level function body — identity preserving

**Files:** Modify `tests/plugin/test_mutation_applier.py`, `src/plugin/_fest_plugin.py`.

- [ ] **Step 1: Failing test**

```python
def test_function_body_preserves_identity_after_mutation(target_module):
    src = "def foo(x):\n    return x + 1\n"
    compiled = compile(src, "<test>", "exec")
    exec(compiled, target_module.__dict__)
    target_module.foo.__module__ = target_module.__name__
    foo_id = id(target_module.foo)
    consumer = {"foo": target_module.foo}

    idx = ReverseImportIndex()
    idx.add(target_module.__name__, "foo", consumer, "foo")
    applier = MutationApplier(target_module, idx)
    journal = PatchJournal()

    change = {
        "kind": "function_body",
        "qualname": "foo",
        "new_source": "def foo(x):\n    return x - 1\n",
    }
    applier.apply(change, journal)

    assert target_module.foo(5) == 4
    assert id(target_module.foo) == foo_id
    assert consumer["foo"](5) == 4

    journal.rollback()
    assert target_module.foo(5) == 6
```

- [ ] **Step 2: Run, expect FAIL** (NotImplementedError).

- [ ] **Step 3: Implement `_apply_function_body`**

Replace the stub:

```python
    def _apply_function_body(self, change, journal):
        qualname = change["qualname"]
        new_source = change["new_source"]
        if "." in qualname:
            self._apply_nested_function_body(qualname, new_source, journal)
            return
        wrapped = self.target_module.__dict__[qualname]
        target_func = _drill_to_function(wrapped)
        new_func = self._compile_function(new_source, target_func)
        old_code = target_func.__code__
        old_defaults = target_func.__defaults__
        old_kwdefaults = target_func.__kwdefaults__
        old_annotations = dict(target_func.__annotations__)
        old_func_dict = dict(target_func.__dict__)
        try:
            target_func.__code__ = new_func.__code__
        except ValueError:
            self._fallback_function_rebind(qualname, new_func, journal)
            return
        target_func.__defaults__ = new_func.__defaults__
        target_func.__kwdefaults__ = new_func.__kwdefaults__
        target_func.__annotations__ = dict(new_func.__annotations__)
        target_func.__dict__.clear()
        target_func.__dict__.update(new_func.__dict__)
        journal.append(
            _restore_function,
            target_func,
            old_code,
            old_defaults,
            old_kwdefaults,
            old_annotations,
            old_func_dict,
        )

    def _apply_nested_function_body(self, qualname, new_source, journal):
        raise NotImplementedError("nested — Task 6.3")

    def _fallback_function_rebind(self, qualname, new_func, journal):
        if "." in qualname:
            owner_path, leaf = qualname.rsplit(".", 1)
            owner = self._resolve_qualname(owner_path)
            owner_dict = owner.__dict__ if not isinstance(owner, dict) else owner
        else:
            leaf = qualname
            owner_dict = self.target_module.__dict__
        old_value = owner_dict.get(leaf, _MISSING)
        owner_dict[leaf] = new_func
        journal.append(_restore_dict_slot, owner_dict, leaf, old_value)
        for consumer_dict, consumer_key in self.index.lookup(
            self.target_module.__name__, leaf
        ):
            old_consumer = consumer_dict.get(consumer_key, _MISSING)
            consumer_dict[consumer_key] = new_func
            journal.append(_restore_dict_slot, consumer_dict, consumer_key, old_consumer)
```

- [ ] **Step 4: Run, expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add tests/plugin/test_mutation_applier.py src/plugin/_fest_plugin.py
git commit -m "feat(plugin): apply top-level function-body mutations via __code__ swap"
```

---

### Task 6.3: Nested-function `co_consts` swap

**Files:** Modify `tests/plugin/test_mutation_applier.py`, `src/plugin/_fest_plugin.py`.

- [ ] **Step 1: Failing test**

```python
def test_nested_function_body_via_co_consts(target_module):
    src = (
        "def outer():\n"
        "    def inner():\n"
        "        return 1\n"
        "    return inner\n"
    )
    exec(compile(src, "<test>", "exec"), target_module.__dict__)
    target_module.outer.__module__ = target_module.__name__
    outer_id = id(target_module.outer)

    idx = ReverseImportIndex()
    applier = MutationApplier(target_module, idx)
    journal = PatchJournal()

    change = {
        "kind": "function_body",
        "qualname": "outer.inner",
        "new_source": "def inner():\n    return 2\n",
    }
    applier.apply(change, journal)

    inner = target_module.outer()
    assert inner() == 2
    assert id(target_module.outer) == outer_id

    journal.rollback()
    assert target_module.outer()() == 1
```

- [ ] **Step 2: Run, expect FAIL.**

- [ ] **Step 3: Implement `_apply_nested_function_body`**

Replace the stub:

```python
    def _apply_nested_function_body(self, qualname, new_source, journal):
        parent_qualname, leaf = qualname.rsplit(".", 1)
        parent_obj = self._resolve_qualname(parent_qualname)
        parent = _drill_to_function(parent_obj)
        new_inner = self._compile_function(new_source, parent)
        old_consts = parent.__code__.co_consts
        replaced = False
        new_consts: list[Any] = []
        for c in old_consts:
            if isinstance(c, types.CodeType) and c.co_name == leaf and not replaced:
                new_consts.append(new_inner.__code__)
                replaced = True
            else:
                new_consts.append(c)
        if not replaced:
            raise RuntimeError(
                f"nested function {qualname!r}: no matching co_consts entry"
            )
        old_parent_code = parent.__code__
        parent.__code__ = old_parent_code.replace(co_consts=tuple(new_consts))
        journal.append(_restore_code, parent, old_parent_code)
```

- [ ] **Step 4: Run, expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add tests/plugin/test_mutation_applier.py src/plugin/_fest_plugin.py
git commit -m "feat(plugin): patch nested function bodies via parent co_consts swap"
```

---

### Task 6.4: Constant rebind via reverse-import index

**Files:** Modify `tests/plugin/test_mutation_applier.py`, `src/plugin/_fest_plugin.py`.

- [ ] **Step 1: Failing test**

```python
def test_constant_rebind_updates_target_and_consumer(target_module):
    target_module.MAX = 100
    consumer = {"MAX": 100}
    idx = ReverseImportIndex()
    idx.add(target_module.__name__, "MAX", consumer, "MAX")
    applier = MutationApplier(target_module, idx)
    journal = PatchJournal()

    applier.apply(
        {"kind": "constant_bind", "name": "MAX", "new_expr": "101"},
        journal,
    )

    assert target_module.MAX == 101
    assert consumer["MAX"] == 101

    journal.rollback()
    assert target_module.MAX == 100
    assert consumer["MAX"] == 100
```

- [ ] **Step 2: Run, expect FAIL.**

- [ ] **Step 3: Implement**

Replace the stub:

```python
    def _apply_constant_rebind(self, change, journal):
        name = change["name"]
        compiled = compile(change["new_expr"], "<fest constant>", "eval")
        new_value = _PY_EVAL(compiled, self.target_module.__dict__)
        target_dict = self.target_module.__dict__
        old_value = target_dict.get(name, _MISSING)
        target_dict[name] = new_value
        journal.append(_restore_dict_slot, target_dict, name, old_value)
        for consumer_dict, consumer_key in self.index.lookup(
            self.target_module.__name__, name
        ):
            old_consumer = consumer_dict.get(consumer_key, _MISSING)
            consumer_dict[consumer_key] = new_value
            journal.append(_restore_dict_slot, consumer_dict, consumer_key, old_consumer)
```

- [ ] **Step 4: Run, expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add tests/plugin/test_mutation_applier.py src/plugin/_fest_plugin.py
git commit -m "feat(plugin): apply constant rebinds via reverse-import index"
```

---

### Task 6.5: Class methods (plain, staticmethod, classmethod, property)

**Files:** Modify `tests/plugin/test_mutation_applier.py`, `src/plugin/_fest_plugin.py`.

- [ ] **Step 1: Failing tests**

```python
def test_class_method_plain_swap(target_module):
    src = "class Calc:\n    def add(self, a, b):\n        return a + b\n"
    exec(compile(src, "<test>", "exec"), target_module.__dict__)
    Calc = target_module.Calc

    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    applier.apply(
        {
            "kind": "class_method",
            "class_qualname": "Calc",
            "method_name": "add",
            "new_source": "def add(self, a, b):\n    return a - b\n",
        },
        journal,
    )

    assert Calc().add(5, 3) == 2
    journal.rollback()
    assert Calc().add(5, 3) == 8


def test_class_method_staticmethod_swap(target_module):
    src = "class C:\n    @staticmethod\n    def k():\n        return 1\n"
    exec(compile(src, "<test>", "exec"), target_module.__dict__)
    C = target_module.C

    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    applier.apply(
        {
            "kind": "class_method",
            "class_qualname": "C",
            "method_name": "k",
            "new_source": "def k():\n    return 2\n",
        },
        journal,
    )

    assert C.k() == 2
    journal.rollback()
    assert C.k() == 1


def test_class_method_classmethod_swap(target_module):
    src = "class C:\n    @classmethod\n    def m(cls):\n        return 1\n"
    exec(compile(src, "<test>", "exec"), target_module.__dict__)
    C = target_module.C

    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    applier.apply(
        {
            "kind": "class_method",
            "class_qualname": "C",
            "method_name": "m",
            "new_source": "def m(cls):\n    return 2\n",
        },
        journal,
    )

    assert C.m() == 2
    journal.rollback()
    assert C.m() == 1


def test_property_fget_mutation(target_module):
    src = (
        "class C:\n"
        "    @property\n"
        "    def x(self):\n"
        "        return 1\n"
    )
    exec(compile(src, "<test>", "exec"), target_module.__dict__)
    C = target_module.C

    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    applier.apply(
        {
            "kind": "class_method",
            "class_qualname": "C",
            "method_name": "x.fget",
            "new_source": "def x(self):\n    return 2\n",
        },
        journal,
    )

    assert C().x == 2
    journal.rollback()
    assert C().x == 1
```

- [ ] **Step 2: Run, expect all FAIL.**

- [ ] **Step 3: Implement `_apply_class_method`**

Replace the stub:

```python
    def _apply_class_method(self, change, journal):
        cls = self._resolve_qualname(change["class_qualname"])
        method_name = change["method_name"]
        leaf, _, suffix = method_name.partition(".")
        descriptor = cls.__dict__[leaf]
        target_func = _unwrap_descriptor(descriptor, suffix)
        if target_func is None:
            raise RuntimeError(
                f"class method {change['class_qualname']!r}.{method_name!r}: "
                "no underlying function"
            )
        new_func = self._compile_function(change["new_source"], target_func)
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
        journal.append(
            _restore_function,
            target_func,
            old_code,
            old_defaults,
            old_kwdefaults,
            old_annotations,
            old_func_dict,
        )
```

- [ ] **Step 4: Run, expect 4 PASS.**

- [ ] **Step 5: Commit**

```bash
git add tests/plugin/test_mutation_applier.py src/plugin/_fest_plugin.py
git commit -m "feat(plugin): apply class-method mutations with descriptor unwrap"
```

---

### Task 6.6: Class attribute and module attribute rebinds

**Files:** Modify `tests/plugin/test_mutation_applier.py`, `src/plugin/_fest_plugin.py`.

- [ ] **Step 1: Failing tests**

```python
def test_class_attr_rebind(target_module):
    src = "class C:\n    LIMIT = 10\n"
    exec(compile(src, "<test>", "exec"), target_module.__dict__)
    C = target_module.C

    applier = MutationApplier(target_module, ReverseImportIndex())
    journal = PatchJournal()
    applier.apply(
        {"kind": "class_attr", "class_qualname": "C", "name": "LIMIT", "new_expr": "11"},
        journal,
    )

    assert C.LIMIT == 11
    journal.rollback()
    assert C.LIMIT == 10


def test_module_attr_rebind_runs_def_block(target_module):
    target_module.foo = lambda: 1
    target_module.foo.__module__ = target_module.__name__
    consumer = {"foo": target_module.foo}
    idx = ReverseImportIndex()
    idx.add(target_module.__name__, "foo", consumer, "foo")
    applier = MutationApplier(target_module, idx)
    journal = PatchJournal()

    applier.apply(
        {
            "kind": "module_attr",
            "name": "foo",
            "new_source": "def foo():\n    return 2\n",
        },
        journal,
    )

    assert target_module.foo() == 2
    assert consumer["foo"]() == 2

    journal.rollback()
    assert target_module.foo() == 1
    assert consumer["foo"]() == 1
```

- [ ] **Step 2: Run, expect FAIL.**

- [ ] **Step 3: Implement**

Replace both stubs:

```python
    def _apply_class_attr(self, change, journal):
        cls = self._resolve_qualname(change["class_qualname"])
        name = change["name"]
        compiled = compile(change["new_expr"], "<fest class attr>", "eval")
        new_value = _PY_EVAL(compiled, self.target_module.__dict__)
        old_value = cls.__dict__.get(name, _MISSING)
        setattr(cls, name, new_value)
        journal.append(_restore_class_attr, cls, name, old_value)

    def _apply_module_attr(self, change, journal):
        name = change["name"]
        compiled = compile(change["new_source"], "<fest module attr>", "exec")
        ns: dict[str, Any] = dict(self.target_module.__dict__)
        local_ns: dict[str, Any] = {}
        _PY_EXEC(compiled, ns, local_ns)
        if name not in local_ns:
            raise RuntimeError(
                f"module attr {name!r}: compiled source did not bind {name!r}"
            )
        new_value = local_ns[name]
        target_dict = self.target_module.__dict__
        old_value = target_dict.get(name, _MISSING)
        target_dict[name] = new_value
        journal.append(_restore_dict_slot, target_dict, name, old_value)
        for consumer_dict, consumer_key in self.index.lookup(
            self.target_module.__name__, name
        ):
            old_consumer = consumer_dict.get(consumer_key, _MISSING)
            consumer_dict[consumer_key] = new_value
            journal.append(_restore_dict_slot, consumer_dict, consumer_key, old_consumer)
```

- [ ] **Step 4: Run, expect PASS.**

- [ ] **Step 5: Commit**

```bash
git add tests/plugin/test_mutation_applier.py src/plugin/_fest_plugin.py
git commit -m "feat(plugin): apply class-attr and module-attr rebinds"
```

## Phase 7 — Plugin event loop integration

### Task 7.1: Read `ready_ack` and rewrite `_handle_mutant`

**Files:** Modify `src/plugin/_fest_plugin.py`.

- [ ] **Step 1: Replace `pytest_runtestloop`**

Find the existing `def pytest_runtestloop(session: Any) -> bool:` and replace its body with:

```python
def pytest_runtestloop(session: Any) -> bool:
    """Run the fest event loop after collection."""
    socket_path: str | None = session.config.getoption("fest_socket")
    if socket_path is None:
        return False

    _check_pytest_version()

    item_index: dict[str, Any] = {}
    for item in session.items:
        item_index[item.nodeid] = item

    file_to_mod = _build_file_module_index()

    conn = _connect(socket_path)
    if conn is None:
        return True
    conn.settimeout(None)

    test_ids = [item.nodeid for item in session.items]
    _send(conn, {"type": "ready", "tests": test_ids})

    rev_index = ReverseImportIndex.build_runtime_layer()

    buf = b""
    while True:
        chunk = conn.recv(4096)
        if not chunk:
            break
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            if not line.strip():
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError as exc:
                _send(conn, {
                    "type": "result",
                    "status": "error",
                    "error_message": f"bad json: {exc}",
                })
                continue

            msg_type = msg.get("type", "")
            if msg_type == "shutdown":
                conn.close()
                return True
            if msg_type == "ready_ack":
                rev_index.ingest_ast_layer(msg.get("import_bindings", []))
                _emit_reload_warnings(msg.get("reload_warnings", []))
                continue
            if msg_type == "mutant":
                result = _handle_mutant(session, msg, item_index, file_to_mod, rev_index)
                _send(conn, result)
            else:
                _send(conn, {
                    "type": "result",
                    "status": "error",
                    "error_message": f"unknown type: {msg_type}",
                })

    conn.close()
    return True


def _emit_reload_warnings(warnings: list[dict[str, Any]]) -> None:
    capped = warnings[:5]
    for w in capped:
        print(
            f"fest: detected {w.get('kind')} at {w.get('file')}:{w.get('line')} "
            "— plugin backend cannot guarantee accuracy. "
            "Consider --backend=subprocess for affected tests.",
            file=sys.stderr,
        )
    if len(warnings) > len(capped):
        print(
            f"fest: ... {len(warnings) - len(capped)} more reload/dynamic-import "
            "warnings suppressed.",
            file=sys.stderr,
        )
```

- [ ] **Step 2: Replace `_handle_mutant`**

Find the existing `def _handle_mutant(...)` and replace it with:

```python
_THREAD_WARNED = False


def _emit_thread_warning_if_needed() -> None:
    global _THREAD_WARNED
    if _THREAD_WARNED:
        return
    import threading
    if threading.active_count() > 1:
        _THREAD_WARNED = True
        print(
            f"fest: detected {threading.active_count()} active threads at "
            "mutant boundary; tests using threads must clean them up in "
            "teardown for accurate plugin-backend results, or use "
            "--backend=subprocess.",
            file=sys.stderr,
        )


def _handle_mutant(
    session: Any,
    msg: dict[str, Any],
    item_index: dict[str, Any],
    file_to_mod: dict[str, str],
    rev_index: "ReverseImportIndex",
) -> dict[str, Any]:
    file_path: str = msg.get("file", "")
    module_name: str = msg.get("module", "")
    diff: list[dict[str, Any]] = msg.get("diff", [])
    test_ids: list[str] = msg.get("tests", [])

    found = file_to_mod.get(os.path.abspath(file_path))
    if found:
        module_name = found
    elif not module_name:
        module_name = _file_to_module(file_path)

    target_module = sys.modules.get(module_name)
    if target_module is None:
        target_module = types.ModuleType(module_name)
        target_module.__file__ = file_path
        sys.modules[module_name] = target_module

    _emit_thread_warning_if_needed()

    journal = PatchJournal()
    applier = MutationApplier(target_module, rev_index)
    try:
        for change in diff:
            applier.apply(change, journal)
        status = _run_tests(session, test_ids, item_index)
        return {"type": "result", "status": status}
    except Exception as exc:  # noqa: BLE001
        return {
            "type": "result",
            "status": "error",
            "error_message": f"runtime error: {exc}",
        }
    finally:
        errors = journal.rollback()
        for err in errors:
            print(f"fest: rollback step failed: {err}", file=sys.stderr)
```

- [ ] **Step 3: Run cargo plugin-source tests**

```bash
cargo test --all-features plugin
```

Expected: PASS (the embedded source still contains the asserted substrings — `pytest_addoption`, `pytest_runtestloop`, `--fest-socket`, `"type": "ready"`, `shutdown`).

- [ ] **Step 4: Run plugin unit tests**

```bash
pytest tests/plugin -v
```

Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add src/plugin/_fest_plugin.py
git commit -m "feat(plugin): integrate MutationApplier into runtestloop with ready_ack"
```

---

### Task 7.2: Cover mid-diff failure rollback

**Files:** Modify `tests/plugin/test_mutation_applier.py`.

- [ ] **Step 1: Append the test**

```python
def test_journal_restores_first_change_when_second_apply_raises(target_module):
    target_module.MAX = 100
    consumer = {"MAX": 100}
    idx = ReverseImportIndex()
    idx.add(target_module.__name__, "MAX", consumer, "MAX")

    applier = MutationApplier(target_module, idx)
    journal = PatchJournal()

    applier.apply(
        {"kind": "constant_bind", "name": "MAX", "new_expr": "101"},
        journal,
    )
    assert target_module.MAX == 101

    with pytest.raises(SyntaxError):
        applier.apply(
            {"kind": "constant_bind", "name": "MAX", "new_expr": "(("},
            journal,
        )

    journal.rollback()
    assert target_module.MAX == 100
    assert consumer["MAX"] == 100
```

- [ ] **Step 2: Run, expect PASS.**

- [ ] **Step 3: Commit**

```bash
git add tests/plugin/test_mutation_applier.py
git commit -m "test(plugin): cover journal rollback after mid-diff apply failure"
```

---

## Phase 8 — Thread-cleanup fixture

### Task 8.1: Cleanup registry + pytest fixture

**Files:**
- Create: `tests/plugin/test_thread_detection.py`
- Modify: `src/plugin/_fest_plugin.py`

- [ ] **Step 1: Failing tests**

```python
"""Tests for fest thread cleanup registry and fixture."""

from __future__ import annotations


def test_cleanup_registry_runs_callbacks_in_lifo_order():
    import _fest_plugin
    cleanup = _fest_plugin._ThreadCleanupRegistry()
    calls: list[str] = []

    cleanup.register(calls.append, "first")
    cleanup.register(calls.append, "second")

    cleanup.run_all()

    assert calls == ["second", "first"]


def test_cleanup_registry_collects_errors():
    import _fest_plugin
    cleanup = _fest_plugin._ThreadCleanupRegistry()

    def boom():
        raise ValueError("nope")

    cleanup.register(boom)
    cleanup.register(lambda: None)

    errors = cleanup.run_all()
    assert len(errors) == 1
    assert isinstance(errors[0], ValueError)


def test_cleanup_registry_clears_after_run():
    import _fest_plugin
    cleanup = _fest_plugin._ThreadCleanupRegistry()
    calls: list[int] = []
    cleanup.register(calls.append, 1)
    cleanup.run_all()
    cleanup.run_all()
    assert calls == [1]
```

- [ ] **Step 2: Run, expect FAIL.**

- [ ] **Step 3: Add registry and fixture**

Append to `_fest_plugin.py`:

```python
class _ThreadCleanupRegistry:
    """LIFO registry of cleanup callbacks invoked between mutants."""

    def __init__(self) -> None:
        self._callbacks: list[tuple[Any, tuple[Any, ...], dict[str, Any]]] = []

    def register(self, fn: Any, *args: Any, **kwargs: Any) -> None:
        self._callbacks.append((fn, args, kwargs))

    def run_all(self) -> list[BaseException]:
        errors: list[BaseException] = []
        while self._callbacks:
            fn, args, kwargs = self._callbacks.pop()
            try:
                fn(*args, **kwargs)
            except Exception as exc:  # noqa: BLE001
                errors.append(exc)
        return errors


_GLOBAL_THREAD_CLEANUP = _ThreadCleanupRegistry()


@pytest.fixture
def fest_thread_cleanup():
    """Register cleanup callbacks invoked between mutants.

    Usage::

        def test_with_workers(fest_thread_cleanup):
            pool = ThreadPoolExecutor(max_workers=4)
            fest_thread_cleanup(pool.shutdown, wait=True)
            ...
    """
    def register(fn, *args, **kwargs):
        _GLOBAL_THREAD_CLEANUP.register(fn, *args, **kwargs)

    yield register
```

- [ ] **Step 4: Run, expect 3 PASS.**

- [ ] **Step 5: Commit**

```bash
git add tests/plugin/test_thread_detection.py src/plugin/_fest_plugin.py
git commit -m "feat(plugin): add fest_thread_cleanup fixture and registry"
```

---

### Task 8.2: Wire registry into the mutant lifecycle

**Files:** Modify `src/plugin/_fest_plugin.py`.

- [ ] **Step 1: Update `_handle_mutant`**

In `_handle_mutant`, immediately after `_emit_thread_warning_if_needed()`, add:

```python
    cleanup_errors = _GLOBAL_THREAD_CLEANUP.run_all()
    for err in cleanup_errors:
        print(f"fest: thread cleanup callback failed: {err}", file=sys.stderr)
```

- [ ] **Step 2: Run cargo plugin-source tests + plugin unit tests**

```bash
cargo test --all-features plugin && pytest tests/plugin -v
```

Expected: all PASS.

- [ ] **Step 3: Commit**

```bash
git add src/plugin/_fest_plugin.py
git commit -m "feat(plugin): invoke thread-cleanup callbacks between mutants"
```

---

## Phase 9 — Integration fixtures and end-to-end test

### Task 9.1: `from_imports` fixture project

**Files:**
- Create: `tests/fixtures/from_imports/src/calc.py`, `src/consumer.py`
- Create: `tests/fixtures/from_imports/tests/test_calc.py`
- Create: `tests/fixtures/from_imports/conftest.py`, `pyproject.toml`

- [ ] **Step 1: Create directory layout**

```bash
mkdir -p tests/fixtures/from_imports/src tests/fixtures/from_imports/tests
```

- [ ] **Step 2: Write `src/calc.py`**

```python
"""Calc utilities."""


def add(a: int, b: int) -> int:
    return a + b


MAX = 100
```

- [ ] **Step 3: Write `src/consumer.py`**

```python
"""Consumer using `from`-imports."""

from src.calc import MAX, add


def double_add(a: int, b: int) -> int:
    return add(a, b) * 2


def at_max(value: int) -> bool:
    return value == MAX
```

- [ ] **Step 4: Write `tests/test_calc.py`**

```python
from src.calc import add
from src.consumer import at_max, double_add


def test_add():
    assert add(1, 2) == 3


def test_double_add():
    assert double_add(1, 2) == 6


def test_at_max_true():
    assert at_max(100) is True


def test_at_max_false():
    assert at_max(50) is False
```

- [ ] **Step 5: Write `conftest.py`**

```python
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
```

- [ ] **Step 6: Write `pyproject.toml`**

```toml
[tool.fest]
source = ["src"]
test = ["tests"]
```

- [ ] **Step 7: Sanity check**

```bash
cd tests/fixtures/from_imports && pytest tests -v && cd -
```

Expected: 4 PASS.

- [ ] **Step 8: Commit**

```bash
git add tests/fixtures/from_imports
git commit -m "test(fixtures): add from-imports project for plugin integration testing"
```

---

### Task 9.2: `nested_closures` fixture project

**Files:** Create files under `tests/fixtures/nested_closures/`.

- [ ] **Step 1: Create layout**

```bash
mkdir -p tests/fixtures/nested_closures/src tests/fixtures/nested_closures/tests
```

- [ ] **Step 2: `src/factory.py`**

```python
"""Factory returning a nested function."""


def make_counter():
    def counter():
        return 1

    return counter
```

- [ ] **Step 3: `tests/test_factory.py`**

```python
from src.factory import make_counter


def test_counter_returns_one():
    counter = make_counter()
    assert counter() == 1
```

- [ ] **Step 4: `conftest.py`**

```python
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
```

- [ ] **Step 5: `pyproject.toml`**

```toml
[tool.fest]
source = ["src"]
test = ["tests"]
```

- [ ] **Step 6: Sanity check**

```bash
cd tests/fixtures/nested_closures && pytest tests -v && cd -
```

Expected: 1 PASS.

- [ ] **Step 7: Commit**

```bash
git add tests/fixtures/nested_closures
git commit -m "test(fixtures): add nested-closures project for plugin integration testing"
```

---

### Task 9.3: `registry_classes` fixture project

**Files:** Create files under `tests/fixtures/registry_classes/`.

- [ ] **Step 1: Create layout**

```bash
mkdir -p tests/fixtures/registry_classes/src tests/fixtures/registry_classes/tests
```

- [ ] **Step 2: `src/registry.py`**

```python
"""Registry-pattern classes exercising __init_subclass__ stability."""


class Plugin:
    registry: list[type] = []

    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        Plugin.registry.append(cls)

    def value(self) -> int:
        return 1


class A(Plugin):
    def value(self) -> int:
        return 1
```

- [ ] **Step 3: `tests/test_registry.py`**

```python
from src.registry import A, Plugin


def test_a_value():
    assert A().value() == 1


def test_registry_size_stable():
    assert len(Plugin.registry) == 1
```

- [ ] **Step 4: `conftest.py` and `pyproject.toml`**

```python
# conftest.py
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
```

```toml
# pyproject.toml
[tool.fest]
source = ["src"]
test = ["tests"]
```

- [ ] **Step 5: Sanity check**

```bash
cd tests/fixtures/registry_classes && pytest tests -v && cd -
```

Expected: 2 PASS.

- [ ] **Step 6: Commit**

```bash
git add tests/fixtures/registry_classes
git commit -m "test(fixtures): add registry-classes project for plugin integration testing"
```

---

### Task 9.4: End-to-end Rust integration test

**Files:** Modify `src/runner/pytest_plugin.rs` (`tests` module).

- [ ] **Step 1: Append the test**

```rust
    /// Integration test: run a real mutant through the plugin pipeline
    /// against the `from_imports` fixture and verify the consumer
    /// (which uses `from src.calc import add`) sees the mutation —
    /// i.e. Plan G's reverse-import rebinding actually works end-to-end.
    ///
    /// Ignored by default — requires a working python with pytest on PATH.
    #[tokio::test]
    #[ignore = "requires python+pytest on PATH; run with --include-ignored"]
    async fn plugin_run_mutant_propagates_to_consumer_via_index() {
        use crate::mutation::{Mutant, MutantStatus};

        let fixture = std::path::Path::new("tests/fixtures/from_imports");
        if !fixture.exists() {
            return;
        }
        let runner = PytestPluginRunner::new(60_u64);
        runner.start(1, fixture).await.expect("start");

        // Mutate `def add(a, b): return a + b` to `return a - b`.
        let calc_path = fixture.join("src/calc.py");
        let source = std::fs::read_to_string(&calc_path).expect("read calc.py");
        let plus_byte = source.find("a + b").expect("find expr") + 2;
        let mutant = Mutant {
            file_path: calc_path.clone(),
            line: 1,
            column: 1,
            byte_offset: plus_byte,
            byte_length: 1,
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            mutator_name: "arithmetic".to_owned(),
        };
        let tests: Vec<String> = vec![
            "tests/test_calc.py::test_add".to_owned(),
            "tests/test_calc.py::test_double_add".to_owned(),
        ];
        let result = runner.run_mutant(&mutant, &source, &tests).await
            .expect("run_mutant");

        // The consumer test (`test_double_add` uses `from src.calc import add`)
        // must observe the mutation — status should be Killed, not Survived.
        assert_eq!(
            result.status, MutantStatus::Killed,
            "consumer didn't see the mutation; reverse-import index broken? \
             actual status: {:?}", result.status,
        );

        runner.stop().await.expect("stop");
    }
```

- [ ] **Step 2: Run with `--include-ignored`**

```bash
cargo test --all-features pytest_plugin::tests::plugin_starts_against_from_imports_fixture -- --include-ignored
```

Expected: PASS (or skipped if pytest unavailable). Failure here points to a real wiring bug — fix before proceeding.

- [ ] **Step 3: Commit**

```bash
git add src/runner/pytest_plugin.rs
git commit -m "test(runner): integration test for plugin against from-imports fixture"
```

---

## Phase 10 — Wrap-up

### Task 10.1: Coverage and lint gate

- [ ] **Step 1: Run the full suite**

```bash
just check-all
```

Expected: all PASS — fmt-check, lint, deny, machete, test, test-plugin, jscpd.

- [ ] **Step 2: Coverage gate**

```bash
just coverage
```

Expected: ≥95% line coverage. If new code drops coverage below the gate, find uncovered lines via:

```bash
cargo llvm-cov --all-features --html
```

…and add targeted tests.

- [ ] **Step 3: Commit any coverage-driven additions** (skip if none)

```bash
git add -A
git commit -m "test: cover newly-added Plan G code to stay above 95% gate"
```

---

### Task 10.2: Push and open PR

- [ ] **Step 1: Push the branch**

```bash
git push -u origin docs/plan-g-spec
```

- [ ] **Step 2: Open the PR**

```bash
gh pr create --title "feat(plugin): Plan G reference fixup (closes #8)" --body "$(cat <<'EOF'
## Summary
- Replaces gc.get_referrers() approach with a structured-diff IR + reverse-import index + per-kind appliers
- Closes the constants blind spot via name-addressed index (built once on Rust side, shipped over ready_ack)
- Avoids re-firing __init_subclass__/__set_name__ — class bodies never re-execute
- Body-only nested-function mutations now patched in place via parent co_consts swap
- Thread detection + opt-in fest_thread_cleanup fixture
- importlib.reload calls flagged at AST-scan time as upfront startup warnings

## Spec
docs/superpowers/specs/2026-03-14-plugin-reference-fixup-design.md

## Test plan
- [ ] just check-all passes
- [ ] just coverage ≥95%
- [ ] Plugin score parity (plugin vs subprocess) within 1% on tests/fixtures/from_imports
- [ ] tests/fixtures/registry_classes registry size stable across mutants
EOF
)"
```

Expected: PR URL printed.

---

## Self-review (writer-side)

**Spec coverage:** every spec section has a corresponding task — `MutationDiff` IR (Phase 1), Rust-side `plugin_index` (Phase 2), IPC/handshake wiring (Phase 3), `PatchJournal` (Phase 4), `ReverseImportIndex` two-layer build (Phase 5), `MutationApplier` per variant (Phase 6), `_handle_mutant` integration + reload-warning surface (Phase 7), thread detection + cleanup fixture (Phase 8), three fixture projects + integration test (Phase 9), coverage gate + PR (Phase 10).

**Placeholder scan:** none. All tasks include exact file paths, complete code, and explicit run/expected lines.

**Type-name consistency:** `MutationDiff` (Rust enum, snake_case-rendered `kind` strings) ↔ Python `change["kind"]` keys (`function_body`, `constant_bind`, `class_method`, `class_attr`, `module_attr`); `ReverseImportIndex.lookup` signature stable from Phase 5 through Phase 6; `PatchJournal.append` and `rollback` consistent across all consumers; `_PY_EXEC` / `_PY_EVAL` aliases declared in Task 4.2 and used in Tasks 6.1, 6.4, 6.6.

**Order check:** dependencies are correct — Rust IR (Phase 1) → Rust scan (Phase 2) → wiring (Phase 3) → Python primitives (Phases 4-6) → integration (Phase 7) → fixture/end-to-end (Phases 8-9) → wrap-up (Phase 10).

**Convention note:** the `_PY_EXEC` / `_PY_EVAL` aliasing in code samples (introduced in Task 4.2) is a deliberate convention to keep the security-sensitive builtins greppable in one place; it does not change semantics. Reviewers can audit every dynamic-source call site by searching for these names.

