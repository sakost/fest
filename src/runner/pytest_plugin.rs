//! Pytest-plugin-based mutant runner.
//!
//! [`PytestPluginRunner`] is the primary (fast) backend: it spawns a
//! single pytest worker process that loads the embedded `_fest_plugin.py`
//! plugin and communicates over a Unix domain socket using a
//! JSON-over-newline protocol.
//!
//! For each [`Runner::run_mutant`] call the runner:
//! 1. Creates a temporary Unix socket and writes the embedded plugin file.
//! 2. Spawns `python -m pytest -p _fest_plugin --fest-socket <path>`.
//! 3. Accepts the connection, waits for the `READY` message.
//! 4. Sends a `MUTANT` message with the mutated source and test IDs.
//! 5. Reads back the `RESULT` and maps it to a [`MutantResult`].
//! 6. Sends `SHUTDOWN` and cleans up.

use core::time::Duration;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
    process::Command,
};

use crate::{
    Error,
    mutation::{Mutant, MutantResult, MutantStatus},
    plugin::FEST_PLUGIN_SOURCE,
    runner::Runner,
};

/// Default timeout in seconds when none is specified.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Name of the plugin file written to the temp directory.
const PLUGIN_FILENAME: &str = "_fest_plugin.py";

/// Configuration for the pytest plugin runner.
///
/// Holds tunable parameters such as the per-mutant timeout.
#[derive(Debug, Clone)]
pub struct PytestPluginRunner {
    /// Maximum wall-clock time for a single mutant run before it is
    /// considered timed out.
    timeout: Duration,
}

impl PytestPluginRunner {
    /// Create a new [`PytestPluginRunner`] with the given timeout.
    #[inline]
    #[must_use]
    pub const fn new(timeout_secs: u64) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    /// Inner implementation that performs the actual socket-based communication.
    ///
    /// Separated from [`run_mutant`](Runner::run_mutant) so that the
    /// timeout wrapper can catch both spawn failures and protocol errors.
    async fn run_mutant_inner(
        &self,
        mutant: &Mutant,
        source: &str,
        tests: &[String],
    ) -> Result<MutantStatus, Error> {
        // 1. Create a temp directory for the plugin and socket.
        let temp_dir = tempfile::tempdir()
            .map_err(|err| Error::Runner(format!("failed to create temp dir: {err}")))?;

        // 2. Write the embedded plugin file.
        let plugin_path = temp_dir.path().join(PLUGIN_FILENAME);
        std::fs::write(&plugin_path, FEST_PLUGIN_SOURCE).map_err(|err| {
            Error::Runner(format!(
                "failed to write plugin to {}: {err}",
                plugin_path.display()
            ))
        })?;

        // 3. Create the Unix socket.
        let socket_path = temp_dir.path().join("fest.sock");
        let listener = UnixListener::bind(&socket_path).map_err(|err| {
            Error::Runner(format!(
                "failed to bind Unix socket at {}: {err}",
                socket_path.display()
            ))
        })?;

        // 4. Build PYTHONPATH so pytest can import the plugin.
        let python_path = super::build_python_path(temp_dir.path());

        // 5. Spawn pytest with the plugin.
        let socket_path_str = socket_path.display().to_string();
        let mut child = Command::new("python")
            .args([
                "-m",
                "pytest",
                "-p",
                "_fest_plugin",
                "--fest-socket",
                &socket_path_str,
                "-x",
                "--no-header",
                "-q",
            ])
            .args(tests)
            .env("PYTHONPATH", &python_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|err| Error::Runner(format!("failed to spawn pytest: {err}")))?;

        // 6. Accept the connection from the plugin.
        let (stream, _addr) = listener.accept().await.map_err(|err| {
            Error::Runner(format!("failed to accept connection on socket: {err}"))
        })?;

        let (reader, mut writer) = tokio::io::split(stream);
        let mut buf_reader = BufReader::new(reader);

        // 7. Wait for the READY message.
        let ready_msg = read_message(&mut buf_reader).await?;
        let ready_type = extract_type(&ready_msg)?;
        if ready_type != "ready" {
            return Err(Error::Runner(format!(
                "expected 'ready' message, got '{ready_type}'"
            )));
        }

        // 8. Apply the mutation and send the MUTANT message.
        let mutated_source = mutant.apply_to_source(source);
        let mutant_msg = build_mutant_message(mutant, &mutated_source, tests);
        write_message(&mut writer, &mutant_msg).await?;

        // 9. Read back the RESULT.
        let result_msg = read_message(&mut buf_reader).await?;
        let status = parse_result_status(&result_msg)?;

        // 10. Send SHUTDOWN.
        let shutdown_msg = r#"{"type":"shutdown"}"#;
        // Best-effort shutdown -- ignore write errors.
        let _shutdown_result = write_message(&mut writer, shutdown_msg).await;

        // 11. Wait for the child process to exit (best-effort).
        let _wait_result = child.wait().await;

        Ok(status)
    }
}

