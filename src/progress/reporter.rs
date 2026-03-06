//! Render handle, progress reporter, and mode resolution.
//!
//! [`RenderHandle`] owns the tokio render task and its shutdown.
//! [`ProgressReporter`] is a cheaply cloneable sender wrapper used by
//! pipeline workers to emit events without blocking.

use std::io::IsTerminal as _;

use tokio::sync::mpsc::UnboundedSender;

use super::event::{MutantDisplay, RenderEvent, SummaryInfo};
use crate::{cli::ProgressStyle, mutation::MutantResult};

/// Resolved render mode (no `Auto` — that is resolved at startup).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Rich output: phase animations, progress bar, colored summary.
    Fancy,
    /// Uncolored plain-text output with timing.
    Plain,
    /// Colored per-mutant lines and phase checkmarks (no progress bar).
    Verbose,
    /// Suppress all stderr progress output.
    Quiet,
}

/// Resolve a [`ProgressStyle`] CLI value into a concrete [`RenderMode`].
///
/// - `--verbose` flag overrides `--progress` to [`RenderMode::Verbose`].
/// - `Auto` selects `Fancy` when stderr is a TTY, `Quiet` otherwise.
#[inline]
#[must_use]
pub fn resolve_render_mode(verbose: bool, progress_style: ProgressStyle) -> RenderMode {
    if verbose {
        return RenderMode::Verbose;
    }
    match progress_style {
        ProgressStyle::Auto => {
            if std::io::stderr().is_terminal() {
                RenderMode::Fancy
            } else {
                RenderMode::Quiet
            }
        }
        ProgressStyle::Fancy => RenderMode::Fancy,
        ProgressStyle::Plain => RenderMode::Plain,
        ProgressStyle::Verbose => RenderMode::Verbose,
        ProgressStyle::Quiet => RenderMode::Quiet,
    }
}

// ---------------------------------------------------------------------------
// RenderHandle
// ---------------------------------------------------------------------------

/// Owns the background render task and provides controlled shutdown.
///
/// Created via [`new`](Self::new), which spawns the render loop on the
/// given tokio runtime.  Call [`shutdown`](Self::shutdown) after the
/// pipeline completes to flush output and join the task.
#[derive(Debug)]
pub struct RenderHandle {
    /// Channel sender kept alive so the render task does not exit early.
    sender: UnboundedSender<RenderEvent>,
    /// Join handle for the spawned render task.
    task_handle: tokio::task::JoinHandle<()>,
}

impl RenderHandle {
    /// Spawn the render task on `runtime` and return a handle.
    #[inline]
    pub fn new(runtime: &tokio::runtime::Runtime, mode: RenderMode) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let task_handle = runtime.spawn(super::render::render_loop(receiver, mode));
        Self {
            sender,
            task_handle,
        }
    }

    /// Create a [`ProgressReporter`] that sends events to this render task.
    #[inline]
    #[must_use]
    pub fn reporter(&self) -> ProgressReporter {
        ProgressReporter {
            sender: self.sender.clone(),
        }
    }

    /// Send a shutdown event and wait for the render task to finish.
    #[inline]
    pub async fn shutdown(self) {
        drop(self.sender.send(RenderEvent::Shutdown));
        drop(self.task_handle.await);
    }
}

// ---------------------------------------------------------------------------
// ProgressReporter
// ---------------------------------------------------------------------------

/// A cheaply cloneable sender for pipeline progress events.
///
/// All methods are non-blocking (unbounded channel send). Failures are
/// silently ignored since progress output is best-effort.
#[derive(Debug, Clone)]
pub struct ProgressReporter {
    /// Channel sender to the render task.
    sender: UnboundedSender<RenderEvent>,
}

impl ProgressReporter {
    /// Signal that a pipeline phase has started.
    #[inline]
    pub fn phase_start(&self, label: &str) {
        drop(self.sender.send(RenderEvent::PhaseStart {
            label: label.to_owned(),
        }));
    }

