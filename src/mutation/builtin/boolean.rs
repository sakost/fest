//! Boolean operator mutation.
//!
//! Swaps boolean operators (`and` <-> `or`) and removes `not` prefix
//! from unary `not` expressions.

use ruff_python_ast::{BoolOp, Expr, Stmt, UnaryOp};
use ruff_text_size::Ranged;

use crate::mutation::mutator::{Mutation, MutationContext, Mutator};

/// Mutator that swaps boolean operators and removes `not`.
#[derive(Debug)]
pub struct BooleanOp;

/// Collect boolean-op swaps from a `BoolOp` expression.
///
/// For `and`/`or`, finds each operator token between consecutive values and swaps it.
#[allow(
    clippy::indexing_slicing,
    reason = "windows(2) guarantees exactly 2 elements per window"
)]
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn collect_bool_op_swaps(
    bool_op: &ruff_python_ast::ExprBoolOp,
    source: &str,
    out: &mut Vec<Mutation>,
) {
    let original_str = bool_op.op.as_str();
    let replacement_str = match bool_op.op {
        BoolOp::And => "or",
        BoolOp::Or => "and",
    };
    let values = &bool_op.values;
    let count = values.len();
    assert!(count >= 2_usize, "BoolOp must have at least 2 values");
    for window in values.windows(2_usize) {
        assert!(window.len() > 1_usize, "windows(2) guarantees length 2");
        let left_end = window[0_usize].range().end().to_usize();
        let right_start = window[1_usize].range().start().to_usize();
        #[allow(clippy::string_slice, reason = "byte offsets from AST are valid")]
        let region = &source[left_end..right_start];
        if let Some(op_offset) = region.find(original_str) {
            let byte_offset = left_end + op_offset;
            out.push(Mutation {
                byte_offset,
                byte_length: original_str.len(),
                original_text: original_str.to_owned(),
                replacement_text: replacement_str.to_owned(),
            });
        }
    }
}

/// Collect `not` removal mutations.
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn collect_not_removal(
    unary_op: &ruff_python_ast::ExprUnaryOp,
    source: &str,
    out: &mut Vec<Mutation>,
) {
    if unary_op.op != UnaryOp::Not {
        return;
    }
    let start = unary_op.range().start().to_usize();
    let operand_start = unary_op.operand.range().start().to_usize();
    let prefix = &source[start..operand_start];
    out.push(Mutation {
        byte_offset: start,
        byte_length: prefix.len(),
        original_text: prefix.to_owned(),
        replacement_text: String::new(),
    });
}

/// Recursively collect mutations from an expression.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr requires pattern_type_mismatch suppression"
)]
fn collect_expr(expr: &Expr, source: &str, out: &mut Vec<Mutation>) {
    match expr {
        Expr::BoolOp(bool_op) => {
            collect_bool_op_swaps(bool_op, source, out);
            visit_exprs(&bool_op.values, source, out);
        }
        Expr::UnaryOp(unary_op) => {
            collect_not_removal(unary_op, source, out);
            collect_expr(&unary_op.operand, source, out);
        }
        Expr::BinOp(e) => {
            collect_expr(&e.left, source, out);
            collect_expr(&e.right, source, out);
        }
        Expr::Compare(e) => {
            collect_expr(&e.left, source, out);
            visit_exprs(&e.comparators, source, out);
        }
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
        Stmt::For(s) => walk_stmts(&s.body, source, out),
        Stmt::Try(s) => walk_try(s, source, out),
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
        | Stmt::Assert(_)
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
}

impl Mutator for BooleanOp {
    #[inline]
    fn name(&self) -> &'static str {
        "boolean_op"
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
        BooleanOp.find_mutations(source, &ast, &ctx)
    }

    /// `and` is swapped to `or`.
    #[test]
    fn swap_and_to_or() {
        let mutations = find("x = a and b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "and");
        assert_eq!(mutations[0_usize].replacement_text, "or");
    }

    /// `or` is swapped to `and`.
    #[test]
    fn swap_or_to_and() {
        let mutations = find("x = a or b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "or");
        assert_eq!(mutations[0_usize].replacement_text, "and");
    }

    /// `not x` removes the `not` prefix.
    #[test]
    fn remove_not_prefix() {
        let mutations = find("x = not y");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "not ");
        assert_eq!(mutations[0_usize].replacement_text, "");
    }

    /// Multiple `and` operators in a chained expression.
    #[test]
    fn chained_and() {
        let mutations = find("x = a and b and c");
        assert_eq!(mutations.len(), 2_usize);
    }

    /// Mixed `and` + `not` produces two mutations.
    #[test]
    fn mixed_and_not() {
        let mutations = find("x = not a and b");
        assert_eq!(mutations.len(), 2_usize);
    }

    /// Mutations are found inside if blocks.
    #[test]
    fn inside_if_block() {
        let source = "if True:\n    x = a and b";
        let mutations = find(source);
        assert!(!mutations.is_empty());
    }

    /// Mutations are found inside try/except blocks.
    #[test]
    fn inside_try_block() {
        let source = "try:\n    x = a and b\nexcept:\n    y = c or d";
        let mutations = find(source);
        assert_eq!(mutations.len(), 2_usize);
    }

    /// Byte offset of `and` operator is correct.
    #[test]
    fn byte_offset_and_length() {
        let source = "x = a and b";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find("and").expect("has and");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, 3_usize);
    }

    /// Byte offset of `or` operator is correct.
    #[test]
    fn byte_offset_or() {
        let source = "x = a or b";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find(" or ").expect("has or") + 1_usize;
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, 2_usize);
    }
}
