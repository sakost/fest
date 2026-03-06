//! Constant replacement mutation.
//!
//! Swaps `True` <-> `False`, `0` <-> `1`, integer/float constants to
//! `original + 1`, and empty string `""` <-> `"mutant"`.

use ruff_python_ast::{Expr, Number, Stmt};

use crate::mutation::mutator::{Mutation, MutationContext, Mutator};

/// Mutator that replaces constant literals.
#[derive(Debug)]
pub struct ConstantReplace;

/// Create a mutation for a boolean literal (`True` <-> `False`).
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn boolean_mutation(bool_lit: &ruff_python_ast::ExprBooleanLiteral, source: &str) -> Mutation {
    let start = bool_lit.range.start().to_usize();
    let end = bool_lit.range.end().to_usize();
    let original = &source[start..end];
    let replacement = if bool_lit.value { "False" } else { "True" };
    Mutation {
        byte_offset: start,
        byte_length: end - start,
        original_text: original.to_owned(),
        replacement_text: replacement.to_owned(),
    }
}

/// Create a mutation for a number literal.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on Number requires pattern_type_mismatch suppression"
)]
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn number_mutation(num_lit: &ruff_python_ast::ExprNumberLiteral, source: &str) -> Option<Mutation> {
    let start = num_lit.range.start().to_usize();
    let end = num_lit.range.end().to_usize();
    let original = &source[start..end];

    let replacement = match &num_lit.value {
        Number::Int(int_val) => int_replacement(int_val)?,
        Number::Float(float_val) => float_replacement(*float_val),
        Number::Complex { .. } => return None,
    };

    Some(Mutation {
        byte_offset: start,
        byte_length: end - start,
        original_text: original.to_owned(),
        replacement_text: replacement,
    })
}

/// Compute the replacement string for an integer literal.
fn int_replacement(int_val: &ruff_python_ast::Int) -> Option<String> {
    let value = int_val.as_i64()?;
    if value == 0_i64 {
        Some("1".to_owned())
    } else if value == 1_i64 {
        Some("0".to_owned())
    } else {
        let incremented = value.checked_add(1_i64)?;
        Some(incremented.to_string())
    }
}

/// Compute the replacement string for a float literal.
#[allow(
    clippy::float_cmp,
    reason = "exact comparison intended for 0.0 and 1.0 sentinel values"
)]
fn float_replacement(float_val: f64) -> String {
    if float_val == 0.0_f64 {
        "1.0".to_owned()
    } else if float_val == 1.0_f64 {
        "0.0".to_owned()
    } else {
        let incremented = float_val + 1.0_f64;
        format!("{incremented}")
    }
}

/// Create a mutation for a string literal (empty `""` <-> `"mutant"`).
#[allow(
    clippy::string_slice,
    reason = "byte offsets from AST are always valid UTF-8 boundaries"
)]
fn string_mutation(str_lit: &ruff_python_ast::ExprStringLiteral, source: &str) -> Mutation {
    let start = str_lit.range.start().to_usize();
    let end = str_lit.range.end().to_usize();
    let original = &source[start..end];
    let str_value = str_lit.value.to_str();
    let replacement = if str_value.is_empty() {
        "\"mutant\""
    } else {
        "\"\""
    };
    Mutation {
        byte_offset: start,
        byte_length: end - start,
        original_text: original.to_owned(),
        replacement_text: replacement.to_owned(),
    }
}

/// Recursively collect mutations from an expression.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr requires pattern_type_mismatch suppression"
)]
fn collect_expr(expr: &Expr, source: &str, out: &mut Vec<Mutation>) {
    match expr {
        Expr::BooleanLiteral(b) => out.push(boolean_mutation(b, source)),
        Expr::NumberLiteral(n) => {
            if let Some(m) = number_mutation(n, source) {
                out.push(m);
            }
        }
        Expr::StringLiteral(s) => out.push(string_mutation(s, source)),
        Expr::BinOp(e) => {
            collect_expr(&e.left, source, out);
            collect_expr(&e.right, source, out);
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
        Expr::List(e) => visit_exprs(&e.elts, source, out),
        Expr::Tuple(e) => visit_exprs(&e.elts, source, out),
        Expr::Dict(e) => collect_dict(e, source, out),
        Expr::Set(e) => visit_exprs(&e.elts, source, out),
        Expr::Subscript(e) => {
            collect_expr(&e.value, source, out);
            collect_expr(&e.slice, source, out);
        }
        Expr::Named(_)
        | Expr::Lambda(_)
        | Expr::ListComp(_)
        | Expr::SetComp(_)
        | Expr::DictComp(_)
        | Expr::Generator(_)
        | Expr::Await(_)
        | Expr::Yield(_)
        | Expr::YieldFrom(_)
        | Expr::FString(_)
        | Expr::BytesLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_)
        | Expr::Attribute(_)
        | Expr::Starred(_)
        | Expr::Name(_)
        | Expr::Slice(_)
        | Expr::IpyEscapeCommand(_) => {}
    }
}

