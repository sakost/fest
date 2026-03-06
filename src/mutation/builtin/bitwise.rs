//! Bitwise operator mutation.
//!
//! Swaps bitwise operators: `&` <-> `|`, `^` <-> `&`,
//! `<<` <-> `>>`.

use ruff_python_ast::{Expr, Operator, Stmt};
use ruff_text_size::Ranged;

use crate::mutation::mutator::{Mutation, MutationContext, Mutator};

/// Mutator that swaps bitwise operators.
#[derive(Debug)]
pub struct BitwiseOp;

/// Return the replacement operator for a bitwise swap, if any.
const fn swapped_bitwise(op: Operator) -> Option<Operator> {
    match op {
        Operator::BitAnd => Some(Operator::BitOr),
        Operator::BitOr | Operator::BitXor => Some(Operator::BitAnd),
        Operator::LShift => Some(Operator::RShift),
        Operator::RShift => Some(Operator::LShift),
        Operator::Add
        | Operator::Sub
        | Operator::Mult
        | Operator::Div
        | Operator::FloorDiv
        | Operator::Mod
        | Operator::Pow
        | Operator::MatMult => None,
    }
}

/// Try to produce a mutation for a single bitwise `BinOp` node.
#[allow(
    clippy::indexing_slicing,
    reason = "byte offsets from AST are always valid"
)]
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn bin_op_mutation(bin_op: &ruff_python_ast::ExprBinOp, source: &str) -> Option<Mutation> {
    let replacement_op = swapped_bitwise(bin_op.op)?;
    let left_end = bin_op.left.range().end().to_usize();
    let right_start = bin_op.right.range().start().to_usize();
    let op_region = &source[left_end..right_start];
    let original_str = bin_op.op.as_str();
    let op_offset_in_region = op_region.find(original_str)?;
    let byte_offset = left_end + op_offset_in_region;
    Some(Mutation {
        byte_offset,
        byte_length: original_str.len(),
        original_text: original_str.to_owned(),
        replacement_text: replacement_op.as_str().to_owned(),
    })
}

/// Recursively collect mutations from an expression tree.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr requires pattern_type_mismatch suppression"
)]
fn collect_expr(expr: &Expr, source: &str, out: &mut Vec<Mutation>) {
    match expr {
        Expr::BinOp(bin_op) => {
            if let Some(mutation) = bin_op_mutation(bin_op, source) {
                out.push(mutation);
            }
            collect_expr(&bin_op.left, source, out);
            collect_expr(&bin_op.right, source, out);
        }
        Expr::BoolOp(e) => {
            for val in &e.values {
                collect_expr(val, source, out);
            }
        }
        Expr::UnaryOp(e) => collect_expr(&e.operand, source, out),
        Expr::Call(e) => {
            collect_expr(&e.func, source, out);
            for arg in &e.arguments.args {
                collect_expr(arg, source, out);
            }
        }
        Expr::If(e) => {
            collect_expr(&e.test, source, out);
            collect_expr(&e.body, source, out);
            collect_expr(&e.orelse, source, out);
        }
        Expr::Named(_)
        | Expr::Lambda(_)
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
        | Expr::IpyEscapeCommand(_) => {}
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
        Stmt::Expr(s) => collect_expr(&s.value, source, out),
        Stmt::Assign(s) => collect_expr(&s.value, source, out),
        Stmt::AugAssign(s) => collect_expr(&s.value, source, out),
        Stmt::Return(s) => {
            if let Some(val) = &s.value {
                collect_expr(val, source, out);
            }
        }
        Stmt::FunctionDef(s) => walk_stmts(&s.body, source, out),
        Stmt::ClassDef(s) => walk_stmts(&s.body, source, out),
        Stmt::If(s) => {
            collect_expr(&s.test, source, out);
            walk_stmts(&s.body, source, out);
            for clause in &s.elif_else_clauses {
                walk_stmts(&clause.body, source, out);
            }
        }
        Stmt::While(s) => {
            collect_expr(&s.test, source, out);
            walk_stmts(&s.body, source, out);
        }
        Stmt::For(s) => walk_stmts(&s.body, source, out),
        Stmt::Try(s) => {
            walk_stmts(&s.body, source, out);
            for handler in &s.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                walk_stmts(&h.body, source, out);
            }
        }
        Stmt::AnnAssign(_)
        | Stmt::Delete(_)
        | Stmt::TypeAlias(_)
        | Stmt::Import(_)
        | Stmt::ImportFrom(_)
        | Stmt::Global(_)
        | Stmt::Nonlocal(_)
        | Stmt::Raise(_)
        | Stmt::With(_)
        | Stmt::Match(_)
        | Stmt::Assert(_)
        | Stmt::Pass(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::IpyEscapeCommand(_) => {}
    }
}

impl Mutator for BitwiseOp {
    #[inline]
    fn name(&self) -> &'static str {
        "bitwise_op"
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
        BitwiseOp.find_mutations(source, &ast, &ctx)
    }

    /// `&` is swapped to `|`.
    #[test]
    fn swap_bitand_to_bitor() {
        let source = "x = a & b";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "&");
        assert_eq!(mutations[0_usize].replacement_text, "|");
    }

    /// `|` is swapped to `&`.
    #[test]
    fn swap_bitor_to_bitand() {
        let source = "x = a | b";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "|");
        assert_eq!(mutations[0_usize].replacement_text, "&");
    }

    /// `<<` is swapped to `>>`.
    #[test]
    fn swap_lshift_to_rshift() {
        let source = "x = a << b";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "<<");
        assert_eq!(mutations[0_usize].replacement_text, ">>");
    }

    /// Arithmetic operators are not mutated by this mutator.
    #[test]
    fn no_mutation_for_arithmetic() {
        let source = "x = a + b";
        let mutations = find(source);
        assert!(mutations.is_empty());
    }

    /// Byte offset of bitwise operator is correct.
    #[test]
    fn byte_offset_is_correct() {
        let source = "x = a & b";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find('&').expect("has &");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, 1_usize);
    }

    /// Byte offset for multi-char bitwise operator.
    #[test]
    fn byte_offset_lshift() {
        let source = "x = a << b";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find("<<").expect("has <<");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, 2_usize);
    }
}
