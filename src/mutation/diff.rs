//! Structured diff IR for mutations dispatched to the plugin backend.

use serde::{Deserialize, Serialize};

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

    /// Module-level `NAME = expr` binding changed value.
    ConstantBind {
        /// Name of the module-level binding.
        name: String,
        /// New expression text for the right-hand side.
        new_expr: String,
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

    /// Class-level non-method attribute changed.
    ClassAttr {
        /// Dotted qualified name of the class containing the attribute.
        class_qualname: String,
        /// Name of the class attribute.
        name: String,
        /// New expression text for the attribute value.
        new_expr: String,
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

use crate::mutation::Mutant;

/// Derive a list of [`MutationDiff`] entries describing the structural change
/// introduced by `mutant` when applied to `original_ast`.
///
/// Currently returns at most one entry. Returns an empty `Vec` when the
/// mutated statement does not map to any supported diff variant.
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

/// Attempt to derive a [`MutationDiff`] for a single top-level statement.
///
/// Returns `None` when the statement kind is not yet supported.
#[allow(
    clippy::string_slice,
    reason = "byte offsets originate from the AST and are always valid UTF-8 boundaries"
)]
fn derive_for_top_level(
    stmt: &Stmt,
    mutated_source: &str,
    mutant: &Mutant,
) -> Option<MutationDiff> {
    match stmt {
        Stmt::FunctionDef(func) => {
            let stmt_start = stmt.range().start().to_usize();
            let stmt_end_in_mutated =
                mutated_stmt_end(stmt.range().end().to_usize(), mutant);
            let new_source = strip_decorators(
                mutated_source
                    .get(stmt_start..stmt_end_in_mutated)
                    .unwrap_or(""),
            );
            Some(MutationDiff::FunctionBody {
                qualname: func.name.id.to_string(),
                new_source,
            })
        }
        Stmt::Assign(assign) => {
            let target = assign.targets.first()?;
            let name = match target {
                ruff_python_ast::Expr::Name(n) => n.id.to_string(),
                _ => return None,
            };
            let value_range = assign.value.range();
            let new_expr = mutated_source
                .get(
                    value_range.start().to_usize()
                        ..mutated_stmt_end(value_range.end().to_usize(), mutant),
                )
                .unwrap_or("")
                .to_owned();
            Some(MutationDiff::ConstantBind { name, new_expr })
        }
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
                        let stmt_end = mutated_stmt_end(end, mutant);
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
                            .get(
                                value_range.start().to_usize()
                                    ..mutated_stmt_end(
                                        value_range.end().to_usize(),
                                        mutant,
                                    ),
                            )
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
        _ => None,
    }
}

/// Compute the new end byte offset of a statement after a mutation is applied.
///
/// Uses signed arithmetic to avoid usize underflow when the mutated text is shorter
/// than the original (e.g. decorator removal replaces `@cache\n` with `""`).
fn mutated_stmt_end(original_end: usize, mutant: &Mutant) -> usize {
    let delta = mutant.mutated_text.len() as isize - mutant.byte_length as isize;
    original_end.checked_add_signed(delta).unwrap_or(original_end)
}

/// Return the property accessor suffix for a method decorated with `@property`,
/// `@x.setter`, or `@x.deleter`.
///
/// Returns `"fget"` for `@property`, `"fset"` for `@x.setter`, `"fdel"` for
/// `@x.deleter`, and `""` for plain (non-property) methods.
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
        let _ = lines.remove(0);
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
    use crate::mutation::Mutant;

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

    #[test]
    fn constant_bind_handles_shrinking_mutation() {
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
        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src);
        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::ConstantBind { name, new_expr } => {
                assert_eq!(name, "MAX");
                assert_eq!(new_expr.trim(), "1");
            }
            other => panic!("expected ConstantBind, got {other:?}"),
        }
    }

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

    #[test]
    fn class_attr_yields_class_attr_variant() {
        let original_src = "class Cfg:\n    LIMIT = 10\n";
        let mutated_src  = "class Cfg:\n    LIMIT = 11\n";
        let original_ast = parse(original_src);
        let mutated_ast = parse(mutated_src);
        let offset = original_src.find("10").unwrap();
        let mutant = make_mutant("cfg.py", "10", "11", offset);

        let diffs = derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src);

        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            MutationDiff::ClassAttr { class_qualname, name, new_expr } => {
                assert_eq!(class_qualname, "Cfg");
                assert_eq!(name, "LIMIT");
                assert_eq!(new_expr.trim(), "11");
            }
            other => panic!("expected ClassAttr, got {other:?}"),
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
        drop(derive_diff(&mutant, &original_ast, &mutated_ast, mutated_src));
    }
}
