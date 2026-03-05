//! Progress reporting for the mutation-testing pipeline.
//!
//! Rendering happens on a dedicated tokio task so worker threads never
//! block on I/O.  [`RenderHandle`] owns the task; [`ProgressReporter`]
//! is a cheaply cloneable sender wrapper shared by pipeline workers.

/// Render events sent from the pipeline to the render task.
pub(crate) mod event;
/// Async render loop that processes events and writes to stderr.
mod render;
/// Render handle, progress reporter, and mode resolution.
mod reporter;
/// Terminal styling and formatting utilities.
mod style;

pub use event::SummaryInfo;
pub use reporter::{ProgressReporter, RenderHandle, RenderMode, resolve_render_mode};
pub(crate) use style::styled_score;
