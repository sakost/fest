//! Decorator removal mutation.
//!
//! Removes each decorator from functions and classes one at a time.

use ruff_python_ast::Stmt;

use crate::mutation::mutator::{Mutation, Mutator};

/// Mutator that removes decorators from functions and classes.
#[derive(Debug)]
pub struct RemoveDecorator;

/// Create mutations that remove individual decorators from a decorator list.
///
/// Each decorator (from `@` to end of line, including trailing newline) is
/// replaced with an empty string.
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn collect_decorator_mutations(
    decorator_list: &[ruff_python_ast::Decorator],
    source: &str,
    out: &mut Vec<Mutation>,
) {
    for decorator in decorator_list {
        let start = decorator.range.start().to_usize();
        let end = decorator.range.end().to_usize();
        let extended_end = skip_trailing_newline(source, end);
        let original = &source[start..extended_end];
        out.push(Mutation {
            byte_offset: start,
            byte_length: extended_end - start,
            original_text: original.to_owned(),
            replacement_text: String::new(),
        });
    }
}

/// Skip past a trailing newline character if present.
fn skip_trailing_newline(source: &str, offset: usize) -> usize {
    let bytes = source.as_bytes();
    if bytes.get(offset).copied() == Some(b'\n') {
        return offset + 1_usize;
    }
    if bytes.get(offset).copied() == Some(b'\r')
        && bytes.get(offset + 1_usize).copied() == Some(b'\n')
    {
        return offset + 2_usize;
    }
    offset
}

/// Walk all statements.
fn walk_stmts(stmts: &[Stmt], source: &str, out: &mut Vec<Mutation>) {
    for stmt in stmts {
        walk_stmt(stmt, source, out);
    }
}

/// Walk a single statement.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt requires pattern_type_mismatch suppression"
)]
fn walk_stmt(stmt: &Stmt, source: &str, out: &mut Vec<Mutation>) {
    match stmt {
        Stmt::FunctionDef(func_def) => {
            collect_decorator_mutations(&func_def.decorator_list, source, out);
            walk_stmts(&func_def.body, source, out);
        }
        Stmt::ClassDef(class_def) => {
            collect_decorator_mutations(&class_def.decorator_list, source, out);
            walk_stmts(&class_def.body, source, out);
        }
        Stmt::If(s) => walk_if(s, source, out),
        Stmt::While(s) => walk_stmts(&s.body, source, out),
        Stmt::For(s) => {
            walk_stmts(&s.body, source, out);
            walk_stmts(&s.orelse, source, out);
        }
        Stmt::Try(s) => walk_try(s, source, out),
        Stmt::With(s) => walk_stmts(&s.body, source, out),
        Stmt::Expr(_)
        | Stmt::Assign(_)
        | Stmt::AugAssign(_)
        | Stmt::AnnAssign(_)
        | Stmt::Return(_)
        | Stmt::Delete(_)
        | Stmt::TypeAlias(_)
        | Stmt::Import(_)
        | Stmt::ImportFrom(_)
        | Stmt::Global(_)
        | Stmt::Nonlocal(_)
        | Stmt::Raise(_)
        | Stmt::Match(_)
        | Stmt::Assert(_)
        | Stmt::Pass(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::IpyEscapeCommand(_) => {}
    }
}

/// Walk an `if` statement.
fn walk_if(if_stmt: &ruff_python_ast::StmtIf, source: &str, out: &mut Vec<Mutation>) {
    walk_stmts(&if_stmt.body, source, out);
    for clause in &if_stmt.elif_else_clauses {
        walk_stmts(&clause.body, source, out);
    }
}

/// Walk a `try` statement.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on ExceptHandler requires suppression"
)]
fn walk_try(try_stmt: &ruff_python_ast::StmtTry, source: &str, out: &mut Vec<Mutation>) {
    walk_stmts(&try_stmt.body, source, out);
    for handler in &try_stmt.handlers {
        let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
        walk_stmts(&h.body, source, out);
    }
}

impl Mutator for RemoveDecorator {
    #[inline]
    fn name(&self) -> &'static str {
        "remove_decorator"
    }

    #[inline]
    fn find_mutations(&self, source: &str, ast: &ruff_python_ast::ModModule) -> Vec<Mutation> {
        let mut out = Vec::new();
        walk_stmts(&ast.body, source, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: parse Python source and run the mutator.
    #[allow(clippy::expect_used, reason = "tests use expect for parse failures")]
    fn find(source: &str) -> Vec<Mutation> {
        let parsed = ruff_python_parser::parse_module(source)
            .expect("valid Python source required for test");
        let ast = parsed.into_syntax();
        RemoveDecorator.find_mutations(source, &ast)
    }

    /// A single decorator is removed.
    #[test]
    fn remove_single_decorator() {
        let source = "@my_decorator\ndef func():\n    pass";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].replacement_text, "");
        assert!(mutations[0_usize].original_text.contains("@my_decorator"));
    }

    /// Two decorators produce two mutations.
    #[test]
    fn remove_multiple_decorators() {
        let source = "@dec1\n@dec2\ndef func():\n    pass";
        let mutations = find(source);
        assert_eq!(mutations.len(), 2_usize);
    }

    /// Class decorators are also removed.
    #[test]
    fn remove_class_decorator() {
        let source = "@dataclass\nclass Pt:\n    x: int";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert!(mutations[0_usize].original_text.contains("@dataclass"));
    }

    /// Functions without decorators produce no mutations.
    #[test]
    fn no_decorators_no_mutation() {
        let source = "def func():\n    pass";
        let mutations = find(source);
        assert!(mutations.is_empty());
    }

    /// Nested function decorators are found.
    #[test]
    fn nested_function_decorator() {
        let source = "class Cls:\n    @staticmethod\n    def method():\n        pass";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
    }
}
