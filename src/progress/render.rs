//! Async render loop for the dedicated terminal output task.
//!
//! Receives [`RenderEvent`]s from the pipeline and updates the terminal.
//! All stderr writes happen on this single task, so worker threads never
//! block on I/O.

use console::Term;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::sync::mpsc::UnboundedReceiver;

use super::{
    event::{RenderEvent, SummaryInfo},
    reporter::RenderMode,
    style,
};

/// Mutable state for the render loop.
struct RenderState {
    /// Handle to the stderr terminal.
    term: Term,
    /// Active render mode.
    mode: RenderMode,
    /// Whether ANSI colors are enabled.
    colored: bool,
    /// Whether the terminal supports line overwriting.
    can_overwrite: bool,
    /// Whether a phase-start line is currently on screen.
    phase_line_active: bool,
    /// Optional progress bar (Fancy mode only).
    bar: Option<ProgressBar>,
}

impl RenderState {
    /// Create a new render state for the given mode.
    fn new(mode: RenderMode) -> Self {
        let term = Term::stderr();
        let colored = matches!(mode, RenderMode::Fancy | RenderMode::Verbose);
        let can_overwrite = term.is_term() && colored;
        Self {
            term,
            mode,
            colored,
            can_overwrite,
            phase_line_active: false,
            bar: None,
        }
    }

    /// Print the startup banner.
    fn write_banner(&self) {
        if self.mode == RenderMode::Quiet {
            return;
        }
        let banner = style::format_banner(self.colored);
        drop(self.term.write_line(&banner));
        drop(self.term.write_line(""));
    }

    /// Dispatch a single event to the appropriate handler.
    fn handle_event(&mut self, event: RenderEvent) {
        match event {
            RenderEvent::PhaseStart { label } => self.on_phase_start(&label),
            RenderEvent::PhaseComplete {
                detail,
                count_detail,
                elapsed,
            } => self.on_phase_complete(&detail, count_detail.as_deref(), elapsed),
            RenderEvent::MutantsStart { total } => self.on_mutants_start(total),
            RenderEvent::MutantCompleted {
                index,
                total,
                summary,
            } => self.on_mutant_completed(index, total, &summary),
            RenderEvent::MutantsFinish { cancelled } => self.on_mutants_finish(cancelled),
            RenderEvent::FinalSummary(info) => self.on_summary(&info),
            RenderEvent::Shutdown => {} // handled in the loop
        }
    }

    /// Handle a phase-start event.
    fn on_phase_start(&mut self, label: &str) {
        if self.mode == RenderMode::Quiet {
            return;
        }
        if self.can_overwrite {
            let spinner = if self.colored { "⠋" } else { "-" };
            drop(self.term.write_line(&format!("  {spinner} {label}...")));
            self.phase_line_active = true;
        }
    }

    /// Handle a phase-complete event.
    fn on_phase_complete(
        &mut self,
        detail: &str,
        count_detail: Option<&str>,
        elapsed: core::time::Duration,
    ) {
        if self.mode == RenderMode::Quiet {
            return;
        }
        if self.phase_line_active && self.can_overwrite {
            drop(self.term.clear_last_lines(1_usize));
            self.phase_line_active = false;
        }

        let timing = style::format_duration(elapsed);
        let count = count_detail.map_or_else(String::new, |ct| format!(" ({ct})"));

        let line = if self.colored {
            let check = console::style("✔").green().bold().force_styling(true);
            let dim_time = console::style(&timing).dim().force_styling(true);
            format!("  {check} {detail}{count}  {dim_time}")
        } else {
            format!("  {detail}{count}  {timing}")
        };
        drop(self.term.write_line(&line));
    }

    /// Handle a mutants-start event — create the progress bar.
    fn on_mutants_start(&mut self, total: u64) {
        if self.mode != RenderMode::Fancy || !self.term.is_term() {
            return;
        }
        if self.phase_line_active {
            drop(self.term.clear_last_lines(1_usize));
            self.phase_line_active = false;
        }
        let pb = ProgressBar::new(total);
        let template = ProgressStyle::with_template(
            "  {spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} mutants ({eta} remaining)",
        );
        if let Ok(styled) = template {
            pb.set_style(styled.progress_chars("#>-"));
        }
        self.bar = Some(pb);
    }

    /// Handle a single mutant-completed event.
    fn on_mutant_completed(
        &mut self,
        index: usize,
        total: usize,
        summary: &super::event::MutantDisplay,
    ) {
        match self.mode {
            RenderMode::Fancy => {
                if let Some(pb) = self.bar.as_ref() {
                    pb.inc(1_u64);
                }
            }
            RenderMode::Verbose => {
                self.phase_line_active = false;
                let line = style::format_colored_mutant_line(index, total, summary);
                drop(self.term.write_line(&line));
            }
            RenderMode::Plain | RenderMode::Quiet => {}
        }
    }

    /// Handle the mutants-finish event — tear down the bar.
    fn on_mutants_finish(&mut self, cancelled: bool) {
        if let Some(pb) = self.bar.take() {
            if cancelled {
                pb.abandon_with_message("cancelled");
            } else {
                pb.finish_and_clear();
            }
        }
    }

    /// Handle the final summary event.
    fn on_summary(&self, info: &SummaryInfo) {
        if self.mode == RenderMode::Quiet {
            return;
        }
        let line = style::format_summary_line(info, self.colored);
        drop(self.term.write_line(""));
        drop(self.term.write_line(&line));
    }
}

/// Run the render loop until a [`RenderEvent::Shutdown`] is received.
///
/// This function should be spawned on a tokio task via
/// [`RenderHandle::new`](super::reporter::RenderHandle::new).
pub(super) async fn render_loop(mut receiver: UnboundedReceiver<RenderEvent>, mode: RenderMode) {
    let mut state = RenderState::new(mode);
    state.write_banner();

    while let Some(event) = receiver.recv().await {
        if matches!(event, RenderEvent::Shutdown) {
            break;
        }
        state.handle_event(event);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The render loop shuts down cleanly in Quiet mode.
    #[tokio::test]
    async fn render_loop_shutdown_quiet() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(render_loop(receiver, RenderMode::Quiet));
        drop(sender.send(RenderEvent::Shutdown));
        let result = handle.await;
        assert!(result.is_ok());
    }

    /// The render loop processes multiple events before shutdown.
    #[tokio::test]
    async fn render_loop_processes_events_before_shutdown() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(render_loop(receiver, RenderMode::Quiet));

        drop(sender.send(RenderEvent::PhaseStart {
            label: "Testing".to_owned(),
        }));
        drop(sender.send(RenderEvent::PhaseComplete {
            detail: "Done".to_owned(),
            count_detail: None,
            elapsed: core::time::Duration::from_millis(10_u64),
        }));
        drop(sender.send(RenderEvent::Shutdown));

        let result = handle.await;
        assert!(result.is_ok());
    }
}
