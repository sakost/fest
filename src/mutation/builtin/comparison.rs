//! Comparison operator mutation.
//!
//! Swaps comparison operators: `==` <-> `!=`, `<` <-> `>=`, `>` <-> `<=`,
//! `is` <-> `is not`, `in` <-> `not in`.

use ruff_python_ast::{CmpOp, Expr, Stmt};
use ruff_text_size::Ranged;

use crate::mutation::mutator::{Mutation, MutationContext, Mutator};

/// Mutator that swaps comparison operators.
#[derive(Debug)]
pub struct ComparisonOp;

/// Return the swapped comparison operator.
const fn swapped_cmp(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Eq => CmpOp::NotEq,
        CmpOp::NotEq => CmpOp::Eq,
        CmpOp::Lt => CmpOp::GtE,
        CmpOp::GtE => CmpOp::Lt,
        CmpOp::Gt => CmpOp::LtE,
        CmpOp::LtE => CmpOp::Gt,
        CmpOp::Is => CmpOp::IsNot,
        CmpOp::IsNot => CmpOp::Is,
        CmpOp::In => CmpOp::NotIn,
        CmpOp::NotIn => CmpOp::In,
    }
}

/// Find the byte offset and length of an operator in source between two positions.
#[allow(
    clippy::indexing_slicing,
    reason = "byte offsets from AST are always valid"
)]
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn find_op_in_source(
    source: &str,
    left_end: usize,
    right_start: usize,
    op_str: &str,
) -> Option<(usize, usize)> {
    let region = &source[left_end..right_start];
    region
        .find(op_str)
        .map(|offset| (left_end + offset, op_str.len()))
}

/// Collect comparison mutations from a single compare expression.
fn collect_compare(compare: &ruff_python_ast::ExprCompare, source: &str, out: &mut Vec<Mutation>) {
    let mut left_end = compare.left.range().end().to_usize();
    for (op, comparator) in compare.ops.iter().zip(compare.comparators.iter()) {
        let right_start = comparator.range().start().to_usize();
        let original_str = op.as_str();
        let replacement = swapped_cmp(*op);
        if let Some((byte_offset, byte_length)) =
            find_op_in_source(source, left_end, right_start, original_str)
        {
            out.push(Mutation {
                byte_offset,
                byte_length,
                original_text: original_str.to_owned(),
                replacement_text: replacement.as_str().to_owned(),
            });
        }
        left_end = comparator.range().end().to_usize();
    }
}

/// Recursively collect mutations from an expression.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr requires pattern_type_mismatch suppression"
)]
fn collect_expr(expr: &Expr, source: &str, out: &mut Vec<Mutation>) {
    match expr {
        Expr::Compare(compare) => {
            collect_compare(compare, source, out);
            collect_expr(&compare.left, source, out);
            visit_exprs(&compare.comparators, source, out);
        }
        Expr::BinOp(e) => {
            collect_expr(&e.left, source, out);
            collect_expr(&e.right, source, out);
        }
        Expr::BoolOp(e) => visit_exprs(&e.values, source, out),
        Expr::UnaryOp(e) => collect_expr(&e.operand, source, out),
        Expr::Call(e) => {
            collect_expr(&e.func, source, out);
            visit_exprs(&e.arguments.args, source, out);
        }
        Expr::If(e) => {
            collect_expr(&e.test, source, out);
            collect_expr(&e.body, source, out);
            collect_expr(&e.orelse, source, out);
        }
        Expr::List(e) => visit_exprs(&e.elts, source, out),
        Expr::Tuple(e) => visit_exprs(&e.elts, source, out),
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
        | Expr::Slice(_)
        | Expr::IpyEscapeCommand(_) => {}
    }
}

