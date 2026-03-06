//! Break/continue swap mutation.
//!
//! Swaps `break` <-> `continue` statements.

use ruff_python_ast::Stmt;

use crate::mutation::mutator::{Mutation, MutationContext, Mutator};

/// Mutator that swaps `break` and `continue` statements.
#[derive(Debug)]
pub struct BreakContinue;

use ruff_text_size::Ranged;

/// Create a swap mutation for a break or continue statement.
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn swap_mutation(stmt: &Stmt, source: &str, replacement: &str) -> Mutation {
    let start = stmt.range().start().to_usize();
    let end = stmt.range().end().to_usize();
    let original = &source[start..end];
    Mutation {
        byte_offset: start,
        byte_length: end - start,
        original_text: original.to_owned(),
        replacement_text: replacement.to_owned(),
    }
}

/// Walk all statements.
fn walk_stmts(stmts: &[Stmt], source: &str, out: &mut Vec<Mutation>) {
    for stmt in stmts {
        walk_stmt(stmt, source, out);
    }
}

/// Walk a single statement looking for break/continue.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt requires pattern_type_mismatch suppression"
)]
fn walk_stmt(stmt: &Stmt, source: &str, out: &mut Vec<Mutation>) {
    match stmt {
        Stmt::Break(_) => out.push(swap_mutation(stmt, source, "continue")),
        Stmt::Continue(_) => out.push(swap_mutation(stmt, source, "break")),
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

impl Mutator for BreakContinue {
    #[inline]
    fn name(&self) -> &'static str {
        "break_continue"
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
        BreakContinue.find_mutations(source, &ast, &ctx)
    }

    /// `break` is swapped to `continue`.
    #[test]
    fn swap_break_to_continue() {
        let source = "for x in y:\n    break";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "break");
        assert_eq!(mutations[0_usize].replacement_text, "continue");
    }

    /// `continue` is swapped to `break`.
    #[test]
    fn swap_continue_to_break() {
        let source = "for x in y:\n    continue";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "continue");
        assert_eq!(mutations[0_usize].replacement_text, "break");
    }

    /// Break inside if block within a loop is found.
    #[test]
    fn break_inside_if_block() {
        let source = "for x in y:\n    if True:\n        break";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
    }

    /// Break inside try/except within a loop is found.
    #[test]
    fn break_inside_try_block() {
        let source = "for x in y:\n    try:\n        break\n    except:\n        continue";
        let mutations = find(source);
        assert_eq!(mutations.len(), 2_usize);
    }

    /// Byte offset and length of break swap mutation are correct.
    #[test]
    fn swap_byte_offset() {
        let source = "for x in y:\n    break";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find("break").expect("has break");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, "break".len());
    }
}