impl Default for PytestPluginRunner {
    #[inline]
    fn default() -> Self {
        Self::new(DEFAULT_TIMEOUT_SECS)
    }
}

impl Runner for PytestPluginRunner {
    /// Run pytest against a single mutant via the plugin backend.
    ///
    /// 1. Write the embedded plugin to a temp directory.
    /// 2. Create a Unix socket in the temp directory.
    /// 3. Spawn pytest with the plugin and socket path.
    /// 4. Accept the connection and wait for `READY`.
    /// 5. Send a `MUTANT` message, read back the `RESULT`.
    /// 6. Send `SHUTDOWN` and clean up.
    #[inline]
    async fn run_mutant(
        &self,
        mutant: &Mutant,
        source: &str,
        tests: &[String],
    ) -> Result<MutantResult, Error> {
        let start = tokio::time::Instant::now();
        let tests_run: Vec<String> = tests.iter().map(ToString::to_string).collect();

        let outcome = tokio::time::timeout(self.timeout, async {
            self.run_mutant_inner(mutant, source, tests).await
        })
        .await;

        let elapsed = start.elapsed();

        let status = match outcome {
            Err(_elapsed) => MutantStatus::Timeout,
            Ok(Err(err)) => MutantStatus::Error(err.to_string()),
            Ok(Ok(run_status)) => run_status,
        };

        Ok(MutantResult {
            mutant: mutant.clone(),
            status,
            tests_run,
            duration: elapsed,
        })
    }
}

/// Read a single newline-delimited JSON message from the reader.
///
/// # Errors
///
/// Returns [`Error::Runner`] if the stream is closed or reading fails.
async fn read_message<R>(reader: &mut BufReader<R>) -> Result<String, Error>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    let bytes_read = reader
        .read_line(&mut line)
        .await
        .map_err(|err| Error::Runner(format!("failed to read from socket: {err}")))?;

    if bytes_read == 0_usize {
        return Err(Error::Runner(
            "connection closed before message received".to_owned(),
        ));
    }

    Ok(line)
}

/// Write a JSON message followed by a newline to the writer.
///
/// # Errors
///
/// Returns [`Error::Runner`] if writing fails.
async fn write_message<W>(writer: &mut W, msg: &str) -> Result<(), Error>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let data = if msg.ends_with('\n') {
        msg.to_owned()
    } else {
        let mut owned = msg.to_owned();
        owned.push('\n');
        owned
    };

    writer
        .write_all(data.as_bytes())
        .await
        .map_err(|err| Error::Runner(format!("failed to write to socket: {err}")))?;

    writer
        .flush()
        .await
        .map_err(|err| Error::Runner(format!("failed to flush socket: {err}")))?;

    Ok(())
}

/// Extract the `"type"` field from a JSON message string.
///
/// # Errors
///
/// Returns [`Error::Runner`] if the JSON is invalid or lacks a `"type"` field.
fn extract_type(msg: &str) -> Result<String, Error> {
    let value: serde_json::Value = serde_json::from_str(msg)
        .map_err(|err| Error::Runner(format!("invalid JSON from plugin: {err}")))?;

    value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::Runner("message missing 'type' field".to_owned()))
}

