//! Variable insertion mutation (requires seed).
//!
//! Wraps `Expr::Name` references on the right-hand side of assignments
//! by inserting a seed-derived arithmetic operation, e.g. `x` becomes
//! `x + 1` or `x * -1`. Without a seed, no mutations are produced.

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::mutation::{
    mutator::{Mutation, MutationContext, Mutator},
    seed::derive_value,
};

/// Mutator that inserts arithmetic perturbations around variable references.
#[derive(Debug)]
pub struct VariableInsert;

/// Name of this mutator (used for seed derivation).
const MUTATOR_NAME: &str = "variable_insert";

/// Operators available for insertion.
const OPERATORS: [&str; 3_usize] = [" + ", " - ", " * "];

/// Operand constants for insertion.
const OPERANDS: [&str; 3_usize] = ["1", "-1", "2"];

/// Derive replacement text: wraps a Name node with `(name OP const)`.
#[allow(
    clippy::indexing_slicing,
    reason = "modulo 3 guarantees index is in bounds for length-3 arrays"
)]
fn inserted_text(
    original: &str,
    seed: u64,
    file_path: &std::path::Path,
    byte_offset: usize,
) -> String {
    let val = derive_value(seed, file_path, byte_offset, MUTATOR_NAME);
    let op = OPERATORS[(val % 3_u64) as usize];
    // Use a second derivation for the operand choice.
    let val2 = derive_value(
        seed.wrapping_add(1_u64),
        file_path,
        byte_offset,
        MUTATOR_NAME,
    );
    let operand = OPERANDS[(val2 % 3_u64) as usize];
    format!("({original}{op}{operand})")
}

/// Collect mutations from an expression (targeting Name nodes).
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr requires pattern_type_mismatch suppression"
)]
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive enum matching on Expr requires many arms"
)]
fn collect_expr(
    expr: &Expr,
    source: &str,
    seed: u64,
    file_path: &std::path::Path,
    out: &mut Vec<Mutation>,
) {
    match expr {
        Expr::Name(name) => {
            let start = name.range().start().to_usize();
            let end = name.range().end().to_usize();
            let original = &source[start..end];
            let replacement = inserted_text(original, seed, file_path, start);
            out.push(Mutation {
                byte_offset: start,
                byte_length: end - start,
                original_text: original.to_owned(),
                replacement_text: replacement,
            });
        }
        Expr::BinOp(bin) => {
            collect_expr(&bin.left, source, seed, file_path, out);
            collect_expr(&bin.right, source, seed, file_path, out);
        }
        Expr::UnaryOp(u) => collect_expr(&u.operand, source, seed, file_path, out),
        Expr::BoolOp(b) => {
            for val in &b.values {
                collect_expr(val, source, seed, file_path, out);
            }
        }
        Expr::Compare(cmp) => {
            collect_expr(&cmp.left, source, seed, file_path, out);
            for comp in &cmp.comparators {
                collect_expr(comp, source, seed, file_path, out);
            }
        }
        Expr::Call(call) => {
            for arg in &call.arguments.args {
                collect_expr(arg, source, seed, file_path, out);
            }
        }
        Expr::If(if_expr) => {
            collect_expr(&if_expr.test, source, seed, file_path, out);
            collect_expr(&if_expr.body, source, seed, file_path, out);
            collect_expr(&if_expr.orelse, source, seed, file_path, out);
        }
        Expr::Subscript(sub) => {
            collect_expr(&sub.value, source, seed, file_path, out);
            collect_expr(&sub.slice, source, seed, file_path, out);
        }
        Expr::Attribute(attr) => collect_expr(&attr.value, source, seed, file_path, out),
        Expr::Starred(star) => collect_expr(&star.value, source, seed, file_path, out),
        Expr::List(list) => {
            for elt in &list.elts {
                collect_expr(elt, source, seed, file_path, out);
            }
        }
        Expr::Tuple(tup) => {
            for elt in &tup.elts {
                collect_expr(elt, source, seed, file_path, out);
            }
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
        | Expr::FString(_)
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_)
        | Expr::Slice(_)
        | Expr::IpyEscapeCommand(_) => {}
    }
}

/// Walk all statements.
fn walk_stmts(
    stmts: &[Stmt],
    source: &str,
    seed: u64,
    file_path: &std::path::Path,
    out: &mut Vec<Mutation>,
) {
    for stmt in stmts {
        walk_stmt(stmt, source, seed, file_path, out);
    }
}

