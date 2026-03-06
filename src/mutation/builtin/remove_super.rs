//! Super-call removal mutation.
//!
//! Removes `super().__init__(...)` calls from `__init__` methods.

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::mutation::mutator::{Mutation, MutationContext, Mutator};

/// Mutator that removes `super()` calls.
#[derive(Debug)]
pub struct RemoveSuperCall;

/// Check if an expression is a `super()` call chain.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr requires pattern_type_mismatch suppression"
)]
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive enum matching on Expr requires many arms"
)]
fn is_super_call(expr: &Expr) -> bool {
    match expr {
        Expr::Call(call) => match call.func.as_ref() {
            Expr::Attribute(attr) => is_super_call(&attr.value),
            #[allow(clippy::string_slice, reason = "identifier access is safe")]
            Expr::Name(name) => name.id.as_str() == "super",
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
            | Expr::BooleanLiteral(_)
            | Expr::NoneLiteral(_)
            | Expr::EllipsisLiteral(_)
            | Expr::Subscript(_)
            | Expr::Starred(_)
            | Expr::List(_)
            | Expr::Tuple(_)
            | Expr::Slice(_)
            | Expr::IpyEscapeCommand(_) => false,
        },
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
        | Expr::FString(_)
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_)
        | Expr::Attribute(_)
        | Expr::Subscript(_)
        | Expr::Starred(_)
        | Expr::Name(_)
        | Expr::List(_)
        | Expr::Tuple(_)
        | Expr::Slice(_)
        | Expr::IpyEscapeCommand(_) => false,
    }
}

/// Create a mutation that replaces a super call statement with `pass`.
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn super_call_mutation(stmt: &Stmt, source: &str) -> Mutation {
    let start = stmt.range().start().to_usize();
    let end = stmt.range().end().to_usize();
    let original = &source[start..end];
    Mutation {
        byte_offset: start,
        byte_length: end - start,
        original_text: original.to_owned(),
        replacement_text: "pass".to_owned(),
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
        Stmt::Expr(s) => {
            if is_super_call(&s.value) {
                out.push(super_call_mutation(stmt, source));
            }
        }
        Stmt::FunctionDef(s) => walk_stmts(&s.body, source, out),
        Stmt::ClassDef(s) => walk_stmts(&s.body, source, out),
        Stmt::If(s) => {
            walk_stmts(&s.body, source, out);
            for clause in &s.elif_else_clauses {
                walk_stmts(&clause.body, source, out);
            }
        }
        Stmt::While(s) => walk_stmts(&s.body, source, out),
        Stmt::For(s) => {
            walk_stmts(&s.body, source, out);
            walk_stmts(&s.orelse, source, out);
        }
        Stmt::Try(s) => {
            walk_stmts(&s.body, source, out);
            for handler in &s.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                walk_stmts(&h.body, source, out);
            }
        }
        Stmt::With(s) => walk_stmts(&s.body, source, out),
        Stmt::Assign(_)
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

impl Mutator for RemoveSuperCall {
    #[inline]
    fn name(&self) -> &'static str {
        "remove_super_call"
    }

    #[inline]
    fn find_mutations(
        &self,
        source: &str,
        ast: &ruff_python_ast::ModModule,
        _ctx: &MutationContext<'_>,
    ) -> Vec<Mutation> {
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
        let ctx = MutationContext {
            file_path: std::path::Path::new("test.py"),
            seed: None,
        };
        RemoveSuperCall.find_mutations(source, &ast, &ctx)
    }

    /// `super().__init__()` is replaced with `pass`.
    #[test]
    fn remove_super_init() {
        let source = "class A(B):\n    def __init__(self):\n        super().__init__()";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].replacement_text, "pass");
    }

    /// Regular function calls are not affected.
    #[test]
    fn regular_call_not_affected() {
        let source = "class A:\n    def method(self):\n        other()";
        let mutations = find(source);
        assert!(mutations.is_empty());
    }

    /// Byte offset and length of super call mutation are correct.
    #[test]
    fn super_call_byte_offset() {
        let source = "class A(B):\n    def __init__(self):\n        super().__init__()";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find("super().__init__()").expect("has super");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, "super().__init__()".len());
    }
}
