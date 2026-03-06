//! Zero-iteration loop mutation.
//!
//! For `for` loops: replaces the iterable expression with `[]` so the loop
//! body never executes. For `while` loops: replaces the condition with
//! `False`.

use ruff_python_ast::Stmt;
use ruff_text_size::Ranged;

use crate::mutation::mutator::{Mutation, MutationContext, Mutator};

/// Mutator that forces loops to execute zero iterations.
#[derive(Debug)]
pub struct ZeroIterationLoop;

/// Create a mutation replacing a `for` loop's iter expression with `[]`.
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn for_iter_mutation(for_stmt: &ruff_python_ast::StmtFor, source: &str) -> Mutation {
    let start = for_stmt.iter.range().start().to_usize();
    let end = for_stmt.iter.range().end().to_usize();
    let original = &source[start..end];
    Mutation {
        byte_offset: start,
        byte_length: end - start,
        original_text: original.to_owned(),
        replacement_text: "[]".to_owned(),
    }
}

/// Create a mutation replacing a `while` loop's condition with `False`.
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn while_condition_mutation(while_stmt: &ruff_python_ast::StmtWhile, source: &str) -> Mutation {
    let start = while_stmt.test.range().start().to_usize();
    let end = while_stmt.test.range().end().to_usize();
    let original = &source[start..end];
    Mutation {
        byte_offset: start,
        byte_length: end - start,
        original_text: original.to_owned(),
        replacement_text: "False".to_owned(),
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
        Stmt::For(s) => {
            out.push(for_iter_mutation(s, source));
            walk_stmts(&s.body, source, out);
            walk_stmts(&s.orelse, source, out);
        }
        Stmt::While(s) => {
            out.push(while_condition_mutation(s, source));
            walk_stmts(&s.body, source, out);
            walk_stmts(&s.orelse, source, out);
        }
        Stmt::FunctionDef(s) => walk_stmts(&s.body, source, out),
        Stmt::ClassDef(s) => walk_stmts(&s.body, source, out),
        Stmt::If(s) => {
            walk_stmts(&s.body, source, out);
            for clause in &s.elif_else_clauses {
                walk_stmts(&clause.body, source, out);
            }
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

impl Mutator for ZeroIterationLoop {
    #[inline]
    fn name(&self) -> &'static str {
        "zero_iteration_loop"
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
        ZeroIterationLoop.find_mutations(source, &ast, &ctx)
    }

    /// `for` loop iter is replaced with `[]`.
    #[test]
    fn for_loop_iter_replaced() {
        let source = "for x in items:\n    process(x)";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "items");
        assert_eq!(mutations[0_usize].replacement_text, "[]");
    }

    /// `while` loop condition is replaced with `False`.
    #[test]
    fn while_loop_condition_replaced() {
        let source = "while True:\n    do_work()";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "True");
        assert_eq!(mutations[0_usize].replacement_text, "False");
    }

    /// Nested loops produce one mutation per loop.
    #[test]
    fn nested_loops() {
        let source = "for x in items:\n    for y in other:\n        pass";
        let mutations = find(source);
        assert_eq!(mutations.len(), 2_usize);
        assert_eq!(mutations[0_usize].replacement_text, "[]");
        assert_eq!(mutations[1_usize].replacement_text, "[]");
    }

    /// Byte offset of for loop iter mutation is correct.
    #[test]
    fn for_iter_byte_offset() {
        let source = "for x in items:\n    process(x)";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find("items").expect("has items");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, "items".len());
    }

    /// Byte offset of while loop condition mutation is correct.
    #[test]
    fn while_condition_byte_offset() {
        let source = "while True:\n    do_work()";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find("True").expect("has True");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, "True".len());
    }
}
