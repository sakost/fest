//! Exception swallowing mutation.
//!
//! Replaces the body of `except` handlers with `pass`, effectively
//! swallowing exceptions.

use ruff_python_ast::Stmt;
use ruff_text_size::Ranged;

use crate::mutation::mutator::{Mutation, Mutator};

/// Mutator that replaces exception handler bodies with `pass`.
#[derive(Debug)]
pub struct ExceptionSwallow;

/// Create a mutation that replaces the body of an except handler with `pass`.
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn except_body_mutation(
    handler: &ruff_python_ast::ExceptHandlerExceptHandler,
    source: &str,
) -> Option<Mutation> {
    if is_pass_only(&handler.body) {
        return None;
    }
    let first_stmt = handler.body.first()?;
    let last_stmt = handler.body.last()?;
    let body_start = first_stmt.range().start().to_usize();
    let body_end = last_stmt.range().end().to_usize();
    let original = &source[body_start..body_end];
    Some(Mutation {
        byte_offset: body_start,
        byte_length: body_end - body_start,
        original_text: original.to_owned(),
        replacement_text: "pass".to_owned(),
    })
}

/// Check if a body consists of only a single `pass` statement.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on Option<&Stmt> requires suppression"
)]
const fn is_pass_only(body: &[Stmt]) -> bool {
    body.len() == 1_usize && matches!(body.first(), Some(Stmt::Pass(_)))
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
        Stmt::Try(try_stmt) => {
            walk_stmts(&try_stmt.body, source, out);
            walk_try_handlers(try_stmt, source, out);
            walk_stmts(&try_stmt.orelse, source, out);
            walk_stmts(&try_stmt.finalbody, source, out);
        }
        Stmt::FunctionDef(s) => walk_stmts(&s.body, source, out),
        Stmt::ClassDef(s) => walk_stmts(&s.body, source, out),
        Stmt::If(s) => walk_if(s, source, out),
        Stmt::While(s) => walk_stmts(&s.body, source, out),
        Stmt::For(s) => {
            walk_stmts(&s.body, source, out);
            walk_stmts(&s.orelse, source, out);
        }
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

/// Walk except handlers for a try statement.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on ExceptHandler requires suppression"
)]
fn walk_try_handlers(try_stmt: &ruff_python_ast::StmtTry, source: &str, out: &mut Vec<Mutation>) {
    for handler in &try_stmt.handlers {
        let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
        if let Some(mutation) = except_body_mutation(h, source) {
            out.push(mutation);
        }
        walk_stmts(&h.body, source, out);
    }
}

/// Walk an `if` statement.
fn walk_if(if_stmt: &ruff_python_ast::StmtIf, source: &str, out: &mut Vec<Mutation>) {
    walk_stmts(&if_stmt.body, source, out);
    for clause in &if_stmt.elif_else_clauses {
        walk_stmts(&clause.body, source, out);
    }
}

impl Mutator for ExceptionSwallow {
    #[inline]
    fn name(&self) -> &'static str {
        "exception_swallow"
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
        ExceptionSwallow.find_mutations(source, &ast)
    }

    /// An except handler body is replaced with `pass`.
    #[test]
    fn replace_except_body_with_pass() {
        let source = "try:\n    risky()\nexcept Exception:\n    log(error)";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].replacement_text, "pass");
        assert!(mutations[0_usize].original_text.contains("log"));
    }

    /// An except handler that already has `pass` produces no mutation.
    #[test]
    fn already_pass_no_mutation() {
        let source = "try:\n    risky()\nexcept Exception:\n    pass";
        let mutations = find(source);
        assert!(mutations.is_empty());
    }

    /// Multiple except handlers produce multiple mutations.
    #[test]
    fn multiple_handlers() {
        let source =
            "try:\n    risky()\nexcept ValueError:\n    handle_v()\nexcept TypeError:\n    \
             handle_t()";
        let mutations = find(source);
        assert_eq!(mutations.len(), 2_usize);
    }

    /// Nested try in function is found.
    #[test]
    fn nested_try_in_function() {
        let source = "def func():\n    try:\n        op()\n    except:\n        recover()";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
    }
}
