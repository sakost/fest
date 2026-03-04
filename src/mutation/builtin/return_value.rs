//! Return value mutation.
//!
//! Replaces `return expr` with `return None`, and swaps `return True`
//! with `return False` (and vice versa).

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::mutation::mutator::{Mutation, Mutator};

/// Mutator that modifies return values.
#[derive(Debug)]
pub struct ReturnValue;

/// Collect mutations from a single `return` statement.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr requires pattern_type_mismatch suppression"
)]
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn return_mutation(ret: &ruff_python_ast::StmtReturn, source: &str) -> Option<Mutation> {
    let value = ret.value.as_ref()?;
    let value_start = value.range().start().to_usize();
    let value_end = value.range().end().to_usize();
    let original_text = &source[value_start..value_end];

    let replacement_text = match value.as_ref() {
        Expr::BooleanLiteral(bool_lit) => if bool_lit.value { "False" } else { "True" }.to_owned(),
        Expr::BoolOp(_)
        | Expr::Named(_)
        | Expr::BinOp(_)
        | Expr::UnaryOp(_)
        | Expr::Lambda(_)
        | Expr::If(_)
        | Expr::Dict(_)
        | Expr::Set(_)
        | Expr::ListComp(_)
        | Expr::SetComp(_)
        | Expr::DictComp(_)
        | Expr::Generator(_)
        | Expr::Await(_)
        | Expr::Yield(_)
        | Expr::YieldFrom(_)
        | Expr::Compare(_)
        | Expr::Call(_)
        | Expr::FString(_)
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_)
        | Expr::Attribute(_)
        | Expr::Subscript(_)
        | Expr::Starred(_)
        | Expr::Name(_)
        | Expr::List(_)
        | Expr::Tuple(_)
        | Expr::Slice(_)
        | Expr::IpyEscapeCommand(_) => "None".to_owned(),
    };

    Some(Mutation {
        byte_offset: value_start,
        byte_length: value_end - value_start,
        original_text: original_text.to_owned(),
        replacement_text,
    })
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
        Stmt::Return(ret) => {
            if let Some(mutation) = return_mutation(ret, source) {
                out.push(mutation);
            }
        }
        Stmt::FunctionDef(s) => walk_stmts(&s.body, source, out),
        Stmt::ClassDef(s) => walk_stmts(&s.body, source, out),
        Stmt::If(s) => walk_if(s, source, out),
        Stmt::While(s) => {
            walk_stmts(&s.body, source, out);
            walk_stmts(&s.orelse, source, out);
        }
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
    walk_stmts(&try_stmt.orelse, source, out);
    walk_stmts(&try_stmt.finalbody, source, out);
}

impl Mutator for ReturnValue {
    #[inline]
    fn name(&self) -> &'static str {
        "return_value"
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
        ReturnValue.find_mutations(source, &ast)
    }

    /// `return expr` is replaced with `return None`.
    #[test]
    fn return_expr_to_none() {
        let source = "def calc():\n    return 42";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "42");
        assert_eq!(mutations[0_usize].replacement_text, "None");
    }

    /// `return True` is swapped to `return False`.
    #[test]
    fn return_true_to_false() {
        let source = "def check():\n    return True";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "True");
        assert_eq!(mutations[0_usize].replacement_text, "False");
    }

    /// `return False` is swapped to `return True`.
    #[test]
    fn return_false_to_true() {
        let source = "def check():\n    return False";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "False");
        assert_eq!(mutations[0_usize].replacement_text, "True");
    }

    /// Bare `return` (no value) produces no mutations.
    #[test]
    fn bare_return_no_mutation() {
        let source = "def noop():\n    return";
        let mutations = find(source);
        assert!(mutations.is_empty());
    }

    /// Return inside nested if produces mutations.
    #[test]
    fn return_inside_if() {
        let source = "def calc():\n    if True:\n        return 1\n    return 2";
        let mutations = find(source);
        assert_eq!(mutations.len(), 2_usize);
    }
}
