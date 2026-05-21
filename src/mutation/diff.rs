//! Structured diff IR for mutations dispatched to the plugin backend.

use serde::{Deserialize, Serialize};

/// Scope in which a [`MutationDiff::StatementBind`] should be exec'd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BindScope {
    /// Bind into the target module's `__dict__`.
    Module,
    /// Bind into a class's namespace. `qualname` is dotted (e.g. `Outer.Inner`).
    Class {
        /// Dotted qualified name of the target class.
        qualname: String,
    },
}

/// One unit of structural change derived from a [`crate::mutation::Mutant`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MutationDiff {
    /// Function body changed. `qualname` may be dotted for nested functions.
    /// `new_source` is the raw `def` block with decorators stripped.
    FunctionBody {
        /// Dotted qualified name of the function (e.g. `outer.inner`).
        qualname: String,
        /// Full `def` block source with decorators stripped.
        new_source: String,
    },

    /// Module- or class-scope statement-level binding change.
    /// Subsumes `Stmt::Assign`, `Stmt::AnnAssign`, and `Stmt::AugAssign`
    /// (added incrementally in Tasks 3-7).
    StatementBind {
        /// Names assigned by this statement. May have one entry (bare assign)
        /// or many (tuple unpack, chained `a = b = ...`).
        names: Vec<String>,
        /// Full mutated statement source; exec'd against the target namespace.
        stmt_source: String,
        /// Target namespace.
        scope: BindScope,
    },

    /// Class method body changed. `method_name` uses dotted suffix
    /// (`x.fget` / `.fset` / `.fdel`) for property accessors.
    ClassMethod {
        /// Dotted qualified name of the class (e.g. `Outer.Inner`).
        class_qualname: String,
        /// Name of the method, including property accessor suffix where applicable.
        method_name: String,
        /// Full `def` block source for the mutated method.
        new_source: String,
    },

    /// Module-level binding requiring statement-mode compilation
    /// (decorator removal, class re-definition).
    ModuleAttr {
        /// Name of the module-level binding.
        name: String,
        /// Full source of the new statement (e.g. decorated def or class body).
        new_source: String,
    },
}

// ---------------------------------------------------------------------------
// Derivation
// ---------------------------------------------------------------------------

use ruff_python_ast::{ModModule, Stmt};
use ruff_text_size::Ranged;

use crate::mutation::{Mutant, SkipReason};

/// Derive the structured diff(s) for a mutant, or return a [`SkipReason`]
/// when the mutation cannot be represented by the plugin IR.
///
/// Returns:
/// - `Ok(vec)` with at least one `MutationDiff` on success
/// - `Err(SkipReason::ConditionMutation)` for mutations on `if`/`while`/`for` test expressions
/// - `Err(SkipReason::UnmappableTarget)` for assigns with non-`Name` targets
/// - `Err(SkipReason::UnsupportedStatement)` for any other unhandled case
///
/// # Errors
///
/// Returns `Err(SkipReason::ConditionMutation)` when the mutation byte offset
/// falls inside an `if`/`while` test expression or `for` iterable.
/// Returns `Err(SkipReason::UnmappableTarget)` when the statement is an
/// assignment but the target contains a subscript, attribute, or starred form.
/// Returns `Err(SkipReason::UnsupportedStatement)` for all other unhandled
/// statement kinds (e.g. `del`, `import`, annotation-only `AnnAssign`).
#[inline]
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt references in a for loop; adding & to patterns is more verbose"
)]
pub fn derive_diff(
    mutant: &Mutant,
    original_ast: &ModModule,
    _mutated_ast: &ModModule,
    mutated_source: &str,
) -> Result<Vec<MutationDiff>, SkipReason> {
    if mutant.mutator_name == "remove_decorator"
        && let Some(d) = derive_decorator_removal(mutant, original_ast, mutated_source)
    {
        return Ok(vec![d]);
    }
    let off = mutant.byte_offset;
    for stmt in &original_ast.body {
        let r = stmt.range();
        if !(r.start().to_usize() <= off && off < r.end().to_usize()) {
            continue;
        }
        if let Some(d) = derive_for_top_level(stmt, mutated_source, mutant) {
            return Ok(vec![d]);
        }
        // Control-flow fallback.
        if matches!(
            stmt,
            Stmt::If(_) | Stmt::While(_) | Stmt::For(_) | Stmt::Try(_)
        ) {
            if is_in_condition(stmt, off) {
                return Err(SkipReason::ConditionMutation);
            }
            if let Some(d) = walk_control_flow(stmt, mutated_source, mutant) {
                return Ok(vec![d]);
            }
        }
        // Unmappable assign-target classification: if the statement is an
        // assign-shaped one and we got here, the targets couldn't be
        // extracted (subscript, attribute, starred, etc.).
        // Exception: annotation-only AnnAssign (no value) is unsupported,
        // not unmappable — there is simply nothing to bind.
        if let Stmt::AnnAssign(ann) = stmt {
            if ann.value.is_none() {
                return Err(SkipReason::UnsupportedStatement);
            }
            return Err(SkipReason::UnmappableTarget);
        }
        if matches!(stmt, Stmt::Assign(_) | Stmt::AugAssign(_)) {
            return Err(SkipReason::UnmappableTarget);
        }
        return Err(SkipReason::UnsupportedStatement);
    }
    Err(SkipReason::UnsupportedStatement)
}