/// Build the JSON `MUTANT` message to send to the plugin.
fn build_mutant_message(mutant: &Mutant, mutated_source: &str, tests: &[String]) -> String {
    let file_path_str = mutant.file_path.display().to_string();
    let msg = serde_json::json!({
        "type": "mutant",
        "file": file_path_str,
        "module": file_to_module(&file_path_str),
        "mutated_source": mutated_source,
        "tests": tests,
    });
    msg.to_string()
}

/// Parse the `"status"` field from a result JSON message into a
/// [`MutantStatus`].
///
/// # Errors
///
/// Returns [`Error::Runner`] if the JSON is invalid or the status is
/// unrecognised.
fn parse_result_status(msg: &str) -> Result<MutantStatus, Error> {
    let value: serde_json::Value = serde_json::from_str(msg)
        .map_err(|err| Error::Runner(format!("invalid result JSON: {err}")))?;

    let msg_type = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    if msg_type != "result" {
        return Err(Error::Runner(format!(
            "expected 'result' message, got '{msg_type}'"
        )));
    }

    let status_str = value
        .get("status")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Error::Runner("result missing 'status' field".to_owned()))?;

    match status_str {
        "killed" => Ok(MutantStatus::Killed),
        "survived" => Ok(MutantStatus::Survived),
        "error" => {
            let error_message = value
                .get("error_message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown plugin error")
                .to_owned();
            Ok(MutantStatus::Error(error_message))
        }
        other => Err(Error::Runner(format!("unknown status: '{other}'"))),
    }
}

