//! Built-in mutation operators shipped with fest.
//!
//! Each operator targets a specific syntactic pattern (e.g. arithmetic,
//! comparison, boolean) and produces one or more mutated variants.

/// Arithmetic operator mutations (`+` <-> `-`, `*` <-> `/`, etc.).
pub mod arithmetic;
/// Augmented assignment operator mutations (`+=` <-> `-=`, etc.).
pub mod augmented_assign;
/// Bitwise operator mutations (`&` <-> `|`, `<<` <-> `>>`, etc.).
pub mod bitwise;
/// Boolean operator mutations (`and` <-> `or`, `not` removal).
pub mod boolean;
/// Break/continue swap mutations.
pub mod break_continue;
/// Comparison operator mutations (`==` <-> `!=`, `<` <-> `>=`, etc.).
pub mod comparison;
/// Constant literal mutations (`True` <-> `False`, `0` <-> `1`, etc.).
pub mod constant;
/// Exception handler body replacement with `pass`.
pub mod exception;
/// Condition negation in `if`/`elif`/`while` statements.
pub mod negate_condition;
/// Decorator removal from functions and classes.
pub mod remove_decorator;
/// Super-call removal mutations.
pub mod remove_super;
/// Return value mutations (`return expr` -> `return None`, etc.).
pub mod return_value;
/// Statement deletion mutations.
pub mod statement_deletion;
/// Unary operator mutations.
pub mod unary;
/// Variable insertion mutations (requires seed).
pub mod variable_insert;
/// Variable replacement mutations (requires seed).
pub mod variable_replace;
/// Zero-iteration loop mutations.
pub mod zero_iteration;

use crate::mutation::mutator::Mutator;

/// Create a [`Vec`] of all built-in mutators.
///
/// Returns one instance of every built-in mutator, ready to be registered
/// in a [`MutatorRegistry`](crate::mutation::mutator::MutatorRegistry).
#[inline]
#[must_use]
pub fn all_builtins() -> Vec<Box<dyn Mutator>> {
    vec![
        Box::new(arithmetic::ArithmeticOp),
        Box::new(comparison::ComparisonOp),
        Box::new(boolean::BooleanOp),
        Box::new(return_value::ReturnValue),
        Box::new(negate_condition::NegateCondition),
        Box::new(remove_decorator::RemoveDecorator),
        Box::new(constant::ConstantReplace),
        Box::new(exception::ExceptionSwallow),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The factory returns exactly 8 built-in mutators.
    #[test]
    fn all_builtins_count() {
        let builtins = all_builtins();
        assert_eq!(builtins.len(), 8_usize);
    }

    /// Each built-in mutator has a unique name.
    #[test]
    fn all_builtins_unique_names() {
        let builtins = all_builtins();
        let mut names: Vec<&str> = builtins.iter().map(|m| m.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 8_usize);
    }
}
