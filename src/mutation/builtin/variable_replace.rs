//! Variable replacement mutation (requires seed).
//!
//! Replaces `Expr::Name` references on the right-hand side of assignments
//! with a deterministic integer constant derived from the seed. Without a
//! seed, no mutations are produced.

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::mutation::{
    mutator::{Mutation, MutationContext, Mutator},
    seed::derive_value,
};

/// Mutator that replaces variable references with seed-derived constants.
#[derive(Debug)]
pub struct VariableReplace;

/// Name of this mutator (used for seed derivation).
const MUTATOR_NAME: &str = "variable_replace";

/// Derive a replacement integer string from seed and byte offset.
#[allow(
    clippy::cast_possible_wrap,
    reason = "val % 201 is at most 200, which fits in i64"
)]
fn replacement_constant(seed: u64, file_path: &std::path::Path, byte_offset: usize) -> String {
    let val = derive_value(seed, file_path, byte_offset, MUTATOR_NAME);
    // Map to range [-100, 100] — 201 distinct values.
    let num = (val % 201_u64) as i64 - 100_i64;
    num.to_string()
}

/// Collect mutations from a single expression (replacing Name nodes).
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
            let replacement = replacement_constant(seed, file_path, start);
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

/// Walk all statements collecting variable replacement mutations.
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

impl Mutator for VariableReplace {
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
        VariableReplace.find_mutations(source, &ast, &ctx)
    }

    /// Without a seed, no mutations are produced.
    #[test]
    fn no_seed_no_mutations() {
        let mutations = find_with_seed("x = y + z", None);
        assert!(mutations.is_empty());
    }

    /// With a seed, Name nodes on RHS are replaced with integer constants.
    #[test]
    fn replaces_name_refs() {
        let mutations = find_with_seed("x = y + z", Some(42_u64));
        // `y` and `z` on the RHS are Name nodes.
        assert_eq!(mutations.len(), 2_usize);
        // Replacement should be a valid integer string.
        for mutation in &mutations {
            assert!(mutation.replacement_text.parse::<i64>().is_ok());
        }
    }

    /// Same seed produces deterministic replacements.
    #[test]
    fn deterministic_with_seed() {
        let mutations_a = find_with_seed("x = y", Some(99_u64));
        let mutations_b = find_with_seed("x = y", Some(99_u64));
        assert_eq!(mutations_a, mutations_b);
    }

    /// Different seeds produce different replacements.
    #[test]
    fn different_seeds_differ() {
        let mutations_a = find_with_seed("x = y", Some(1_u64));
        let mutations_b = find_with_seed("x = y", Some(2_u64));
        assert_ne!(
            mutations_a[0_usize].replacement_text,
            mutations_b[0_usize].replacement_text
        );
    }

    /// Replacement constants are in range [-100, 100].
    #[test]
    fn replacement_in_range() {
        let mutations = find_with_seed("a = b + c + d + e", Some(12345_u64));
        for mutation in &mutations {
            let val: i64 = mutation
                .replacement_text
                .parse()
                .expect("should be integer");
            assert!((-100_i64..=100_i64).contains(&val));
        }
    }

    /// Mutator name matches expected identifier.
    #[test]
    fn mutator_name() {
        assert_eq!(VariableReplace.name(), "variable_replace");
    }

    /// Byte offset and length of variable replacement are correct.
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