/// Walk a single statement.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt requires pattern_type_mismatch suppression"
)]
fn walk_stmt(
    stmt: &Stmt,
    source: &str,
    seed: u64,
    file_path: &std::path::Path,
    out: &mut Vec<Mutation>,
) {
    match stmt {
        Stmt::Assign(s) => collect_expr(&s.value, source, seed, file_path, out),
        Stmt::AugAssign(s) => collect_expr(&s.value, source, seed, file_path, out),
        Stmt::AnnAssign(s) => {
            if let Some(val) = &s.value {
                collect_expr(val, source, seed, file_path, out);
            }
        }
        Stmt::Return(s) => {
            if let Some(val) = &s.value {
                collect_expr(val, source, seed, file_path, out);
            }
        }
        Stmt::Expr(s) => collect_expr(&s.value, source, seed, file_path, out),
        Stmt::FunctionDef(s) => walk_stmts(&s.body, source, seed, file_path, out),
        Stmt::ClassDef(s) => walk_stmts(&s.body, source, seed, file_path, out),
        Stmt::If(s) => {
            walk_stmts(&s.body, source, seed, file_path, out);
            for clause in &s.elif_else_clauses {
                walk_stmts(&clause.body, source, seed, file_path, out);
            }
        }
        Stmt::While(s) => {
            walk_stmts(&s.body, source, seed, file_path, out);
            walk_stmts(&s.orelse, source, seed, file_path, out);
        }
        Stmt::For(s) => {
            walk_stmts(&s.body, source, seed, file_path, out);
            walk_stmts(&s.orelse, source, seed, file_path, out);
        }
        Stmt::Try(s) => {
            walk_stmts(&s.body, source, seed, file_path, out);
            for handler in &s.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                walk_stmts(&h.body, source, seed, file_path, out);
            }
            walk_stmts(&s.orelse, source, seed, file_path, out);
            walk_stmts(&s.finalbody, source, seed, file_path, out);
        }
        Stmt::With(s) => walk_stmts(&s.body, source, seed, file_path, out),
        Stmt::Assert(s) => {
            collect_expr(&s.test, source, seed, file_path, out);
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

impl Mutator for VariableInsert {
    #[inline]
    fn name(&self) -> &'static str {
        MUTATOR_NAME
    }

    #[inline]
    fn find_mutations(
        &self,
        source: &str,
        ast: &ruff_python_ast::ModModule,
        ctx: &MutationContext<'_>,
    ) -> Vec<Mutation> {
        let Some(seed) = ctx.seed else {
            return Vec::new();
        };
        let mut out = Vec::new();
        walk_stmts(&ast.body, source, seed, ctx.file_path, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: parse Python source and run the mutator with a seed.
    #[allow(clippy::expect_used, reason = "tests use expect for parse failures")]
    fn find_with_seed(source: &str, seed: Option<u64>) -> Vec<Mutation> {
        let parsed = ruff_python_parser::parse_module(source)
            .expect("valid Python source required for test");
        let ast = parsed.into_syntax();
        let ctx = MutationContext {
            file_path: std::path::Path::new("test.py"),
            seed,
        };
        VariableInsert.find_mutations(source, &ast, &ctx)
    }

    /// Without a seed, no mutations are produced.
    #[test]
    fn no_seed_no_mutations() {
        let mutations = find_with_seed("x = y + z", None);
        assert!(mutations.is_empty());
    }

    /// With a seed, Name nodes are wrapped with arithmetic ops.
    #[test]
    fn inserts_around_names() {
        let mutations = find_with_seed("x = y + z", Some(42_u64));
        // `y` and `z` on the RHS.
        assert_eq!(mutations.len(), 2_usize);
        for mutation in &mutations {
            assert!(mutation.replacement_text.starts_with('('));
            assert!(mutation.replacement_text.ends_with(')'));
        }
    }

    /// Same seed produces deterministic results.
    #[test]
    fn deterministic_with_seed() {
        let mutations_a = find_with_seed("x = y", Some(99_u64));
        let mutations_b = find_with_seed("x = y", Some(99_u64));
        assert_eq!(mutations_a, mutations_b);
    }

    /// Replacement contains the original name.
    #[test]
    fn replacement_contains_original() {
        let mutations = find_with_seed("x = y", Some(42_u64));
        assert_eq!(mutations.len(), 1_usize);
        assert!(mutations[0_usize].replacement_text.contains("y"));
    }

    /// Mutator name matches expected identifier.
    #[test]
    fn mutator_name() {
        assert_eq!(VariableInsert.name(), "variable_insert");
    }

    /// Byte offset and length of variable insertion are correct.
    #[test]
    fn byte_offset_is_correct() {
        let source = "x = y";
        let mutations = find_with_seed(source, Some(42_u64));
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find('y').expect("has y");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, 1_usize);
    }
}
