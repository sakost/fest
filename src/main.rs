//! CLI entry point for fest.

/// Thin binary entry point — delegates to the library.
fn main() -> Result<(), fest::Error> {
    fest::run()
}
