//! Embedded pytest plugin for fest.
//!
//! This module embeds the `_fest_plugin.py` pytest plugin source code
//! as a compile-time string constant.  The plugin communicates with
//! the fest Rust process over a Unix domain socket using a
//! JSON-over-newline protocol, enabling in-process module patching
//! instead of spawning a fresh pytest process per mutant.

/// Embedded Python pytest plugin source code.
///
/// This constant contains the full source of `_fest_plugin.py`, which
/// is a pytest plugin that:
///
/// 1. Accepts a `--fest-socket` CLI option.
/// 2. Connects to a Unix domain socket on session start.
/// 3. Receives mutant descriptions as JSON, patches the target module, runs the relevant tests, and
///    sends results back.
///
/// The plugin is written to a temporary directory at runtime and
/// registered with pytest via `-p _fest_plugin`.
pub const FEST_PLUGIN_SOURCE: &str = include_str!("plugin/_fest_plugin.py");

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded plugin source is non-empty.
    #[test]
    fn plugin_source_is_non_empty() {
        assert!(!FEST_PLUGIN_SOURCE.is_empty());
    }

    /// The embedded plugin source contains the expected pytest hook.
    #[test]
    fn plugin_source_contains_pytest_addoption() {
        assert!(FEST_PLUGIN_SOURCE.contains("def pytest_addoption"));
    }

    /// The embedded plugin source contains the session start hook.
    #[test]
    fn plugin_source_contains_session_start_hook() {
        assert!(FEST_PLUGIN_SOURCE.contains("def pytest_sessionstart"));
    }

    /// The embedded plugin source contains the fest-socket option.
    #[test]
    fn plugin_source_contains_socket_option() {
        assert!(FEST_PLUGIN_SOURCE.contains("--fest-socket"));
    }

    /// The embedded plugin source contains the ready message.
    #[test]
    fn plugin_source_contains_ready_message() {
        assert!(FEST_PLUGIN_SOURCE.contains(r#""type": "ready""#));
    }

    /// The embedded plugin source contains shutdown handling.
    #[test]
    fn plugin_source_contains_shutdown_handling() {
        assert!(FEST_PLUGIN_SOURCE.contains("shutdown"));
    }
}