/// Returns true when `off` falls inside the test expression of a control-flow
/// statement (the part that runs each iteration / once). For `Stmt::For`,
/// this is the `iter` expression — the iterable evaluated once before the loop.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt reference; explicit & in arms is more verbose without benefit"
)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "all non-condition-bearing statement variants intentionally fall through to false"
)]
fn is_in_condition(stmt: &Stmt, off: usize) -> bool {
    let test_range = match stmt {
        Stmt::If(s) => s.test.range(),
        Stmt::While(s) => s.test.range(),
        Stmt::For(s) => s.iter.range(),
        _ => return false,
    };
    test_range.start().to_usize() <= off && off < test_range.end().to_usize()
}

/// Derive a [`MutationDiff::ModuleAttr`] for a decorator-removal mutant.
///
/// Searches the original AST for a top-level `FunctionDef` or `ClassDef` whose
/// byte range contains the mutant's byte offset (decorator lines are included in
/// the node's range), then slices the mutated source to produce the post-removal
/// statement body and wraps it in [`MutationDiff::ModuleAttr`].
///
/// Returns `None` if no matching top-level statement is found.
#[allow(
    clippy::string_slice,
    reason = "byte offsets originate from the AST and are always valid UTF-8 boundaries"
)]
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt references in a for loop; adding & to patterns is more verbose"
)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "only FunctionDef and ClassDef carry a name; all other statement variants are skipped"
)]
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
        let stmt_end_in_mutated = mutated_stmt_end(end, mutant);
        let new_source =
            strip_decorators(mutated_source.get(start..stmt_end_in_mutated).unwrap_or(""));
        return Some(MutationDiff::ModuleAttr { name, new_source });
    }
    None
}

/// Recursively descend into a function definition to find the innermost function
/// whose body contains the mutant's byte offset, returning a [`MutationDiff::FunctionBody`]
/// with a dotted `qualname` (e.g. `outer.inner`).
///
/// `qualname_prefix` is the dotted name accumulated so far from outer scopes.
/// Pass `""` at the top level — the prefix is prepended automatically.
#[allow(
    clippy::string_slice,
    reason = "byte offsets originate from the AST and are always valid UTF-8 boundaries"
)]
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt references in a for loop; adding & to patterns is more verbose"
)]
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
            return descend_function(nested, mutated_source, mutant, &outer_qualname);
        }
    }
    let range = func.range();
    let stmt_start = range.start().to_usize();
    let stmt_end = mutated_stmt_end(range.end().to_usize(), mutant);
    let new_source = strip_decorators(mutated_source.get(stmt_start..stmt_end).unwrap_or(""));
    Some(MutationDiff::FunctionBody {
        qualname: outer_qualname,
        new_source,
    })
}

/// Derive a [`MutationDiff::StatementBind`] (class scope) from an assign-shaped
/// statement inside a class body. Returns `None` when the statement is
/// annotation-only (no value) or has an unmappable target.
#[allow(
    clippy::string_slice,
    reason = "byte offsets originate from the AST and are always valid UTF-8 boundaries"
)]
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt reference; adding & to patterns is more verbose"
)]
fn derive_class_stmt_bind(
    body_stmt: &Stmt,
    r: ruff_text_size::TextRange,
    mutant: &Mutant,
    mutated_source: &str,
    class_qualname: String,
) -> Option<MutationDiff> {
    if let Stmt::AnnAssign(ann) = body_stmt
        && ann.value.is_none()
    {
        return None;
    }
    let names = extract_assign_names(body_stmt);
    if names.is_empty() {
        return None;
    }
    let stmt_end = mutated_stmt_end(r.end().to_usize(), mutant);
    let stmt_source = mutated_source
        .get(r.start().to_usize()..stmt_end)
        .unwrap_or("")
        .to_owned();
    Some(MutationDiff::StatementBind {
        names,
        stmt_source,
        scope: BindScope::Class {
            qualname: class_qualname,
        },
    })
}

/// Derive a [`MutationDiff`] for a class body when the mutation offset falls
/// inside it. Builds a dotted `class_qualname` from `qualname_prefix` for
/// nested classes — pass `""` at the top level.
#[allow(
    clippy::string_slice,
    reason = "byte offsets originate from the AST and are always valid UTF-8 boundaries"
)]
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt references in a for loop; adding & to patterns is more verbose"
)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "only the handled variants are relevant; all others are not class body mutations"
)]
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
        if !(r.start().to_usize() <= mutant.byte_offset && mutant.byte_offset < r.end().to_usize())
        {
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
                let stmt_end = mutated_stmt_end(r.end().to_usize(), mutant);
                let new_source = strip_decorators(
                    mutated_source
                        .get(r.start().to_usize()..stmt_end)
                        .unwrap_or(""),
                );
                return Some(MutationDiff::ClassMethod {
                    class_qualname,
                    method_name,
                    new_source,
                });
            }
            Stmt::Assign(_) | Stmt::AnnAssign(_) | Stmt::AugAssign(_) => {
                return derive_class_stmt_bind(
                    body_stmt,
                    r,
                    mutant,
                    mutated_source,
                    class_qualname,
                );
            }
            Stmt::ClassDef(nested) => {
                return derive_for_class_body(nested, mutant, mutated_source, &class_qualname);
            }
            _ => return None,
        }
    }
    None
}

