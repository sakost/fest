//! Mutation engine — AST rewriting and mutant generation.
//!
//! This module contains the core mutation logic: walking a Python AST,
//! applying mutation operators, and producing [`Mutant`] descriptors.

/// Built-in mutation operators shipped with fest.
pub mod builtin;
