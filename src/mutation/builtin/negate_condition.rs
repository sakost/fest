//! Negate condition mutation.
//!
//! Wraps `if`/`elif`/`while` conditions with `not (...)`.

use ruff_python_ast::Stmt;
use ruff_text_size::Ranged;

use crate::mutation::mutator::{Mutation, Mutator};

/// Mutator that negates conditions in `if`, `elif`, and `while` statements.
#[derive(Debug)]
pub struct NegateCondition;

/// Create a mutation that wraps a condition expression with `not (...)`.
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn negate_mutation(source: &str, start: usize, end: usize) -> Mutation {
    let original = &source[start..end];
    let mut replacement = String::with_capacity(6_usize + original.len());
    replacement.push_str("not (");
    replacement.push_str(original);
    replacement.push(')');
    Mutation {
        byte_offset: start,
        byte_length: end - start,
        original_text: original.to_owned(),
        replacement_text: replacement,
    }
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
        Stmt::If(if_stmt) => {
            let start = if_stmt.test.range().start().to_usize();
            let end = if_stmt.test.range().end().to_usize();
            out.push(negate_mutation(source, start, end));
            walk_stmts(&if_stmt.body, source, out);
            walk_elif_clauses(if_stmt, source, out);
        }
        Stmt::While(while_stmt) => {
            let start = while_stmt.test.range().start().to_usize();
            let end = while_stmt.test.range().end().to_usize();
            out.push(negate_mutation(source, start, end));
            walk_stmts(&while_stmt.body, source, out);
            walk_stmts(&while_stmt.orelse, source, out);
        }
        Stmt::FunctionDef(s) => walk_stmts(&s.body, source, out),
        Stmt::ClassDef(s) => walk_stmts(&s.body, source, out),
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

/// Walk elif/else clauses of an `if` statement.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on Option<Expr> requires suppression"
)]
fn walk_elif_clauses(if_stmt: &ruff_python_ast::StmtIf, source: &str, out: &mut Vec<Mutation>) {
    for clause in &if_stmt.elif_else_clauses {
        if let Some(test) = &clause.test {
            let cstart = test.range().start().to_usize();
            let cend = test.range().end().to_usize();
            out.push(negate_mutation(source, cstart, cend));
        }
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
    walk_stmts(&try_stmt.orelse, source, out);
    walk_stmts(&try_stmt.finalbody, source, out);
}

impl Mutator for NegateCondition {
    #[inline]
    fn name(&self) -> &'static str {
        "negate_condition"
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
        NegateCondition.find_mutations(source, &ast)
    }

    /// An `if` condition is negated.
    #[test]
    fn negate_if_condition() {
        let source = "if x:\n    pass";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "x");
        assert_eq!(mutations[0_usize].replacement_text, "not (x)");
    }

    /// A `while` condition is negated.
    #[test]
    fn negate_while_condition() {
        let source = "while running:\n    pass";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "running");
        assert_eq!(mutations[0_usize].replacement_text, "not (running)");
    }

    /// An `elif` condition is also negated.
    #[test]
    fn negate_elif_condition() {
        let source = "if x:\n    pass\nelif y:\n    pass";
        let mutations = find(source);
        assert_eq!(mutations.len(), 2_usize);
        assert_eq!(mutations[0_usize].original_text, "x");
        assert_eq!(mutations[1_usize].original_text, "y");
    }

    /// `else` clause (no condition) does not produce a mutation.
    #[test]
    fn no_mutation_for_else() {
        let source = "if x:\n    pass\nelse:\n    pass";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
    }

    /// Nested `if` inside function produces mutations.
    #[test]
    fn nested_if_in_function() {
        let source = "def func():\n    if a:\n        pass";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "a");
    }
}