/// Visit a slice of expressions.
fn visit_exprs(exprs: &[Expr], source: &str, out: &mut Vec<Mutation>) {
    for expr in exprs {
        collect_expr(expr, source, out);
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
        Stmt::Return(s) => {
            if let Some(val) = &s.value {
                collect_expr(val, source, out);
            }
        }
        Stmt::FunctionDef(s) => walk_stmts(&s.body, source, out),
        Stmt::ClassDef(s) => walk_stmts(&s.body, source, out),
        Stmt::If(s) => walk_if(s, source, out),
        Stmt::While(s) => {
            collect_expr(&s.test, source, out);
            walk_stmts(&s.body, source, out);
        }
        Stmt::For(s) => {
            walk_stmts(&s.body, source, out);
            walk_stmts(&s.orelse, source, out);
        }
        Stmt::Try(s) => walk_try(s, source, out),
        Stmt::Assert(s) => collect_expr(&s.test, source, out),
        Stmt::AugAssign(_)
        | Stmt::AnnAssign(_)
        | Stmt::Delete(_)
        | Stmt::TypeAlias(_)
        | Stmt::Import(_)
        | Stmt::ImportFrom(_)
        | Stmt::Global(_)
        | Stmt::Nonlocal(_)
        | Stmt::Raise(_)
        | Stmt::With(_)
        | Stmt::Match(_)
        | Stmt::Pass(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::IpyEscapeCommand(_) => {}
    }
}

/// Walk an `if` statement.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on Option<Expr> requires suppression"
)]
fn walk_if(if_stmt: &ruff_python_ast::StmtIf, source: &str, out: &mut Vec<Mutation>) {
    collect_expr(&if_stmt.test, source, out);
    walk_stmts(&if_stmt.body, source, out);
    for clause in &if_stmt.elif_else_clauses {
        if let Some(test) = &clause.test {
            collect_expr(test, source, out);
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

impl Mutator for ComparisonOp {
    #[inline]
    fn name(&self) -> &'static str {
        "comparison_op"
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
        ComparisonOp.find_mutations(source, &ast, &ctx)
    }

    /// `==` is swapped to `!=`.
    #[test]
    fn swap_eq_to_ne() {
        let mutations = find("x = a == b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "==");
        assert_eq!(mutations[0_usize].replacement_text, "!=");
    }

    /// `!=` is swapped to `==`.
    #[test]
    fn swap_ne_to_eq() {
        let mutations = find("x = a != b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "!=");
        assert_eq!(mutations[0_usize].replacement_text, "==");
    }

    /// `<` is swapped to `>=`.
    #[test]
    fn swap_lt_to_gte() {
        let mutations = find("x = a < b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "<");
        assert_eq!(mutations[0_usize].replacement_text, ">=");
    }

    /// `>` is swapped to `<=`.
    #[test]
    fn swap_gt_to_lte() {
        let mutations = find("x = a > b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, ">");
        assert_eq!(mutations[0_usize].replacement_text, "<=");
    }

    /// `is` is swapped to `is not`.
    #[test]
    fn swap_is_to_is_not() {
        let mutations = find("x = a is b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "is");
        assert_eq!(mutations[0_usize].replacement_text, "is not");
    }

    /// `is not` is swapped to `is`.
    #[test]
    fn swap_is_not_to_is() {
        let mutations = find("x = a is not b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "is not");
        assert_eq!(mutations[0_usize].replacement_text, "is");
    }

    /// `in` is swapped to `not in`.
    #[test]
    fn swap_in_to_not_in() {
        let mutations = find("x = a in b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "in");
        assert_eq!(mutations[0_usize].replacement_text, "not in");
    }

    /// `not in` is swapped to `in`.
    #[test]
    fn swap_not_in_to_in() {
        let mutations = find("x = a not in b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "not in");
        assert_eq!(mutations[0_usize].replacement_text, "in");
    }

    /// Chained comparisons produce multiple mutations.
    #[test]
    fn chained_comparison() {
        let mutations = find("x = a < b < c");
        assert_eq!(mutations.len(), 2_usize);
    }

    /// Mutations are found inside if blocks.
    #[test]
    fn inside_if_block() {
        let source = "if True:\n    x = a == b";
        let mutations = find(source);
        assert!(!mutations.is_empty());
    }

    /// Mutations are found inside try/except blocks.
    #[test]
    fn inside_try_block() {
        let source = "try:\n    x = a == b\nexcept:\n    y = c != d";
        let mutations = find(source);
        assert_eq!(mutations.len(), 2_usize);
    }

    /// Mutations are found inside expressions visited via visit_exprs (e.g. bool_op values).
    #[test]
    fn inside_bool_op_values() {
        let source = "x = (a == b) or (c != d)";
        let mutations = find(source);
        assert_eq!(mutations.len(), 2_usize);
    }

    /// Byte offset of `==` operator is correct.
    #[test]
    fn byte_offset_is_correct() {
        let source = "x = a == b";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find("==").expect("has ==");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, 2_usize);
    }

    /// find_op_in_source returns correct offset for operator not at start of region.
    #[test]
    fn find_op_in_source_offset() {
        let source = "x = a  !=  b";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find("!=").expect("has !=");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, 2_usize);
    }
}