/// Collect mutations from dict items.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on Option<Expr> requires suppression"
)]
fn collect_dict(dict: &ruff_python_ast::ExprDict, source: &str, out: &mut Vec<Mutation>) {
    for item in &dict.items {
        if let Some(key) = &item.key {
            collect_expr(key, source, out);
        }
        collect_expr(&item.value, source, out);
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
        }
        Stmt::For(s) => walk_stmts(&s.body, source, out),
        Stmt::Try(s) => walk_try(s, source, out),
        Stmt::Assert(s) => collect_expr(&s.test, source, out),
        Stmt::Delete(_)
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
}

impl Mutator for ConstantReplace {
    #[inline]
    fn name(&self) -> &'static str {
        "constant_replace"
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
        ConstantReplace.find_mutations(source, &ast, &ctx)
    }

    /// `True` is swapped to `False`.
    #[test]
    fn swap_true_to_false() {
        let mutations = find("x = True");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "True");
        assert_eq!(mutations[0_usize].replacement_text, "False");
    }

    /// `False` is swapped to `True`.
    #[test]
    fn swap_false_to_true() {
        let mutations = find("x = False");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "False");
        assert_eq!(mutations[0_usize].replacement_text, "True");
    }

    /// `0` is swapped to `1`.
    #[test]
    fn swap_zero_to_one() {
        let mutations = find("x = 0");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "0");
        assert_eq!(mutations[0_usize].replacement_text, "1");
    }

    /// `1` is swapped to `0`.
    #[test]
    fn swap_one_to_zero() {
        let mutations = find("x = 1");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "1");
        assert_eq!(mutations[0_usize].replacement_text, "0");
    }

    /// Other integers are incremented.
    #[test]
    fn increment_integer() {
        let mutations = find("x = 42");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].original_text, "42");
        assert_eq!(mutations[0_usize].replacement_text, "43");
    }

    /// Float `0.0` is swapped to `1.0`.
    #[test]
    fn swap_float_zero() {
        let mutations = find("x = 0.0");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].replacement_text, "1.0");
    }

    /// Empty string is replaced with `"mutant"`.
    #[test]
    fn empty_string_to_mutant() {
        let mutations = find("x = \"\"");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].replacement_text, "\"mutant\"");
    }

    /// Non-empty string is replaced with `""`.
    #[test]
    fn nonempty_string_to_empty() {
        let mutations = find("x = \"hello\"");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].replacement_text, "\"\"");
    }

    /// Mutations are found inside if blocks.
    #[test]
    fn inside_if_block() {
        let source = "if True:\n    x = 42";
        let mutations = find(source);
        assert!(!mutations.is_empty());
    }

    /// Mutations are found inside try/except blocks.
    #[test]
    fn inside_try_block() {
        let source = "try:\n    x = 42\nexcept:\n    y = 0";
        let mutations = find(source);
        assert_eq!(mutations.len(), 2_usize);
    }

    /// Mutations are found inside expressions visited via visit_exprs (e.g. bool_op values).
    #[test]
    fn inside_bool_op_values() {
        let source = "x = 1 or 2";
        let mutations = find(source);
        assert_eq!(mutations.len(), 2_usize);
    }

    /// Mutations are found inside dict literals.
    #[test]
    fn inside_dict() {
        let source = "x = {1: 2}";
        let mutations = find(source);
        assert_eq!(mutations.len(), 2_usize);
    }

    /// Byte offset for boolean literal is correct.
    #[test]
    fn boolean_byte_offset() {
        let source = "x = True";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find("True").expect("has True");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, 4_usize);
    }

    /// Byte offset for number literal is correct.
    #[test]
    fn number_byte_offset() {
        let source = "x = 42";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find("42").expect("has 42");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, 2_usize);
    }

    /// Byte offset for string literal is correct.
    #[test]
    fn string_byte_offset() {
        let source = "x = \"hello\"";
        let mutations = find(source);
        assert_eq!(mutations.len(), 1_usize);
        let expected_offset = source.find('"').expect("has quote");
        assert_eq!(mutations[0_usize].byte_offset, expected_offset);
        assert_eq!(mutations[0_usize].byte_length, 7_usize);
    }

    /// Float `1.0` is swapped to `0.0`.
    #[test]
    fn swap_float_one() {
        let mutations = find("x = 1.0");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].replacement_text, "0.0");
    }

    /// Non-special float is incremented.
    #[test]
    fn increment_float() {
        let mutations = find("x = 2.5");
        assert_eq!(mutations.len(), 1_usize);
        assert_eq!(mutations[0_usize].replacement_text, "3.5");
    }
}
