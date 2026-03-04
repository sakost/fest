//! Mutation engine — AST rewriting and mutant generation.
//!
//! This module contains the core mutation logic: walking a Python AST,
//! applying mutation operators, and producing [`Mutant`] descriptors.

/// Built-in mutation operators shipped with fest.
pub mod builtin;
/// Data types for representing mutants and their execution results.
pub mod mutant;
/// Mutator trait and registry for mutation operators.
pub mod mutator;

pub use mutant::{Mutant, MutantResult, MutantStatus};
pub use mutator::{Mutation, Mutator, MutatorRegistry};
