//! CLI entry point for fest.

/// Thin binary entry point — delegates to the library.
///
/// Exits with code 130 when the run is cancelled by a signal (SIGINT /
/// SIGTERM / SIGQUIT), matching the conventional Unix exit code for
/// signal-interrupted processes.
fn main() -> Result<(), fest::Error> {
    let args = fest::cli::parse();

    match args.command {
        Some(fest::cli::Command::Init(init_args)) => {
            let cwd = std::env::current_dir()
                .map_err(|err| fest::Error::Config(format!("cannot determine cwd: {err}")))?;
            fest::init::run(&init_args, &cwd)
        }
        other => {
            let run_args = fest::cli::run_args(fest::cli::Args { command: other });
            match fest::run(run_args) {
                Ok(()) => Ok(()),
                Err(fest::Error::Cancelled(msg)) => {
                    let mut stderr = std::io::stderr().lock();
                    let _result = std::io::Write::write_all(
                        &mut stderr,
                        format!("cancelled: {msg}\n").as_bytes(),
                    );
                    #[allow(clippy::exit, reason = "intentional exit with signal code")]
                    std::process::exit(130_i32);
                }
                Err(other_err) => Err(other_err),
            }
        }
    }
}
