//! Augmented assignment operator mutation.
//!
//! Swaps augmented assignment operators: `+=` <-> `-=`, `*=` <-> `/=`, etc.

use ruff_python_ast::{Operator, Stmt};
use ruff_text_size::Ranged;

use crate::mutation::mutator::{Mutation, MutationContext, Mutator};

/// Mutator that swaps augmented assignment operators.
#[derive(Debug)]
pub struct AugmentedAssign;

/// Return the replacement operator for an augmented assignment swap, if any.
const fn swapped_aug_op(op: Operator) -> Option<Operator> {
    match op {
        Operator::Add => Some(Operator::Sub),
        Operator::Sub => Some(Operator::Add),
        Operator::Mult => Some(Operator::Div),
        Operator::Div | Operator::FloorDiv | Operator::Mod | Operator::Pow => Some(Operator::Mult),
        Operator::BitAnd => Some(Operator::BitOr),
        Operator::BitOr | Operator::BitXor => Some(Operator::BitAnd),
        Operator::LShift => Some(Operator::RShift),
        Operator::RShift => Some(Operator::LShift),
        Operator::MatMult => None,
    }
}

/// Format an augmented assignment operator string.
fn aug_op_str(op: Operator) -> String {
    let mut result = String::from(op.as_str());
    result.push('=');
    result
}

/// Create a mutation for an augmented assignment.
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn aug_assign_mutation(aug: &ruff_python_ast::StmtAugAssign, source: &str) -> Option<Mutation> {
    let replacement_op = swapped_aug_op(aug.op)?;
    let target_end = aug.target.range().end().to_usize();
    let value_start = aug.value.range().start().to_usize();
    let region = &source[target_end..value_start];
    let original_str = aug_op_str(aug.op);
    let op_offset = region.find(&original_str)?;
    let byte_offset = target_end + op_offset;
    Some(Mutation {
        byte_offset,
        byte_length: original_str.len(),
        original_text: original_str,
        replacement_text: aug_op_str(replacement_op),
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
        Stmt::AugAssign(s) => {
            if let Some(mutation) = aug_assign_mutation(s, source) {
                out.push(mutation);
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
        Stmt::Expr(_)
        | Stmt::Assign(_)
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

impl Mutator for AugmentedAssign {
    #[inline]
    fn name(&self) -> &'static str {
        "augmented_assign"
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
        AugmentedAssign.find_mutations(source, &ast, &ctx)
    }

    /// `+=` is swapped to `-=`.
    #[test]
    fn swap_add_assign() {
        let source = "x += 1";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "+=");
        assert_eq!(mutations[0_usize].replacement_text, "-=");
    }

    /// `-=` is swapped to `+=`.
    #[test]
    fn swap_sub_assign() {
        let source = "x -= 1";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "-=");
        assert_eq!(mutations[0_usize].replacement_text, "+=");
    }

    /// Byte offset of augmented assignment operator is correct.
    #[test]
    fn byte_offset_is_correct() {
        let source = "x += 1";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find("+=").expect("has +=");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, 2_usize);
    }

    /// Byte offset with wider spacing.
    #[test]
    fn byte_offset_wider_spacing() {
        let source = "x  -=  1";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find("-=").expect("has -=");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, 2_usize);
    }
}