/// Collect bare names from a Python assignment target. Returns `false` if
/// the target contains any non-bindable form (subscript, attribute, starred).
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr references; adding & to each arm is more verbose"
)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "subscript, attribute, starred and all other non-bindable variants fall through to \
              false"
)]
fn collect_names_from_target(expr: &ruff_python_ast::Expr, out: &mut Vec<String>) -> bool {
    use ruff_python_ast::Expr;
    match expr {
        Expr::Name(n) => {
            out.push(n.id.to_string());
            true
        }
        Expr::Tuple(t) => t.elts.iter().all(|e| collect_names_from_target(e, out)),
        Expr::List(l) => l.elts.iter().all(|e| collect_names_from_target(e, out)),
        // Subscript, Attribute, Starred, anything else: not bindable.
        _ => false,
    }
}

/// Extract bound names from an assign-shaped statement.
///
/// Returns an empty `Vec` when any target is non-`Name` (subscript, attribute,
/// tuple containing a non-Name, starred unpack). Callers treat empty as a
/// signal to fall through derivation (the mutation is unmappable).
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt and &Expr references; adding & to each arm is more verbose"
)]
fn extract_assign_names(stmt: &Stmt) -> Vec<String> {
    use ruff_python_ast::Expr;
    let mut out = Vec::new();
    if let Stmt::Assign(assign) = stmt {
        // Walks every target; multi-target chains (a = b = ...) and
        // tuple/list unpacks (a, b = ...) both decompose to a list of
        // bindable names. Any non-bindable target bails out entirely.
        for target in &assign.targets {
            if !collect_names_from_target(target, &mut out) {
                return Vec::new();
            }
        }
        return out;
    }
    if let Stmt::AnnAssign(ann) = stmt
        && ann.value.is_some()
        && let Expr::Name(n) = &*ann.target
    {
        // Only count as bound if a value is actually being assigned.
        // Annotation-only (`X: int`) has value == None and is non-mutable.
        out.push(n.id.to_string());
    }
    if let Stmt::AugAssign(aug) = stmt
        && let Expr::Name(n) = &*aug.target
    {
        out.push(n.id.to_string());
    }
    out
}

/// Collect the body slices of a control-flow statement that should be
/// recursed into when searching for a binding mutation.
///
/// Returns `None` for statement kinds that are not control-flow containers.
/// Intentionally does **not** include the `if`/`while`/`for` test expression —
/// mutations on those are left unmapped (Task 16 classifies them as
/// `ConditionMutation`).
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt requires pattern_type_mismatch suppression"
)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "all non-control-flow statement variants intentionally return None"
)]
fn control_flow_bodies(stmt: &Stmt) -> Option<Vec<&Vec<Stmt>>> {
    match stmt {
        Stmt::If(s) => {
            let mut v: Vec<&Vec<Stmt>> = vec![&s.body];
            for clause in &s.elif_else_clauses {
                v.push(&clause.body);
            }
            Some(v)
        }
        Stmt::While(s) => Some(vec![&s.body, &s.orelse]),
        Stmt::For(s) => Some(vec![&s.body, &s.orelse]),
        Stmt::Try(s) => {
            let mut v: Vec<&Vec<Stmt>> = vec![&s.body, &s.orelse, &s.finalbody];
            for h in &s.handlers {
                #[allow(
                    clippy::pattern_type_mismatch,
                    reason = "matching on ExceptHandler requires suppression"
                )]
                let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = h;
                v.push(&handler.body);
            }
            Some(v)
        }
        _ => None,
    }
}

/// Recurse into the body branches of a control-flow statement. If the mutant
/// offset is inside any branch and that branch's inner statement is matched
/// by [`derive_for_top_level`], return the corresponding diff. Returns `None`
/// for mutations on the `if`/`while`/`for` test expression (Task 16
/// classifies those as `ConditionMutation`).
fn walk_control_flow(stmt: &Stmt, mutated_source: &str, mutant: &Mutant) -> Option<MutationDiff> {
    let bodies = control_flow_bodies(stmt)?;
    for body in bodies {
        for inner in body {
            let r = inner.range();
            if !(r.start().to_usize() <= mutant.byte_offset
                && mutant.byte_offset < r.end().to_usize())
            {
                continue;
            }
            if let Some(d) = derive_for_top_level(inner, mutated_source, mutant) {
                return Some(d);
            }
            if control_flow_bodies(inner).is_some()
                && let Some(d) = walk_control_flow(inner, mutated_source, mutant)
            {
                return Some(d);
            }
        }
    }
    None
}

/// Attempt to derive a [`MutationDiff`] for a single top-level statement.
///
/// Returns `None` when the statement kind is not yet supported.
#[allow(
    clippy::string_slice,
    reason = "byte offsets originate from the AST and are always valid UTF-8 boundaries"
)]
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt references; adding & to each arm is more verbose"
)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "only the listed statement kinds are supported; all others return None"
)]
#[allow(
    clippy::question_mark,
    reason = "ann.value.as_ref()? would trigger unused_results since Box<Expr> is \
              must_use-adjacent"
)]
fn derive_for_top_level(
    stmt: &Stmt,
    mutated_source: &str,
    mutant: &Mutant,
) -> Option<MutationDiff> {
    match stmt {
        Stmt::FunctionDef(func) => descend_function(func, mutated_source, mutant, ""),
        Stmt::Assign(_) | Stmt::AugAssign(_) => {
            let names = extract_assign_names(stmt);
            if names.is_empty() {
                return None;
            }
            let stmt_end = mutated_stmt_end(stmt.range().end().to_usize(), mutant);
            let stmt_start = stmt.range().start().to_usize();
            let stmt_source = mutated_source
                .get(stmt_start..stmt_end)
                .unwrap_or("")
                .to_owned();
            Some(MutationDiff::StatementBind {
                names,
                stmt_source,
                scope: BindScope::Module,
            })
        }
        Stmt::AnnAssign(ann) => {
            if ann.value.is_none() {
                return None;
            }
            let names = extract_assign_names(stmt);
            if names.is_empty() {
                return None;
            }
            let stmt_end = mutated_stmt_end(stmt.range().end().to_usize(), mutant);
            let stmt_start = stmt.range().start().to_usize();
            let stmt_source = mutated_source
                .get(stmt_start..stmt_end)
                .unwrap_or("")
                .to_owned();
            Some(MutationDiff::StatementBind {
                names,
                stmt_source,
                scope: BindScope::Module,
            })
        }
        Stmt::ClassDef(class) => derive_for_class_body(class, mutant, mutated_source, ""),
        _ => None,
    }
}