/// Convert a Python file path to a dotted module name.
///
/// For example, `src/calc.py` becomes `src.calc`.
fn file_to_module(file_path: &str) -> String {
    let name = file_path
        .strip_suffix(".py")
        .or_else(|| file_path.strip_suffix(".pyw"))
        .unwrap_or(file_path);

    name.replace(['/', '\\'], ".")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    /// Helper to create a simple test mutant.
    fn make_test_mutant() -> Mutant {
        Mutant {
            file_path: PathBuf::from("src/calc.py"),
            line: 1_u32,
            column: 7_u32,
            byte_offset: 6_usize,
            byte_length: 1_usize,
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            mutator_name: "arithmetic_op".to_owned(),
        }
    }

    /// `PytestPluginRunner::new` sets the timeout correctly.
    #[test]
    fn runner_timeout_configuration() {
        let runner = PytestPluginRunner::new(60_u64);
        assert_eq!(runner.timeout, Duration::from_secs(60_u64));
    }

    /// `PytestPluginRunner::default` uses the default timeout.
    #[test]
    fn runner_default_timeout() {
        let runner = PytestPluginRunner::default();
        assert_eq!(runner.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    }

    /// `file_to_module` converts simple paths correctly.
    #[test]
    fn file_to_module_simple_path() {
        assert_eq!(file_to_module("src/calc.py"), "src.calc");
    }

    /// `file_to_module` handles nested paths.
    #[test]
    fn file_to_module_nested_path() {
        assert_eq!(file_to_module("src/utils/helpers.py"), "src.utils.helpers");
    }

    /// `file_to_module` handles bare filename.
    #[test]
    fn file_to_module_bare_filename() {
        assert_eq!(file_to_module("app.py"), "app");
    }

    /// `file_to_module` handles `.pyw` extension.
    #[test]
    fn file_to_module_pyw_extension() {
        assert_eq!(file_to_module("gui/main.pyw"), "gui.main");
    }

    /// `file_to_module` handles path without Python extension.
    #[test]
    fn file_to_module_no_extension() {
        assert_eq!(file_to_module("some/path"), "some.path");
    }

    /// `file_to_module` handles Windows-style backslashes.
    #[test]
    fn file_to_module_backslash() {
        assert_eq!(file_to_module("src\\calc.py"), "src.calc");
    }

    /// `extract_type` parses the type field from valid JSON.
    #[test]
    fn extract_type_valid() {
        let msg = r#"{"type": "ready"}"#;
        let result = extract_type(msg);
        assert!(result.is_ok());
        assert_eq!(result.ok(), Some("ready".to_owned()));
    }

    /// `extract_type` returns error for missing type field.
    #[test]
    fn extract_type_missing() {
        let msg = r#"{"status": "ok"}"#;
        let result = extract_type(msg);
        assert!(result.is_err());
    }

    /// `extract_type` returns error for invalid JSON.
    #[test]
    fn extract_type_invalid_json() {
        let result = extract_type("not json");
        assert!(result.is_err());
    }

    /// `parse_result_status` handles "killed" status.
    #[test]
    fn parse_result_killed() {
        let msg = r#"{"type": "result", "status": "killed"}"#;
        let status = parse_result_status(msg);
        assert!(status.is_ok());
        assert_eq!(status.ok(), Some(MutantStatus::Killed));
    }

    /// `parse_result_status` handles "survived" status.
    #[test]
    fn parse_result_survived() {
        let msg = r#"{"type": "result", "status": "survived"}"#;
        let status = parse_result_status(msg);
        assert!(status.is_ok());
        assert_eq!(status.ok(), Some(MutantStatus::Survived));
    }

    /// `parse_result_status` handles "error" status with message.
    #[test]
    fn parse_result_error_with_message() {
        let msg = r#"{"type": "result", "status": "error", "error_message": "compile failed"}"#;
        let status = parse_result_status(msg);
        assert!(status.is_ok());
        assert_eq!(
            status.ok(),
            Some(MutantStatus::Error("compile failed".to_owned()))
        );
    }

    /// `parse_result_status` handles "error" without an error message.
    #[test]
    fn parse_result_error_without_message() {
        let msg = r#"{"type": "result", "status": "error"}"#;
        let status = parse_result_status(msg);
        assert!(status.is_ok());
        assert_eq!(
            status.ok(),
            Some(MutantStatus::Error("unknown plugin error".to_owned()))
        );
    }

    /// `parse_result_status` returns error for unknown status.
    #[test]
    fn parse_result_unknown_status() {
        let msg = r#"{"type": "result", "status": "magic"}"#;
        let result = parse_result_status(msg);
        assert!(result.is_err());
    }

    /// `parse_result_status` returns error for wrong message type.
    #[test]
    fn parse_result_wrong_type() {
        let msg = r#"{"type": "ready"}"#;
        let result = parse_result_status(msg);
        assert!(result.is_err());
    }

    /// `parse_result_status` returns error for invalid JSON.
    #[test]
    fn parse_result_invalid_json() {
        let result = parse_result_status("{invalid");
        assert!(result.is_err());
    }

    /// `parse_result_status` returns error when status field is missing.
    #[test]
    fn parse_result_missing_status() {
        let msg = r#"{"type": "result"}"#;
        let result = parse_result_status(msg);
        assert!(result.is_err());
    }

    /// `build_mutant_message` produces valid JSON with expected fields.
    #[test]
    fn build_mutant_message_valid_json() {
        let mutant = make_test_mutant();
        let mutated = "x = a - b";
        let tests = vec!["test_calc.py::test_add".to_owned()];

        let msg = build_mutant_message(&mutant, mutated, &tests);
        let parsed: serde_json::Value = serde_json::from_str(&msg).expect("should be valid JSON");

        assert_eq!(
            parsed.get("type").and_then(|val| val.as_str()),
            Some("mutant")
        );
        assert_eq!(
            parsed.get("file").and_then(|val| val.as_str()),
            Some("src/calc.py")
        );
        assert_eq!(
            parsed.get("module").and_then(|val| val.as_str()),
            Some("src.calc")
        );
        assert_eq!(
            parsed.get("mutated_source").and_then(|val| val.as_str()),
            Some("x = a - b")
        );
        assert!(parsed.get("tests").and_then(|val| val.as_array()).is_some());
    }

    /// `build_mutant_message` includes all test IDs.
    #[test]
    fn build_mutant_message_includes_tests() {
        let mutant = make_test_mutant();
        let tests = vec![
            "test_a.py::test_one".to_owned(),
            "test_b.py::test_two".to_owned(),
        ];

        let msg = build_mutant_message(&mutant, "x = a - b", &tests);
        let parsed: serde_json::Value = serde_json::from_str(&msg).expect("should be valid JSON");

        let test_array = parsed
            .get("tests")
            .and_then(|val| val.as_array())
            .expect("should have tests array");
        assert_eq!(test_array.len(), 2_usize);
    }

    /// `build_python_path` prepends the directory.
    #[test]
    fn python_path_construction() {
        let dir = Path::new("/tmp/fest_plugin");
        let result = super::super::build_python_path(dir);
        assert!(result.starts_with("/tmp/fest_plugin"));
    }

    /// The plugin file is successfully written to a temp directory.
    #[test]
    fn plugin_file_written_to_temp_dir() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let plugin_path = temp_dir.path().join(PLUGIN_FILENAME);
        std::fs::write(&plugin_path, FEST_PLUGIN_SOURCE).expect("write plugin");

        let contents = std::fs::read_to_string(&plugin_path).expect("read plugin");
        assert_eq!(contents, FEST_PLUGIN_SOURCE);
    }

    /// The runner can be constructed and has the correct timeout.
    #[test]
    fn runner_construction() {
        let runner = PytestPluginRunner::new(45_u64);
        assert_eq!(runner.timeout, Duration::from_secs(45_u64));
    }

    /// Timeout with zero seconds produces a very short timeout.
    #[test]
    fn runner_zero_timeout() {
        let runner = PytestPluginRunner::new(0_u64);
        assert_eq!(runner.timeout, Duration::from_secs(0_u64));
    }

    /// `read_message` returns error on empty input.
    #[tokio::test]
    async fn read_message_empty_stream() {
        let data: &[u8] = b"";
        let mut reader = BufReader::new(data);
        let result = read_message(&mut reader).await;
        assert!(result.is_err());
    }

    /// `read_message` reads a full line.
    #[tokio::test]
    async fn read_message_valid_line() {
        let data = b"{\"type\":\"ready\"}\n";
        let mut reader = BufReader::new(&data[..]);
        let result = read_message(&mut reader).await;
        assert!(result.is_ok());
        let line = result.expect("should read line");
        assert!(line.contains("ready"));
    }

    /// `write_message` appends newline if missing.
    #[tokio::test]
    async fn write_message_appends_newline() {
        let mut buf: Vec<u8> = Vec::new();
        let result = write_message(&mut buf, r#"{"type":"shutdown"}"#).await;
        assert!(result.is_ok());
        let written = String::from_utf8(buf).expect("should be utf8");
        assert!(written.ends_with('\n'));
    }

    /// `write_message` does not double newline.
    #[tokio::test]
    async fn write_message_no_double_newline() {
        let mut buf: Vec<u8> = Vec::new();
        let result = write_message(&mut buf, "{\"type\":\"shutdown\"}\n").await;
        assert!(result.is_ok());
        let written = String::from_utf8(buf).expect("should be utf8");
        assert!(written.ends_with("}\n"));
        assert!(!written.ends_with("}\n\n"));
    }

    /// A timeout of zero seconds causes the runner to time out.
    #[tokio::test]
    async fn timeout_produces_timeout_status() {
        let runner = PytestPluginRunner::new(0_u64);
        let mutant = make_test_mutant();
        let source = "x = a + b";
        let tests = vec!["test_calc.py::test_add".to_owned()];

        let result = runner
            .run_mutant(&mutant, source, &tests)
            .await
            .expect("should not return Err");

        // With a zero timeout, the operation should time out or error.
        assert!(
            result.status == MutantStatus::Timeout
                || matches!(result.status, MutantStatus::Error(_)),
            "expected Timeout or Error, got {:?}",
            result.status,
        );
    }

    /// The result includes the correct `tests_run` list.
    #[tokio::test]
    async fn result_contains_tests_run() {
        let runner = PytestPluginRunner::new(0_u64);
        let mutant = make_test_mutant();
        let source = "x = a + b";
        let tests = vec![
            "test_a.py::test_add".to_owned(),
            "test_b.py::test_sub".to_owned(),
        ];

        let result = runner
            .run_mutant(&mutant, source, &tests)
            .await
            .expect("should not return Err");

        assert_eq!(result.tests_run.len(), 2_usize);
        assert_eq!(result.tests_run[0_usize], "test_a.py::test_add");
        assert_eq!(result.tests_run[1_usize], "test_b.py::test_sub");
    }

    /// The result mutant matches the input mutant.
    #[tokio::test]
    async fn result_mutant_matches_input() {
        let runner = PytestPluginRunner::new(0_u64);
        let mutant = make_test_mutant();
        let source = "x = a + b";
        let tests: Vec<String> = Vec::new();

        let result = runner
            .run_mutant(&mutant, source, &tests)
            .await
            .expect("should not return Err");

        assert_eq!(result.mutant, mutant);
    }

    /// End-to-end socket protocol test: simulates the plugin side.
    #[tokio::test]
    async fn socket_protocol_end_to_end() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let socket_path = temp_dir.path().join("test.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind socket");

        // Spawn a mock plugin that sends READY, reads MUTANT, sends RESULT.
        let socket_path_clone = socket_path.clone();
        let mock_handle = tokio::spawn(async move {
            let stream = tokio::net::UnixStream::connect(&socket_path_clone)
                .await
                .expect("connect");
            let (reader, mut writer) = tokio::io::split(stream);
            let mut buf_reader = BufReader::new(reader);

            // Send READY
            writer
                .write_all(b"{\"type\":\"ready\"}\n")
                .await
                .expect("write ready");
            writer.flush().await.expect("flush ready");

            // Read MUTANT
            let mut line = String::new();
            let _bytes = buf_reader.read_line(&mut line).await.expect("read mutant");
            let parsed: serde_json::Value = serde_json::from_str(&line).expect("parse mutant json");
            assert_eq!(
                parsed.get("type").and_then(|val| val.as_str()),
                Some("mutant")
            );

            // Send RESULT
            writer
                .write_all(b"{\"type\":\"result\",\"status\":\"killed\"}\n")
                .await
                .expect("write result");
            writer.flush().await.expect("flush result");

            // Read SHUTDOWN
            let mut shutdown_line = String::new();
            let _bytes = buf_reader
                .read_line(&mut shutdown_line)
                .await
                .expect("read shutdown");
        });

        // Accept connection from the mock.
        let (stream, _addr) = listener.accept().await.expect("accept");
        let (reader, mut writer) = tokio::io::split(stream);
        let mut buf_reader = BufReader::new(reader);

        // Read READY.
        let ready = read_message(&mut buf_reader).await.expect("read ready");
        assert_eq!(extract_type(&ready).expect("type"), "ready");

        // Send MUTANT.
        let mutant = make_test_mutant();
        let msg = build_mutant_message(&mutant, "x = a - b", &["test.py::test_x".to_owned()]);
        write_message(&mut writer, &msg)
            .await
            .expect("write mutant");

        // Read RESULT.
        let result = read_message(&mut buf_reader).await.expect("read result");
        let status = parse_result_status(&result).expect("parse result");
        assert_eq!(status, MutantStatus::Killed);

        // Send SHUTDOWN.
        write_message(&mut writer, r#"{"type":"shutdown"}"#)
            .await
            .expect("write shutdown");

        // Wait for mock to finish.
        mock_handle.await.expect("mock should finish");
    }
}
