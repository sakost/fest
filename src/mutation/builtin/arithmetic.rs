//! Arithmetic operator mutation.
//!
//! Swaps binary arithmetic operators: `+` <-> `-`, `*` <-> `/`,
//! `//` <-> `*`, `%` <-> `*`, `**` <-> `*`.

use ruff_python_ast::{Expr, Operator, Stmt};
use ruff_text_size::Ranged;

use crate::mutation::mutator::{Mutation, Mutator};

/// Mutator that swaps binary arithmetic operators.
#[derive(Debug)]
pub struct ArithmeticOp;

/// Return the replacement operator for an arithmetic swap, if any.
const fn swapped_operator(op: Operator) -> Option<Operator> {
    match op {
        Operator::Add => Some(Operator::Sub),
        Operator::Sub => Some(Operator::Add),
        Operator::Mult => Some(Operator::Div),
        Operator::Div | Operator::FloorDiv | Operator::Mod | Operator::Pow => Some(Operator::Mult),
        Operator::MatMult
        | Operator::LShift
        | Operator::RShift
        | Operator::BitOr
        | Operator::BitXor
        | Operator::BitAnd => None,
    }
}

/// Try to produce a mutation for a single `BinOp` node.
#[allow(
    clippy::indexing_slicing,
    reason = "byte offsets from AST are always valid"
)]
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn bin_op_mutation(bin_op: &ruff_python_ast::ExprBinOp, source: &str) -> Option<Mutation> {
    let replacement_op = swapped_operator(bin_op.op)?;
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
        Expr::BoolOp(e) => visit_exprs(&e.values, source, out),
        Expr::UnaryOp(e) => collect_expr(&e.operand, source, out),
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
        Expr::Named(e) => collect_expr(&e.value, source, out),
        Expr::Lambda(e) => collect_expr(&e.body, source, out),
        Expr::Starred(e) => collect_expr(&e.value, source, out),
        Expr::List(e) => visit_exprs(&e.elts, source, out),
        Expr::Tuple(e) => visit_exprs(&e.elts, source, out),
        Expr::Subscript(e) => {
            collect_expr(&e.value, source, out);
            collect_expr(&e.slice, source, out);
        }
        Expr::Attribute(e) => collect_expr(&e.value, source, out),
        Expr::Dict(_)
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

/// Walk all statements and collect arithmetic mutations.
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
        Stmt::AnnAssign(s) => {
            if let Some(val) = &s.value {
                collect_expr(val, source, out);
            }
        }
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
            walk_stmts(&s.orelse, source, out);
        }
        Stmt::For(s) => {
            collect_expr(&s.iter, source, out);
            walk_stmts(&s.body, source, out);
            walk_stmts(&s.orelse, source, out);
        }
        Stmt::Try(s) => walk_try(s, source, out),
        Stmt::With(s) => walk_stmts(&s.body, source, out),
        Stmt::Assert(s) => {
            collect_expr(&s.test, source, out);
            if let Some(msg) = &s.msg {
                collect_expr(msg, source, out);
            }
        }
        Stmt::Delete(_)
        | Stmt::TypeAlias(_)
        | Stmt::Import(_)
        | Stmt::ImportFrom(_)
        | Stmt::Global(_)
        | Stmt::Nonlocal(_)
        | Stmt::Raise(_)
        | Stmt::Pass(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::Match(_)
        | Stmt::IpyEscapeCommand(_) => {}
    }
}

/// Walk an `if` statement including elif/else clauses.
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

impl Mutator for ArithmeticOp {
    #[inline]
    fn name(&self) -> &'static str {
        "arithmetic_op"
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
        ArithmeticOp.find_mutations(source, &ast)
    }

    /// Addition is swapped to subtraction.
    #[test]
    fn swap_add_to_sub() {
        let mutations = find("x = a + b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "+");
        assert_eq!(mutations[0_usize].replacement_text, "-");
    }

    /// Subtraction is swapped to addition.
    #[test]
    fn swap_sub_to_add() {
        let mutations = find("x = a - b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "-");
        assert_eq!(mutations[0_usize].replacement_text, "+");
    }

    /// Multiplication is swapped to division.
    #[test]
    fn swap_mult_to_div() {
        let mutations = find("x = a * b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "*");
        assert_eq!(mutations[0_usize].replacement_text, "/");
    }

    /// Division is swapped to multiplication.
    #[test]
    fn swap_div_to_mult() {
        let mutations = find("x = a / b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "/");
        assert_eq!(mutations[0_usize].replacement_text, "*");
    }

    /// Floor division is swapped to multiplication.
    #[test]
    fn swap_floordiv_to_mult() {
        let mutations = find("x = a // b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "//");
        assert_eq!(mutations[0_usize].replacement_text, "*");
    }

    /// Modulo is swapped to multiplication.
    #[test]
    fn swap_mod_to_mult() {
        let mutations = find("x = a % b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "%");
        assert_eq!(mutations[0_usize].replacement_text, "*");
    }

    /// Power is swapped to multiplication.
    #[test]
    fn swap_pow_to_mult() {
        let mutations = find("x = a ** b");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "**");
        assert_eq!(mutations[0_usize].replacement_text, "*");
    }

    /// Multiple operators in one expression produce multiple mutations.
    #[test]
    fn multiple_operators() {
        let mutations = find("x = a + b * c");
        assert_eq!(mutations.len(), 2_usize);
    }

    /// No mutations for bitwise operators.
    #[test]
    fn no_mutation_for_bitwise() {
        let mutations = find("x = a & b");
        assert!(mutations.is_empty());
    }
}
