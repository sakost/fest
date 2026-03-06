//! Unary operator mutation.
//!
//! Swaps and removes unary operators: `-x` → `+x`/`~x`/`x`,
//! `+x` → `-x`/`~x`, `~x` → `-x`/`+x`/`x`. Skips `not` (handled
//! by `boolean_op`).

use ruff_python_ast::{Expr, Stmt, UnaryOp as UnaryOpKind};
use ruff_text_size::Ranged;

use crate::mutation::mutator::{Mutation, MutationContext, Mutator};

/// Mutator that swaps and removes unary operators.
#[derive(Debug)]
pub struct UnaryOp;

/// Emit swap and removal mutations for a unary operator.
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn collect_unary_mutations(
    unary_op: &ruff_python_ast::ExprUnaryOp,
    source: &str,
    out: &mut Vec<Mutation>,
) {
    let prefix_start = unary_op.range().start().to_usize();
    let operand_start = unary_op.operand.range().start().to_usize();
    let prefix = &source[prefix_start..operand_start];

    let (tok, swap_targets, can_remove) = match unary_op.op {
        UnaryOpKind::USub => ("-", &["+", "~"][..], true),
        UnaryOpKind::UAdd => ("+", &["-", "~"][..], false),
        UnaryOpKind::Invert => ("~", &["-", "+"][..], true),
        UnaryOpKind::Not => return,
    };

    if let Some(op_rel) = prefix.find(tok) {
        let op_abs = prefix_start + op_rel;
        // Swap mutations: replace the operator token with each alternative.
        for replacement in swap_targets {
            out.push(Mutation {
                byte_offset: op_abs,
                byte_length: tok.len(),
                original_text: tok.to_owned(),
                replacement_text: (*replacement).to_owned(),
            });
        }
        // Removal mutation: remove the entire prefix (operator + any whitespace).
        if can_remove {
            out.push(Mutation {
                byte_offset: prefix_start,
                byte_length: operand_start - prefix_start,
                original_text: prefix.to_owned(),
                replacement_text: String::new(),
            });
        }
    }
}

/// Recursively collect mutations from an expression.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr requires pattern_type_mismatch suppression"
)]
fn collect_expr(expr: &Expr, source: &str, out: &mut Vec<Mutation>) {
    match expr {
        Expr::UnaryOp(unary) => {
            collect_unary_mutations(unary, source, out);
            collect_expr(&unary.operand, source, out);
        }
        Expr::BinOp(bin) => {
            collect_expr(&bin.left, source, out);
            collect_expr(&bin.right, source, out);
        }
        Expr::BoolOp(bool_op) => visit_exprs(&bool_op.values, source, out),
        Expr::Compare(cmp) => {
            collect_expr(&cmp.left, source, out);
            visit_exprs(&cmp.comparators, source, out);
        }
        Expr::Call(call) => {
            collect_expr(&call.func, source, out);
            visit_exprs(&call.arguments.args, source, out);
        }
        Expr::If(if_expr) => {
            collect_expr(&if_expr.test, source, out);
            collect_expr(&if_expr.body, source, out);
            collect_expr(&if_expr.orelse, source, out);
        }
        Expr::Named(named) => collect_expr(&named.value, source, out),
        Expr::Lambda(lam) => collect_expr(&lam.body, source, out),
        Expr::Starred(star) => collect_expr(&star.value, source, out),
        Expr::List(list) => visit_exprs(&list.elts, source, out),
        Expr::Tuple(tup) => visit_exprs(&tup.elts, source, out),
        Expr::Subscript(sub) => {
            collect_expr(&sub.value, source, out);
            collect_expr(&sub.slice, source, out);
        }
        Expr::Attribute(attr) => collect_expr(&attr.value, source, out),
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
        | Stmt::Match(_)
        | Stmt::Pass(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
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

impl Mutator for UnaryOp {
    #[inline]
    fn name(&self) -> &'static str {
        "unary_op"
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
        UnaryOp.find_mutations(source, &ast, &ctx)
    }

    /// `-x` produces 3 mutations: swap to `+`, swap to `~`, and removal.
    #[test]
    fn usub_swap_and_remove() {
        let mutations = find("x = -y");
        assert_eq!(mutations.len(), 3_usize);
        assert_eq!(mutations[0_usize].replacement_text, "+");
        assert_eq!(mutations[1_usize].replacement_text, "~");
        assert_eq!(mutations[2_usize].replacement_text, "");
    }

    /// `+x` produces 2 swap mutations (no removal since `+x` == `x`).
    #[test]
    fn uadd_swap_only() {
        let mutations = find("x = +y");
        assert_eq!(mutations.len(), 2_usize);
        assert_eq!(mutations[0_usize].replacement_text, "-");
        assert_eq!(mutations[1_usize].replacement_text, "~");
    }

    /// `~x` produces 3 mutations: swap to `-`, swap to `+`, and removal.
    #[test]
    fn invert_swap_and_remove() {
        let mutations = find("x = ~y");
        assert_eq!(mutations.len(), 3_usize);
        assert_eq!(mutations[0_usize].replacement_text, "-");
        assert_eq!(mutations[1_usize].replacement_text, "+");
        assert_eq!(mutations[2_usize].replacement_text, "");
    }

    /// `not` is not handled by this mutator (handled by boolean_op).
    #[test]
    fn not_is_skipped() {
        let mutations = find("x = not y");
        assert!(mutations.is_empty());
    }

    /// Nested unary operators produce mutations for each level.
    #[test]
    fn nested_unary() {
        let mutations = find("x = -(-y)");
        // Outer `-`: 3 mutations (swap +, swap ~, remove)
        // Inner `-`: 3 mutations (swap +, swap ~, remove)
        assert_eq!(mutations.len(), 6_usize);
    }

    /// Mutations are found inside if blocks.
    #[test]
    fn inside_if_block() {
        let source = "if True:\n    x = -y";
        let mutations = find(source);
        assert_eq!(mutations.len(), 3_usize);
    }

    /// Mutations are found inside try/except blocks.
    #[test]
    fn inside_try_block() {
        let source = "try:\n    x = -y\nexcept:\n    z = +w";
        let mutations = find(source);
        // -y: 3 mutations (swap +, swap ~, remove), +w: 2 mutations (swap -, swap ~)
        assert_eq!(mutations.len(), 5_usize);
    }

    /// Mutations are found inside expressions visited via visit_exprs.
    #[test]
    fn inside_bool_op_values() {
        let source = "x = (-a) or (-b)";
        let mutations = find(source);
        assert_eq!(mutations.len(), 6_usize);
    }

    /// Byte offset of unary operator swap mutation is correct.
    #[test]
    fn swap_byte_offset() {
        let source = "x = -y";
        let mutations = find(source);
        // "-" is at byte offset 4
        let expected_offset = source.find('-').expect("has -");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, 1_usize);
    }

    /// Byte offset of removal mutation covers the entire prefix.
    #[test]
    fn removal_byte_offset() {
        let source = "x = -y";
        let mutations = find(source);
        // The removal mutation is the last one (index 2)
        let removal = &mutations[2_usize];
        let expected_offset = source.find('-').expect("has -");
        assert_eq!(removal.byte_offset, expected_offset);
        // prefix is "-" (1 byte, no extra whitespace)
        assert_eq!(removal.byte_length, 1_usize);
    }
}
