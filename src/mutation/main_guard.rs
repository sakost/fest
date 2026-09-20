//! Detection of module-level `if __name__ == "__main__":` guards.
//!
//! Nothing under such a guard runs when the module is imported by a test,
//! so no mutant there can be killed legitimately — and *applying* one can
//! be actively harmful: flipping `==` to `!=` makes `main()` run at import
//! time, inside the test process (issue #16). Mutants whose span starts
//! inside a guard's test expression or body are therefore never generated.
//! `elif`/`else` clauses *do* run under import and stay mutable.

use ruff_python_ast::{CmpOp, Expr, ModModule, Stmt, StmtIf};
use ruff_text_size::{Ranged, TextRange};

/// Byte ranges covered by every module-level `__main__` guard in `module`:
/// from the `if` keyword to the end of the last statement in its body.
#[inline]
#[must_use]
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt requires pattern_type_mismatch suppression"
)]
pub fn main_guard_ranges(module: &ModModule) -> Vec<TextRange> {
    module
        .body
        .iter()
        .filter_map(|stmt| {
            let Stmt::If(if_stmt) = stmt else {
                return None;
            };
            is_main_guard_test(&if_stmt.test).then(|| guard_range(if_stmt))
        })
        .collect()
}

/// Whether `byte_offset` falls inside any of `ranges`.
#[inline]
#[must_use]
pub fn is_inside_any(ranges: &[TextRange], byte_offset: usize) -> bool {
    u32::try_from(byte_offset).is_ok_and(|offset| {
        ranges
            .iter()
            .any(|range| range.contains_inclusive(offset.into()))
    })
}

/// `if` keyword through the last body statement — excludes `elif`/`else`.
fn guard_range(if_stmt: &StmtIf) -> TextRange {
    let end = if_stmt
        .body
        .last()
        .map_or_else(|| if_stmt.test.range().end(), Ranged::end);
    TextRange::new(if_stmt.range().start(), end)
}

/// `__name__ == "__main__"` in either operand order.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr requires pattern_type_mismatch suppression"
)]
fn is_main_guard_test(test: &Expr) -> bool {
    let Expr::Compare(compare) = test else {
        return false;
    };
    if compare.ops.as_ref() != [CmpOp::Eq] {
        return false;
    }
    let Some(right) = compare.comparators.first() else {
        return false;
    };
    (is_dunder_name(&compare.left) && is_main_literal(right))
        || (is_main_literal(&compare.left) && is_dunder_name(right))
}

/// The bare name `__name__`.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr requires pattern_type_mismatch suppression"
)]
fn is_dunder_name(expr: &Expr) -> bool {
    matches!(expr, Expr::Name(name) if name.id.as_str() == "__name__")
}

/// The string literal `"__main__"`.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr requires pattern_type_mismatch suppression"
)]
fn is_main_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::StringLiteral(lit) if lit.value.to_str() == "__main__")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `source` and return its guard ranges.
    fn ranges(source: &str) -> Vec<TextRange> {
        let parsed = ruff_python_parser::parse_module(source).expect("valid Python");
        main_guard_ranges(&parsed.into_syntax())
    }

    /// A plain guard covers the `if` line and its body, nothing before.
    #[test]
    fn plain_guard_spans_test_and_body() {
        let source = "x = 1\nif __name__ == \"__main__\":\n    main()\n";
        let found = ranges(source);
        assert_eq!(found.len(), 1_usize);
        let guard_start = source.find("if ").expect("if present");
        assert_eq!(found[0_usize].start().to_usize(), guard_start);
        assert_eq!(found[0_usize].end().to_usize(), source.trim_end().len());
    }

    /// `elif`/`else` clauses are not part of the excluded range.
    #[test]
    fn else_clause_is_outside_range() {
        let source = "if __name__ == \"__main__\":\n    a()\nelse:\n    b()\n";
        let found = ranges(source);
        let else_pos = source.find("else").expect("else present");
        assert!(!is_inside_any(&found, else_pos));
        assert!(is_inside_any(
            &found,
            source.find("a()").expect("a() present")
        ));
    }

    /// Other comparisons and operators are not guards.
    #[test]
    fn non_guard_ifs_are_ignored() {
        assert!(ranges("if __name__ != \"__main__\":\n    pass\n").is_empty());
        assert!(ranges("if __name__ == \"__other__\":\n    pass\n").is_empty());
        assert!(ranges("if name == \"__main__\":\n    pass\n").is_empty());
        assert!(ranges("if __name__ == \"__main__\" == x:\n    pass\n").is_empty());
    }

    /// Only module-level statements count.
    #[test]
    fn nested_guard_is_ignored() {
        assert!(ranges("def f():\n    if __name__ == \"__main__\":\n        pass\n").is_empty());
    }

    /// Reversed operands are recognised.
    #[test]
    fn reversed_operands_are_a_guard() {
        assert_eq!(
            ranges("if \"__main__\" == __name__:\n    pass\n").len(),
            1_usize
        );
    }
}
