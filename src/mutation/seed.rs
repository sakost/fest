//! Deterministic per-mutation value derivation from a seed.

use core::hash::{BuildHasher, Hash, Hasher};
use std::path::Path;

use ahash::RandomState;

/// Derive a deterministic `u64` value from (seed, `file_path`, `byte_offset`, `mutator_name`).
///
/// Uses `ahash` for speed. The result is suitable for modulo indexing into
/// a fixed-size choice table.
#[allow(
    clippy::redundant_pub_crate,
    reason = "pub(crate) clarifies intent for crate-internal API"
)]
pub(crate) fn derive_value(
    seed: u64,
    file_path: &Path,
    byte_offset: usize,
    mutator_name: &str,
) -> u64 {
    let state = RandomState::with_seeds(seed, seed, seed, seed);
    let mut hasher = state.build_hasher();
    file_path.hash(&mut hasher);
    byte_offset.hash(&mut hasher);
    mutator_name.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same inputs produce the same value.
    #[test]
    fn deterministic() {
        let val_a = derive_value(
            42_u64,
            Path::new("src/app.py"),
            10_usize,
            "variable_replace",
        );
        let val_b = derive_value(
            42_u64,
            Path::new("src/app.py"),
            10_usize,
            "variable_replace",
        );
        assert_eq!(val_a, val_b);
    }

    /// Different seeds produce different values.
    #[test]
    fn different_seeds() {
        let val_a = derive_value(1_u64, Path::new("src/app.py"), 10_usize, "variable_replace");
        let val_b = derive_value(2_u64, Path::new("src/app.py"), 10_usize, "variable_replace");
        assert_ne!(val_a, val_b);
    }

    /// Different offsets produce different values.
    #[test]
    fn different_offsets() {
        let val_a = derive_value(
            42_u64,
            Path::new("src/app.py"),
            10_usize,
            "variable_replace",
        );
        let val_b = derive_value(
            42_u64,
            Path::new("src/app.py"),
            20_usize,
            "variable_replace",
        );
        assert_ne!(val_a, val_b);
    }

    /// Different mutator names produce different values.
    #[test]
    fn different_mutator_names() {
        let val_a = derive_value(
            42_u64,
            Path::new("src/app.py"),
            10_usize,
            "variable_replace",
        );
        let val_b = derive_value(42_u64, Path::new("src/app.py"), 10_usize, "variable_insert");
        assert_ne!(val_a, val_b);
    }
}
