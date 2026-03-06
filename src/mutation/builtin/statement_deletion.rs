//! Statement deletion mutation.
//!
//! Replaces individual statements with `pass`.

use ruff_python_ast::Stmt;
use ruff_text_size::Ranged;

use crate::mutation::mutator::{Mutation, MutationContext, Mutator};

/// Mutator that replaces statements with `pass`.
#[derive(Debug)]
pub struct StatementDeletion;

/// Check if a statement is deletable (not a `pass`, `import`, or structural).
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt requires pattern_type_mismatch suppression"
)]
const fn is_deletable(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::Expr(_)
            | Stmt::Assign(_)
            | Stmt::AugAssign(_)
            | Stmt::AnnAssign(_)
            | Stmt::Return(_)
            | Stmt::Delete(_)
            | Stmt::Raise(_)
            | Stmt::Assert(_)
    )
}

/// Create a mutation replacing a statement with `pass`.
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn stmt_deletion_mutation(stmt: &Stmt, source: &str) -> Mutation {
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
        if is_deletable(stmt) {
            out.push(stmt_deletion_mutation(stmt, source));
        }
        walk_stmt(stmt, source, out);
    }
}

/// Walk a single statement into nested bodies.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt requires pattern_type_mismatch suppression"
)]
fn walk_stmt(stmt: &Stmt, source: &str, out: &mut Vec<Mutation>) {
    match stmt {
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
            walk_stmts(&s.orelse, source, out);
            walk_stmts(&s.finalbody, source, out);
        }
        Stmt::With(s) => walk_stmts(&s.body, source, out),
        Stmt::Return(_)
        | Stmt::Delete(_)
        | Stmt::TypeAlias(_)
        | Stmt::Assign(_)
        | Stmt::AugAssign(_)
        | Stmt::AnnAssign(_)
        | Stmt::Match(_)
        | Stmt::Raise(_)
        | Stmt::Assert(_)
        | Stmt::Import(_)
        | Stmt::ImportFrom(_)
        | Stmt::Global(_)
        | Stmt::Nonlocal(_)
        | Stmt::Expr(_)
        | Stmt::Pass(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::IpyEscapeCommand(_) => {}
    }
}

impl Mutator for StatementDeletion {
    #[inline]
    fn name(&self) -> &'static str {
        "statement_deletion"
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
        StatementDeletion.find_mutations(source, &ast, &ctx)
    }

    /// An assignment statement is replaced with `pass`.
    #[test]
    fn delete_assignment() {
        let source = "x = 1";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].replacement_text, "pass");
    }

    /// `pass` itself is not deleted.
    #[test]
    fn pass_not_deleted() {
        let source = "pass";
        let mutations = find(source);
        assert!(mutations.is_empty());
    }

    /// Mutator name matches expected identifier.
    #[test]
    fn mutator_name() {
        assert_eq!(StatementDeletion.name(), "statement_deletion");
    }

    /// Statements inside function bodies are found via walk_stmt.
    #[test]
    fn inside_function_body() {
        let source = "def func():\n    x = 1";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].replacement_text, "pass");
    }

    /// Byte offset and length of statement deletion are correct.
    #[test]
    fn deletion_byte_offset() {
        let source = "x = 1";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].byte_offset, 0_usize);
        assert_eq!(mutations[0_usize].byte_length, source.len());
    }
}