    /// Signal that a pipeline phase has completed.
    #[inline]
    pub fn phase_complete(
        &self,
        detail: &str,
        count_detail: Option<&str>,
        elapsed: core::time::Duration,
    ) {
        drop(self.sender.send(RenderEvent::PhaseComplete {
            detail: detail.to_owned(),
            count_detail: count_detail.map(str::to_owned),
            elapsed,
        }));
    }

    /// Signal that the mutant execution phase has started.
    #[inline]
    pub fn start_mutants(&self, total: u64) {
        drop(self.sender.send(RenderEvent::MutantsStart { total }));
    }

    /// Report the result of a single mutant.
    #[inline]
    pub fn report_mutant(&self, index: usize, total: usize, result: &MutantResult) {
        let summary = MutantDisplay {
            status: result.status.clone(),
            file_path: result.mutant.file_path.display().to_string(),
            line: result.mutant.line,
            mutator_name: result.mutant.mutator_name.clone(),
            original_text: result.mutant.original_text.clone(),
            mutated_text: result.mutant.mutated_text.clone(),
            duration: result.duration,
        };
        drop(self.sender.send(RenderEvent::MutantCompleted {
            index,
            total,
            summary,
        }));
    }

    /// Signal that all mutants have been processed.
    #[inline]
    pub fn finish_mutants(&self, cancelled: bool) {
        drop(self.sender.send(RenderEvent::MutantsFinish { cancelled }));
    }

    /// Emit a warning message (displayed inline in the progress output).
    #[inline]
    pub fn warning(&self, message: &str) {
        drop(self.sender.send(RenderEvent::Warning {
            message: message.to_owned(),
        }));
    }

    /// Emit the final summary scoreboard.
    #[inline]
    pub fn summary(&self, info: SummaryInfo) {
        drop(self.sender.send(RenderEvent::FinalSummary(info)));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbose flag overrides any progress style.
    #[test]
    fn resolve_verbose_flag_overrides_progress() {
        assert_eq!(
            resolve_render_mode(true, ProgressStyle::Auto),
            RenderMode::Verbose
        );
        assert_eq!(
            resolve_render_mode(true, ProgressStyle::Quiet),
            RenderMode::Verbose
        );
    }

    /// Explicit progress styles map to the expected modes.
    #[test]
    fn resolve_explicit_modes() {
        assert_eq!(
            resolve_render_mode(false, ProgressStyle::Fancy),
            RenderMode::Fancy
        );
        assert_eq!(
            resolve_render_mode(false, ProgressStyle::Plain),
            RenderMode::Plain
        );
        assert_eq!(
            resolve_render_mode(false, ProgressStyle::Verbose),
            RenderMode::Verbose
        );
        assert_eq!(
            resolve_render_mode(false, ProgressStyle::Quiet),
            RenderMode::Quiet
        );
    }

    /// Reporter can be cloned and methods do not panic even when the
    /// receiver has been dropped.
    #[test]
    fn reporter_clone_and_send_after_drop() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let reporter = ProgressReporter { sender };
        let cloned = reporter.clone();
        drop(receiver);

        // All sends silently fail (receiver gone) — no panic.
        cloned.phase_start("test");
        cloned.phase_complete("done", None, core::time::Duration::from_millis(1_u64));
        cloned.start_mutants(10_u64);
        cloned.finish_mutants(false);
        cloned.summary(SummaryInfo {
            score: 100.0,
            killed: 1_usize,
            survived: 0_usize,
            timeouts: 0_usize,
            errors: 0_usize,
            no_coverage: 0_usize,
            duration: core::time::Duration::from_secs(1_u64),
        });
    }

    /// `RenderHandle` can be constructed and shut down without panicking.
    #[test]
    fn render_handle_lifecycle() {
        let runtime = tokio::runtime::Runtime::new().expect("should create runtime");
        let handle = RenderHandle::new(&runtime, RenderMode::Quiet);
        let reporter = handle.reporter();
        reporter.phase_start("test");
        runtime.block_on(handle.shutdown());
    }
}
