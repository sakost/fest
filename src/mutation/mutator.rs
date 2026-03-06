//! Mutator trait and registry for mutation operators.
//!
//! A [`Mutator`] knows how to inspect Python source code (both raw text and
//! its parsed AST) and produce a set of [`Mutation`] descriptors.  The
//! [`MutatorRegistry`] collects all active mutators and provides iteration.

/// A single text-level mutation produced by a [`Mutator`].
///
/// Describes a byte-range replacement in the source text: the region
/// starting at [`byte_offset`](Self::byte_offset) with length
/// [`byte_length`](Self::byte_length) should be replaced by
/// [`replacement_text`](Self::replacement_text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mutation {
    /// Byte offset into the source text where the replaced region begins.
    pub byte_offset: usize,
    /// Length in bytes of the replaced region.
    pub byte_length: usize,
    /// The original source text that will be replaced.
    pub original_text: String,
    /// The replacement text that constitutes the mutation.
    pub replacement_text: String,
}

/// Context passed to every mutator during a mutation-generation run.
///
/// Carries per-file and per-run information that operators may use to
/// produce deterministic, seed-dependent mutations.
#[derive(Debug, Clone)]
pub struct MutationContext<'ctx> {
    /// Path of the file being mutated.
    pub file_path: &'ctx std::path::Path,
    /// Global seed for deterministic randomised operators.
    pub seed: Option<u64>,
}

/// A mutation operator that can inspect Python source code and produce
/// candidate mutations.
///
/// Implementors walk the parsed AST (or the raw source text) looking for
/// patterns they know how to mutate, and return a [`Vec<Mutation>`] of
/// proposed text replacements.
pub trait Mutator: Send + Sync {
    /// Human-readable name of this mutator (e.g. `"arithmetic_op"`).
    fn name(&self) -> &str;

    /// Scan the given Python source and its AST, returning all mutations
    /// this operator can produce.
    fn find_mutations(
        &self,
        source: &str,
        ast: &ruff_python_ast::ModModule,
        ctx: &MutationContext<'_>,
    ) -> Vec<Mutation>;
}

/// A collection of active [`Mutator`] implementations.
///
/// The registry owns its mutators and provides methods to register new
/// ones and iterate over the full set.
#[derive(Debug, Default)]
pub struct MutatorRegistry {
    /// Registered mutators.
    mutators: Vec<Box<dyn Mutator>>,
}

impl MutatorRegistry {
    /// Create an empty registry.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self {
            mutators: Vec::new(),
        }
    }

    /// Register a mutator.
    #[inline]
    pub fn register(&mut self, mutator: Box<dyn Mutator>) {
        self.mutators.push(mutator);
    }

    /// Return the number of registered mutators.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.mutators.len()
    }

    /// Return `true` if no mutators are registered.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mutators.is_empty()
    }

    /// Iterate over registered mutators.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &dyn Mutator> {
        self.mutators.iter().map(AsRef::as_ref)
    }
}

/// Manual [`Debug`] implementation for `dyn Mutator` so that
/// [`MutatorRegistry`] can derive `Debug`.
impl core::fmt::Debug for dyn Mutator {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Mutator({})", self.name())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial mutator used only in tests.
    struct StubMutator {
        /// Name returned by [`Mutator::name`].
        label: &'static str,
    }

    impl Mutator for StubMutator {
        fn name(&self) -> &str {
            self.label
        }

        fn find_mutations(
            &self,
            _source: &str,
            _ast: &ruff_python_ast::ModModule,
            _ctx: &MutationContext<'_>,
        ) -> Vec<Mutation> {
            Vec::new()
        }
    }

    /// `Mutation` struct can be constructed and compared.
    #[test]
    fn mutation_construction_and_equality() {
        let mutation_a = Mutation {
            byte_offset: 10_usize,
            byte_length: 3_usize,
            original_text: "and".to_owned(),
            replacement_text: "or".to_owned(),
        };

        let mutation_b = mutation_a.clone();
        assert_eq!(mutation_a, mutation_b);
    }

    /// Two different `Mutation` values are not equal.
    #[test]
    fn mutation_inequality() {
        let mutation_a = Mutation {
            byte_offset: 0_usize,
            byte_length: 1_usize,
            original_text: "+".to_owned(),
            replacement_text: "-".to_owned(),
        };

        let mutation_b = Mutation {
            byte_offset: 0_usize,
            byte_length: 1_usize,
            original_text: "+".to_owned(),
            replacement_text: "*".to_owned(),
        };

        assert_ne!(mutation_a, mutation_b);
    }

    /// An empty `MutatorRegistry` reports length 0 and `is_empty`.
    #[test]
    fn empty_registry() {
        let registry = MutatorRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0_usize);
        assert_eq!(registry.iter().count(), 0_usize);
    }

    /// Registering mutators increments the length and they can be iterated.
    #[test]
    fn register_and_iterate() {
        let mut registry = MutatorRegistry::new();

        registry.register(Box::new(StubMutator {
            label: "arithmetic_op",
        }));
        registry.register(Box::new(StubMutator {
            label: "comparison_op",
        }));

        assert_eq!(registry.len(), 2_usize);
        assert!(!registry.is_empty());

        let names: Vec<&str> = registry.iter().map(Mutator::name).collect();
        assert_eq!(names, vec!["arithmetic_op", "comparison_op"]);
    }

    /// `MutatorRegistry::default` produces an empty registry.
    #[test]
    fn default_registry_is_empty() {
        let registry = MutatorRegistry::default();
        assert!(registry.is_empty());
    }

    /// The `Debug` impl for `dyn Mutator` produces a human-readable string.
    #[test]
    fn mutator_debug_impl() {
        let stub = StubMutator { label: "test_mut" };
        let trait_ref: &dyn Mutator = &stub;
        let debug_str = format!("{trait_ref:?}");
        assert_eq!(debug_str, "Mutator(test_mut)");
    }
}