/// Compute the new end byte offset of a statement after a mutation is applied.
///
/// Uses signed arithmetic to avoid usize underflow when the mutated text is shorter
/// than the original (e.g. decorator removal replaces `@cache\n` with `""`).
fn mutated_stmt_end(original_end: usize, mutant: &Mutant) -> usize {
    let delta = mutant.mutated_text.len().cast_signed() - mutant.byte_length.cast_signed();
    original_end
        .checked_add_signed(delta)
        .unwrap_or(original_end)
}

/// Return the property accessor suffix for a method decorated with `@property`,
/// `@x.setter`, or `@x.deleter`.
///
/// Returns `"fget"` for `@property`, `"fset"` for `@x.setter`, `"fdel"` for
/// `@x.deleter`, and `""` for plain (non-property) methods.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr references inside decorator_list; adding & to each arm is more \
              verbose"
)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "only Name and Attribute variants are relevant for property detection; all others \
              continue"
)]
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
            _ => {}
        }
    }
    ""
}

/// Strip leading decorator lines (`@…`) from a function/class source block.
///
/// Lines are removed from the front while the first non-empty, non-whitespace
/// character is `@`.  The remaining lines are joined with `"\n"`.
fn strip_decorators(source: &str) -> String {
    let mut lines: Vec<&str> = source.lines().collect();
    while lines
        .first()
        .is_some_and(|line| line.trim_start().starts_with('@'))
    {
        let _removed: &str = lines.remove(0);
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use ruff_python_ast::ModModule;
    use ruff_python_parser::parse_module;

    use super::*;
    use crate::mutation::{Mutant, SkipReason};

    fn parse(src: &str) -> ModModule {
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
    fn subscript_target_yields_unmappable_target() {
        let src = "d = {}\nd['k'] = 1\n";
        let mutated = "d = {}\nd['k'] = 2\n";
        let module = parse(src);
        let off = src.find("'k'] = 1").unwrap() + "'k'] = ".len();
        let mutant = Mutant {
            file_path: "m.py".into(),
            line: 2,
            column: 9,
            byte_offset: off,
            byte_length: 1,
            original_text: "1".into(),
            mutated_text: "2".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(
            diff,
            Err(SkipReason::UnmappableTarget),
            "subscript target d['k'] = ... is unmappable, expected UnmappableTarget, got {diff:?}"
        );
    }

    #[test]
    fn attribute_target_yields_unmappable_target() {
        let src = "class C: pass\nobj = C()\nobj.x = 1\n";
        let mutated = "class C: pass\nobj = C()\nobj.x = 2\n";
        let module = parse(src);
        let off = src.find("obj.x = 1").unwrap() + "obj.x = ".len();
        let mutant = Mutant {
            file_path: "m.py".into(),
            line: 3,
            column: 9,
            byte_offset: off,
            byte_length: 1,
            original_text: "1".into(),
            mutated_text: "2".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(
            diff,
            Err(SkipReason::UnmappableTarget),
            "attribute target obj.x = ... is unmappable, expected UnmappableTarget, got {diff:?}"
        );
    }

    #[test]
    fn top_level_function_body_yields_function_body_variant() {
        let original_src = "def add(a, b):\n    return a + b\n";
        let mutated_src = "def add(a, b):\n    return a - b\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let plus_offset = original_src.find('+').unwrap();
        let mutant = make_mutant("calc.py", "+", "-", plus_offset);

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src)
            .expect("derive_diff should succeed");

        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::FunctionBody {
                qualname,
                new_source,
            } => {
                assert_eq!(qualname, "add");
                assert!(new_source.starts_with("def add"));
                assert!(new_source.contains("return a - b"));
            }
            other => panic!("expected FunctionBody, got {other:?}"),
        }
    }

    #[test]
    fn module_level_constant_yields_statement_bind_variant() {
        let original_src = "MAX = 100\n";
        let mutated_src = "MAX = 101\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let mutant = make_mutant("config.py", "100", "101", 6);

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src)
            .expect("derive_diff should succeed");

        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::StatementBind {
                names,
                stmt_source,
                scope,
            } => {
                assert_eq!(names, &["MAX"]);
                assert!(stmt_source.contains("101"), "stmt_source={stmt_source:?}");
                assert_eq!(*scope, BindScope::Module);
            }
            other => panic!("expected StatementBind, got {other:?}"),
        }
    }

    #[test]
    fn statement_bind_handles_shrinking_mutation() {
        // "100" -> "1" — shrinks by 2 bytes.
        let original_src = "MAX = 100\n";
        let mutated_src = "MAX = 1\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let mutant = Mutant {
            file_path: "config.py".into(),
            line: 1,
            column: 1,
            byte_offset: 6,
            byte_length: 3,
            original_text: "100".to_owned(),
            mutated_text: "1".to_owned(),
            mutator_name: "test".to_owned(),
        };
        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src)
            .expect("derive_diff should succeed");
        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::StatementBind {
                names,
                stmt_source,
                scope,
            } => {
                assert_eq!(names, &["MAX"]);
                assert!(stmt_source.contains("1"), "stmt_source={stmt_source:?}");
                assert_eq!(*scope, BindScope::Module);
            }
            other => panic!("expected StatementBind, got {other:?}"),
        }
    }

    #[test]
    fn class_method_body_yields_class_method_variant() {
        let original_src = "class Calc:\n    def add(self, a, b):\n        return a + b\n";
        let mutated_src = "class Calc:\n    def add(self, a, b):\n        return a - b\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let plus_offset = original_src.find('+').unwrap();
        let mutant = make_mutant("calc.py", "+", "-", plus_offset);

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src)
            .expect("derive_diff should succeed");

        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::ClassMethod {
                class_qualname,
                method_name,
                new_source,
            } => {
                assert_eq!(class_qualname, "Calc");
                assert_eq!(method_name, "add");
                assert!(new_source.contains("return a - b"));
            }
            other => panic!("expected ClassMethod, got {other:?}"),
        }
    }

    #[test]
    fn class_attr_yields_statement_bind_class_variant() {
        let original_src = "class Cfg:\n    LIMIT = 10\n";
        let mutated_src = "class Cfg:\n    LIMIT = 11\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let offset = original_src.find("10").unwrap();
        let mutant = make_mutant("cfg.py", "10", "11", offset);

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src)
            .expect("derive_diff should succeed");

        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::StatementBind {
                names,
                stmt_source,
                scope,
            } => {
                assert_eq!(names, &["LIMIT"]);
                assert!(stmt_source.contains("11"), "stmt_source={stmt_source:?}");
                assert_eq!(
                    *scope,
                    BindScope::Class {
                        qualname: "Cfg".to_owned()
                    }
                );
            }
            other => panic!("expected StatementBind (class scope), got {other:?}"),
        }
    }

    #[test]
    fn class_scope_assign_yields_statement_bind_class() {
        let src = "class C:\n    X = 5\n";
        let mutated = "class C:\n    X = 6\n";
        let module = parse(src);
        let off = src.find("X = 5").unwrap() + "X = ".len();
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 2,
            column: 9,
            byte_offset: off,
            byte_length: 1,
            original_text: "5".into(),
            mutated_text: "6".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(
            diff,
            Ok(vec![MutationDiff::StatementBind {
                names: vec!["X".to_owned()],
                stmt_source: "X = 6".to_owned(),
                scope: BindScope::Class {
                    qualname: "C".to_owned()
                },
            }])
        );
    }

    #[test]
    fn class_scope_annotated_assign_yields_statement_bind_class() {
        let src = "class C:\n    X: int = 5\n";
        let mutated = "class C:\n    X: int = 6\n";
        let module = parse(src);
        let off = src.find("X: int = 5").unwrap() + "X: int = ".len();
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 2,
            column: 14,
            byte_offset: off,
            byte_length: 1,
            original_text: "5".into(),
            mutated_text: "6".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(
            diff,
            Ok(vec![MutationDiff::StatementBind {
                names: vec!["X".to_owned()],
                stmt_source: "X: int = 6".to_owned(),
                scope: BindScope::Class {
                    qualname: "C".to_owned()
                },
            }])
        );
    }

    #[test]
    fn nested_function_yields_dotted_qualname() {
        let original_src = "def outer():\n    def inner():\n        return 1\n    return inner\n";
        let mutated_src = "def outer():\n    def inner():\n        return 2\n    return inner\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let offset = original_src.find("1\n").unwrap();
        let mutant = make_mutant("nested.py", "1", "2", offset);

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src)
            .expect("derive_diff should succeed");

        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::FunctionBody {
                qualname,
                new_source,
            } => {
                assert_eq!(qualname, "outer.inner");
                assert!(new_source.contains("return 2"));
                assert!(new_source.starts_with("def inner"));
            }
            other => panic!("expected FunctionBody for outer.inner, got {other:?}"),
        }
    }

    #[test]
    fn decorator_removal_yields_module_attr_variant() {
        let original_src = "@cache\ndef foo():\n    return 1\n";
        let mutated_src = "def foo():\n    return 1\n";
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

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src)
            .expect("derive_diff should succeed");

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

    #[test]
    fn shrinking_mutation_does_not_panic_or_underflow() {
        // Decorator removal: replaces "@cache\n" with "" — mutated_text shorter than byte_length.
        let original_src = "@cache\ndef foo():\n    return 1\n";
        let mutated_src = "def foo():\n    return 1\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let mutant = Mutant {
            file_path: "f.py".into(),
            line: 1,
            column: 1,
            byte_offset: 0,
            byte_length: "@cache\n".len(),
            original_text: "@cache\n".into(),
            mutated_text: String::new(),
            mutator_name: "remove_decorator".to_owned(),
        };
        // Must not panic.
        drop(derive_diff(
            &mutant,
            &original_ast,
            &mutated_ast,
            mutated_src,
        ));
    }

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

    #[test]
    fn property_setter_yields_fset_suffix() {
        let original_src =
            "class Foo:\n    @x.setter\n    def x(self, v):\n        self._x = v + 1\n";
        let mutated_src =
            "class Foo:\n    @x.setter\n    def x(self, v):\n        self._x = v - 1\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let plus_offset = original_src.find("v + 1").unwrap() + 2;
        let mutant = make_mutant("p.py", "+", "-", plus_offset);

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src)
            .expect("derive_diff should succeed");
        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::ClassMethod {
                class_qualname,
                method_name,
                ..
            } => {
                assert_eq!(class_qualname, "Foo");
                assert_eq!(method_name, "x.fset");
            }
            other => panic!("expected ClassMethod with x.fset, got {other:?}"),
        }
    }

    #[test]
    fn statement_bind_serializes_with_scope_tag() {
        use serde_json::json;
        let diff = MutationDiff::StatementBind {
            names: vec!["X".to_owned(), "Y".to_owned()],
            stmt_source: "X, Y = 1, 2".to_owned(),
            scope: BindScope::Module,
        };
        let v = serde_json::to_value(&diff).unwrap();
        assert_eq!(
            v,
            json!({
                "kind": "statement_bind",
                "names": ["X", "Y"],
                "stmt_source": "X, Y = 1, 2",
                "scope": {"kind": "module"},
            })
        );
    }

    #[test]
    fn statement_bind_class_scope_includes_qualname() {
        use serde_json::json;
        let diff = MutationDiff::StatementBind {
            names: vec!["counter".to_owned()],
            stmt_source: "counter: int = 5".to_owned(),
            scope: BindScope::Class {
                qualname: "Outer.Inner".to_owned(),
            },
        };
        let v = serde_json::to_value(&diff).unwrap();
        assert_eq!(
            v["scope"],
            json!({"kind": "class", "qualname": "Outer.Inner"})
        );
    }

    #[test]
    fn aug_assign_yields_statement_bind_with_full_stmt() {
        let src = "COUNTER = 0\nCOUNTER += 1\n";
        let mutated = "COUNTER = 0\nCOUNTER += 2\n";
        let module = parse(src);
        // Locate the "1" inside `+= 1`. Compute dynamically to avoid drift.
        let off = src.find("+= 1").unwrap() + "+= ".len();
        let mutant = Mutant {
            file_path: "m.py".into(),
            line: 2,
            column: 11,
            byte_offset: off,
            byte_length: 1,
            original_text: "1".into(),
            mutated_text: "2".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(
            diff,
            Ok(vec![MutationDiff::StatementBind {
                names: vec!["COUNTER".to_owned()],
                stmt_source: "COUNTER += 2".to_owned(),
                scope: BindScope::Module,
            }])
        );
    }

    #[test]
    fn bare_assign_emits_statement_bind_module_scope() {
        let original = "X = 5\n";
        let mutated = "X = 6\n";
        let module = parse(original);
        let mutant = Mutant {
            file_path: "m.py".into(),
            line: 1,
            column: 5,
            byte_offset: 4, // position of "5"
            byte_length: 1,
            original_text: "5".into(),
            mutated_text: "6".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(
            diff,
            Ok(vec![MutationDiff::StatementBind {
                names: vec!["X".to_owned()],
                stmt_source: "X = 6".to_owned(),
                scope: BindScope::Module,
            }])
        );
    }

    #[test]
    fn module_scope_annotated_assign_yields_statement_bind() {
        let src = "X: int = 5\n";
        let mutated = "X: int = 6\n";
        let module = parse(src);
        let mutant = Mutant {
            file_path: "m.py".into(),
            line: 1,
            column: 9,
            byte_offset: 9,
            byte_length: 1,
            original_text: "5".into(),
            mutated_text: "6".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(
            diff,
            Ok(vec![MutationDiff::StatementBind {
                names: vec!["X".to_owned()],
                stmt_source: "X: int = 6".to_owned(),
                scope: BindScope::Module,
            }])
        );
    }

    #[test]
    fn module_scope_annotation_only_no_value_yields_unsupported_statement() {
        // X: int (no value) — nothing to bind, derive_diff returns UnsupportedStatement.
        let src = "X: int\nY = 5\n";
        let mutated = "X: int\nY = 6\n";
        let module = parse(src);
        let mutant = Mutant {
            file_path: "m.py".into(),
            // Point mutant at the annotation line and verify UnsupportedStatement.
            line: 1,
            column: 3,
            byte_offset: 3,
            byte_length: 3, // "int" position
            original_text: "int".into(),
            mutated_text: "str".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        // AnnAssign with no value yields UnsupportedStatement (no binding to derive).
        assert_eq!(
            diff,
            Err(SkipReason::UnsupportedStatement),
            "annotation-only AnnAssign should return UnsupportedStatement"
        );
    }

    #[test]
    fn property_deleter_yields_fdel_suffix() {
        let original_src = concat!(
            "class Foo:\n",
            "    @x.deleter\n",
            "    def x(self):\n",
            "        self._x = 1\n",
        );
        let mutated_src = concat!(
            "class Foo:\n",
            "    @x.deleter\n",
            "    def x(self):\n",
            "        self._x = 2\n",
        );
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let offset = original_src.find("= 1").unwrap() + 2;
        let mutant = make_mutant("p.py", "1", "2", offset);

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src)
            .expect("derive_diff should succeed");
        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::ClassMethod {
                class_qualname,
                method_name,
                ..
            } => {
                assert_eq!(class_qualname, "Foo");
                assert_eq!(method_name, "x.fdel");
            }
            other => panic!("expected ClassMethod with x.fdel, got {other:?}"),
        }
    }

    #[test]
    fn tuple_unpack_yields_multi_name_bind() {
        let src = "a, b = 1, 2\n";
        let mutated = "a, b = 1, 3\n";
        let module = parse(src);
        let off = src.find(", 2").unwrap() + ", ".len();
        let mutant = Mutant {
            file_path: "m.py".into(),
            line: 1,
            column: 10,
            byte_offset: off,
            byte_length: 1,
            original_text: "2".into(),
            mutated_text: "3".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(
            diff,
            Ok(vec![MutationDiff::StatementBind {
                names: vec!["a".to_owned(), "b".to_owned()],
                stmt_source: "a, b = 1, 3".to_owned(),
                scope: BindScope::Module,
            }])
        );
    }

    #[test]
    fn chained_assign_yields_multi_name_bind() {
        let src = "a = b = 5\n";
        let mutated = "a = b = 6\n";
        let module = parse(src);
        let off = src.find("= 5").unwrap() + "= ".len();
        let mutant = Mutant {
            file_path: "m.py".into(),
            line: 1,
            column: 8,
            byte_offset: off,
            byte_length: 1,
            original_text: "5".into(),
            mutated_text: "6".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        // Multi-target chains: Python AST gives targets = [a, b]. Order matches AST.
        assert_eq!(
            diff,
            Ok(vec![MutationDiff::StatementBind {
                names: vec!["a".to_owned(), "b".to_owned()],
                stmt_source: "a = b = 6".to_owned(),
                scope: BindScope::Module,
            }])
        );
    }

    #[test]
    fn mutation_inside_if_body_recurses() {
        let src = "import sys\nif sys.version_info >= (3, 11):\n    X = 5\n";
        let mutated = "import sys\nif sys.version_info >= (3, 11):\n    X = 6\n";
        let module = parse(src);
        let off = src.find("X = 5").unwrap() + "X = ".len();
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 3,
            column: 9,
            byte_offset: off,
            byte_length: 1,
            original_text: "5".into(),
            mutated_text: "6".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(
            diff,
            Ok(vec![MutationDiff::StatementBind {
                names: vec!["X".to_owned()],
                stmt_source: "X = 6".to_owned(),
                scope: BindScope::Module,
            }])
        );
    }

    #[test]
    fn mutation_inside_if_orelse_recurses() {
        let src = "if False:\n    X = 1\nelse:\n    X = 2\n";
        let mutated = "if False:\n    X = 1\nelse:\n    X = 3\n";
        let module = parse(src);
        let off = src.rfind("X = 2").unwrap() + "X = ".len();
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 4,
            column: 9,
            byte_offset: off,
            byte_length: 1,
            original_text: "2".into(),
            mutated_text: "3".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert!(
            matches!(diff, Ok(ref v) if matches!(v.as_slice(), [MutationDiff::StatementBind { .. }])),
            "expected StatementBind from else branch, got {diff:?}"
        );
    }

    #[test]
    fn mutation_inside_try_handler_recurses() {
        let src = "try:\n    X = 1\nexcept Exception:\n    X = 2\n";
        let mutated = "try:\n    X = 1\nexcept Exception:\n    X = 3\n";
        let module = parse(src);
        let off = src.rfind("X = 2").unwrap() + "X = ".len();
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 4,
            column: 9,
            byte_offset: off,
            byte_length: 1,
            original_text: "2".into(),
            mutated_text: "3".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert!(
            matches!(diff, Ok(ref v) if matches!(v.as_slice(), [MutationDiff::StatementBind { .. }])),
            "expected StatementBind from try handler, got {diff:?}"
        );
    }

    #[test]
    fn mutation_inside_for_body_recurses() {
        let src = "for i in range(3):\n    X = 1\n";
        let mutated = "for i in range(3):\n    X = 2\n";
        let module = parse(src);
        let off = src.find("X = 1").unwrap() + "X = ".len();
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 2,
            column: 9,
            byte_offset: off,
            byte_length: 1,
            original_text: "1".into(),
            mutated_text: "2".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert!(
            matches!(diff, Ok(ref v) if matches!(v.as_slice(), [MutationDiff::StatementBind { .. }])),
            "expected StatementBind from for body, got {diff:?}"
        );
    }

    #[test]
    fn mutation_inside_while_body_recurses() {
        let src = "while True:\n    X = 1\n    break\n";
        let mutated = "while True:\n    X = 2\n    break\n";
        let module = parse(src);
        let off = src.find("X = 1").unwrap() + "X = ".len();
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 2,
            column: 9,
            byte_offset: off,
            byte_length: 1,
            original_text: "1".into(),
            mutated_text: "2".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert!(
            matches!(diff, Ok(ref v) if matches!(v.as_slice(), [MutationDiff::StatementBind { .. }])),
            "expected StatementBind from while body, got {diff:?}"
        );
    }

    #[test]
    fn mutation_in_if_test_yields_condition_mutation() {
        // Mutation on the `if` condition itself returns ConditionMutation.
        let src = "if 1 < 2:\n    X = 1\n";
        let mutated = "if 1 < 3:\n    X = 1\n";
        let module = parse(src);
        let off = src.find("1 < 2").unwrap() + 4; // position of "2"
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 1,
            column: 8,
            byte_offset: off,
            byte_length: 1,
            original_text: "2".into(),
            mutated_text: "3".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(
            diff,
            Err(SkipReason::ConditionMutation),
            "condition mutation should return ConditionMutation, got {diff:?}"
        );
    }

    #[test]
    fn tuple_with_starred_target_yields_unmappable_target() {
        let src = "*a, b = 1, 2, 3\n";
        let mutated = "*a, b = 1, 2, 4\n";
        let module = parse(src);
        let off = src.find(", 3").unwrap() + ", ".len();
        let mutant = Mutant {
            file_path: "m.py".into(),
            line: 1,
            column: 14,
            byte_offset: off,
            byte_length: 1,
            original_text: "3".into(),
            mutated_text: "4".into(),
            mutator_name: "constant".into(),
        };
        let diff = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(
            diff,
            Err(SkipReason::UnmappableTarget),
            "starred unpack target is unmappable, got {diff:?}"
        );
    }

    #[test]
    fn nested_class_method_uses_dotted_qualname() {
        let src = "class Outer:\n    class Inner:\n        def m(self):\n            return 1\n";
        let mutated =
            "class Outer:\n    class Inner:\n        def m(self):\n            return 2\n";
        let module = parse(src);
        let off = src.find("return 1").unwrap() + "return ".len();
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 4,
            column: 20,
            byte_offset: off,
            byte_length: 1,
            original_text: "1".into(),
            mutated_text: "2".into(),
            mutator_name: "constant".into(),
        };
        let diff =
            derive_diff(&mutant, &module, &module, mutated).expect("derive_diff should succeed");
        match diff.as_slice() {
            [
                MutationDiff::ClassMethod {
                    class_qualname,
                    method_name,
                    ..
                },
            ] => {
                assert_eq!(class_qualname, "Outer.Inner");
                assert_eq!(method_name, "m");
            }
            other => panic!("expected ClassMethod with qualname Outer.Inner, got {other:?}"),
        }
    }

    #[test]
    fn nested_class_attr_uses_dotted_qualname() {
        let src = "class Outer:\n    class Inner:\n        X = 5\n";
        let mutated = "class Outer:\n    class Inner:\n        X = 6\n";
        let module = parse(src);
        let off = src.find("X = 5").unwrap() + "X = ".len();
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 3,
            column: 13,
            byte_offset: off,
            byte_length: 1,
            original_text: "5".into(),
            mutated_text: "6".into(),
            mutator_name: "constant".into(),
        };
        let diff =
            derive_diff(&mutant, &module, &module, mutated).expect("derive_diff should succeed");
        match diff.as_slice() {
            [
                MutationDiff::StatementBind {
                    names,
                    scope: BindScope::Class { qualname },
                    ..
                },
            ] => {
                assert_eq!(qualname, "Outer.Inner");
                assert_eq!(names, &vec!["X".to_owned()]);
            }
            other => panic!("expected StatementBind class scope Outer.Inner, got {other:?}"),
        }
    }

    #[test]
    fn condition_mutation_returns_condition_skip_reason() {
        let src = "if 1 < 2:\n    X = 1\n";
        let mutated = "if 1 < 3:\n    X = 1\n";
        let module = parse(src);
        let off = src.find("1 < 2").unwrap() + 4; // position of "2"
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 1,
            column: 8,
            byte_offset: off,
            byte_length: 1,
            original_text: "2".into(),
            mutated_text: "3".into(),
            mutator_name: "constant".into(),
        };
        let result = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(result, Err(SkipReason::ConditionMutation));
    }

    #[test]
    fn while_condition_mutation_returns_condition_skip_reason() {
        let src = "while 1 < 2:\n    X = 1\n    break\n";
        let mutated = "while 1 < 3:\n    X = 1\n    break\n";
        let module = parse(src);
        let off = src.find("1 < 2").unwrap() + 4;
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 1,
            column: 11,
            byte_offset: off,
            byte_length: 1,
            original_text: "2".into(),
            mutated_text: "3".into(),
            mutator_name: "constant".into(),
        };
        let result = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(result, Err(SkipReason::ConditionMutation));
    }

    #[test]
    fn for_iter_mutation_returns_condition_skip_reason() {
        let src = "for i in range(5):\n    X = 1\n";
        let mutated = "for i in range(6):\n    X = 1\n";
        let module = parse(src);
        let off = src.find("range(5)").unwrap() + "range(".len();
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 1,
            column: 16,
            byte_offset: off,
            byte_length: 1,
            original_text: "5".into(),
            mutated_text: "6".into(),
            mutator_name: "constant".into(),
        };
        let result = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(result, Err(SkipReason::ConditionMutation));
    }

    #[test]
    fn subscript_target_returns_unmappable_target() {
        let src = "d = {}\nd['k'] = 1\n";
        let mutated = "d = {}\nd['k'] = 2\n";
        let module = parse(src);
        let off = src.find("'k'] = 1").unwrap() + "'k'] = ".len();
        let mutant = Mutant {
            file_path: std::path::PathBuf::from("m.py"),
            line: 2,
            column: 9,
            byte_offset: off,
            byte_length: 1,
            original_text: "1".into(),
            mutated_text: "2".into(),
            mutator_name: "constant".into(),
        };
        let result = derive_diff(&mutant, &module, &module, mutated);
        assert_eq!(result, Err(SkipReason::UnmappableTarget));
    }
}
