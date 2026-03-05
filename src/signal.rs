//! Graceful signal handling for the mutation-testing pipeline.
//!
//! Provides [`CancellationState`] — an `Arc<AtomicBool>` wrapper that is set
//! when SIGINT, SIGTERM, or SIGQUIT is received. The main mutant loop polls
//! this flag between iterations to exit early while still producing a partial
//! report.
//!
//! A second signal forces an immediate exit with code 130 (the conventional
//! exit code for SIGINT termination).

extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

/// Shared cancellation flag polled by the pipeline.
#[derive(Debug, Clone)]
pub struct CancellationState {
    /// Set to `true` when the first signal is received.
    cancelled: Arc<AtomicBool>,
}

impl CancellationState {
    /// Create a new state with `cancelled = false`.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Returns `true` if a cancellation signal has been received.
    #[inline]
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Set the cancellation flag. Only available in test builds.
    #[cfg(test)]
    pub fn set_cancelled_for_test(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Default for CancellationState {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// Install signal handlers for SIGINT, SIGTERM, and SIGQUIT.
///
/// - First signal: sets `state.cancelled` → the pipeline finishes the current mutant, builds a
///   partial report, then returns `Error::Cancelled`.
/// - Second signal: calls [`std::process::exit(130)`].
///
/// The handlers are spawned as a tokio task on the provided `runtime`.
///
/// # Errors
///
/// Returns [`crate::Error::Runner`] if any Unix signal listener cannot be
/// created.
#[inline]
pub fn install_signal_handlers(
    runtime: &tokio::runtime::Runtime,
    state: &CancellationState,
) -> Result<(), crate::Error> {
    use tokio::signal::unix::{SignalKind, signal};

    // Enter the runtime context so `signal()` can register with the reactor.
    let _guard = runtime.enter();

    let mut sigint = signal(SignalKind::interrupt())
        .map_err(|err| crate::Error::Runner(format!("failed to install SIGINT handler: {err}")))?;
    let mut sigterm = signal(SignalKind::terminate())
        .map_err(|err| crate::Error::Runner(format!("failed to install SIGTERM handler: {err}")))?;
    let mut sigquit = signal(SignalKind::quit())
        .map_err(|err| crate::Error::Runner(format!("failed to install SIGQUIT handler: {err}")))?;

    let cancelled = Arc::clone(&state.cancelled);

    let _handle = runtime.spawn(async move {
        // Wait for the first signal from any of the three sources.
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
            _ = sigquit.recv() => {}
        }

        // Mark graceful cancellation.
        cancelled.store(true, Ordering::Relaxed);

        {
            let mut stderr = std::io::stderr().lock();
            // Best-effort — ignore write failures on stderr.
            let _result = std::io::Write::write_all(
                &mut stderr,
                b"\nReceived signal, finishing current mutant...\n",
            );
        }

        // Wait for a second signal → hard exit.
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
            _ = sigquit.recv() => {}
        }

        {
            // Second signal — abort immediately.
            let mut stderr = std::io::stderr().lock();
            let _result =
                std::io::Write::write_all(&mut stderr, b"\nReceived second signal, aborting.\n");
        }
        #[allow(clippy::exit, reason = "intentional hard exit on second signal")]
        std::process::exit(130_i32);
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Default state is not cancelled.
    #[test]
    fn default_not_cancelled() {
        let state = CancellationState::new();
        assert!(!state.is_cancelled());
    }

    /// Setting the flag is observable.
    #[test]
    fn manual_cancellation() {
        let state = CancellationState::new();
        state.cancelled.store(true, Ordering::Relaxed);
        assert!(state.is_cancelled());
    }

    /// Clones share the same flag.
    #[test]
    fn clone_shares_flag() {
        let state = CancellationState::new();
        let clone = state.clone();
        state.cancelled.store(true, Ordering::Relaxed);
        assert!(clone.is_cancelled());
    }

    /// Signal handlers can be installed on a tokio runtime.
    #[test]
    fn install_handlers_succeeds() {
        let runtime = tokio::runtime::Runtime::new().ok();
        if let Some(runtime) = runtime.as_ref() {
            let state = CancellationState::new();
            let result = install_signal_handlers(runtime, &state);
            assert!(result.is_ok());
        }
    }
}
