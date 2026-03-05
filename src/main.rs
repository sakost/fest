//! CLI entry point for fest.

/// Thin binary entry point — delegates to the library.
///
/// Exits with code 130 when the run is cancelled by a signal (SIGINT /
/// SIGTERM / SIGQUIT), matching the conventional Unix exit code for
/// signal-interrupted processes.
fn main() -> Result<(), fest::Error> {
    let args = fest::cli::parse();
    let run_args = fest::cli::run_args(args);

    match fest::run(run_args) {
        Ok(()) => Ok(()),
        Err(fest::Error::Cancelled(msg)) => {
            // Print the cancellation message to stderr, then exit 130.
            let mut stderr = std::io::stderr().lock();
            let _result =
                std::io::Write::write_all(&mut stderr, format!("cancelled: {msg}\n").as_bytes());
            #[allow(clippy::exit, reason = "intentional exit with signal code")]
            std::process::exit(130_i32);
        }
        Err(other) => Err(other),
    }
}
