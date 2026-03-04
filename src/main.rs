//! CLI entry point for fest.

/// Thin binary entry point — delegates to the library.
fn main() -> Result<(), fest::Error> {
    let args = fest::cli::parse();
    let run_args = fest::cli::run_args(args);
    fest::run(run_args)
}
