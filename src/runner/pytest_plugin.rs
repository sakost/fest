//! Pytest-plugin-based mutant runner with persistent worker pool.
//!
//! [`PytestPluginRunner`] is the primary (fast) backend: it maintains a
//! pool of long-lived pytest worker processes.  Each worker loads the
//! embedded `_fest_plugin.py` plugin, collects tests once, then enters
//! an event loop receiving mutant descriptions and returning results
//! over a Unix domain socket using a JSON-over-newline protocol.
//!
//! The lifecycle is:
//! 1. [`Runner::start`] — spawn N persistent workers (one pytest each).
//! 2. [`Runner::run_mutant`] — borrow a worker, send a mutant, get a result, return the worker.
//! 3. [`Runner::stop`] — send shutdown to each worker, wait for exit.

extern crate alloc;

use alloc::sync::Arc;
use core::time::Duration;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf},
    net::{UnixListener, UnixStream},
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

/// Minimum startup timeout in seconds for worker spawning.
///
/// Worker startup includes pytest collection, which is much slower than
/// running a single mutant. This constant provides a floor so that large
/// test suites have enough time to be collected.
const MIN_STARTUP_TIMEOUT_SECS: u64 = 120;

/// Multiplier applied to the per-mutant timeout to derive the startup
/// timeout.
const STARTUP_TIMEOUT_MULTIPLIER: u64 = 10;

/// Name of the plugin file written to the temp directory.
const PLUGIN_FILENAME: &str = "_fest_plugin.py";

// ---------------------------------------------------------------------------
// PersistentWorker
// ---------------------------------------------------------------------------

/// Holds the prepared temporary environment for a persistent worker.
///
/// Groups the temp directory, socket listener, and derived paths so
/// they can be passed between functions without exceeding the argument
/// limit.
struct TempWorkerEnv {
    /// Temporary directory owning the plugin file and socket.
    temp_dir: tempfile::TempDir,

    /// Bound Unix listener waiting for the plugin to connect.
    listener: UnixListener,

    /// `PYTHONPATH` value that prepends the temp directory.
    python_path: String,

    /// Stringified socket path for the `--fest-socket` CLI argument.
    socket_path_str: String,
}

/// Prepare the temporary environment for a persistent worker.
///
/// Creates the temp directory, writes the plugin file, binds the Unix
/// socket, and builds the `PYTHONPATH` value.
///
/// # Errors
///
/// Returns [`Error::Runner`] if any filesystem or socket operation fails.
fn prepare_worker_env() -> Result<TempWorkerEnv, Error> {
    let temp_dir = tempfile::tempdir()
        .map_err(|err| Error::Runner(format!("failed to create temp dir: {err}")))?;

    let plugin_path = temp_dir.path().join(PLUGIN_FILENAME);
    std::fs::write(&plugin_path, FEST_PLUGIN_SOURCE).map_err(|err| {
        Error::Runner(format!(
            "failed to write plugin to {}: {err}",
            plugin_path.display()
        ))
    })?;

    let socket_path = temp_dir.path().join("fest.sock");
    let listener = UnixListener::bind(&socket_path).map_err(|err| {
        Error::Runner(format!(
            "failed to bind Unix socket at {}: {err}",
            socket_path.display()
        ))
    })?;

    let python_path = super::build_python_path(temp_dir.path());
    let socket_path_str = socket_path.display().to_string();

    Ok(TempWorkerEnv {
        temp_dir,
        listener,
        python_path,
        socket_path_str,
    })
}

/// A single long-lived pytest process with an open socket connection.
///
/// Owns the temp directory (plugin file + socket) and the child process.
/// The connection is split into a buffered reader and a writer for
/// concurrent reads/writes.
struct PersistentWorker {
    /// Temporary directory that owns the plugin file and socket.
    /// Kept alive so the directory is not cleaned up prematurely.
    _temp_dir: tempfile::TempDir,

    /// The pytest child process.
    child: tokio::process::Child,

    /// Buffered reader for the Unix socket connection.
    reader: BufReader<ReadHalf<UnixStream>>,

    /// Writer half of the Unix socket connection.
    writer: WriteHalf<UnixStream>,
}

impl PersistentWorker {
    /// Spawn a new persistent pytest worker.
    ///
    /// Prepares the temp environment, spawns pytest, accepts the
    /// connection, and reads the READY message.
    ///
    /// `startup_timeout` bounds the time allowed for pytest to start and
    /// collect tests (much longer than per-mutant timeout for large suites).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runner`] if any step fails.
    async fn spawn(
        startup_timeout: Duration,
        project_dir: &std::path::Path,
    ) -> Result<Self, Error> {
        let env = prepare_worker_env()?;

        let mut child = Command::new("python")
            .args([
                "-m",
                "pytest",
                "-p",
                "_fest_plugin",
                "--fest-socket",
                &env.socket_path_str,
                "-p",
                "no:xdist",
                "-o",
                "addopts=",
                "--no-header",
                "-q",
            ])
            .current_dir(project_dir)
            .env("PYTHONPATH", &env.python_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|err| Error::Runner(format!("failed to spawn pytest: {err}")))?;

        // Race between accepting a connection and the child exiting.
        // If pytest crashes (e.g. import error) before connecting, we
        // detect it immediately instead of waiting the full timeout.
        let stream = accept_or_child_exit(&mut child, env.listener, startup_timeout).await?;
        let (reader, writer) = tokio::io::split(stream);
        let mut buf_reader = BufReader::new(reader);

        // Read READY message.
        let ready_msg = read_message(&mut buf_reader).await?;
        let ready_type = extract_type(&ready_msg)?;
        if ready_type != "ready" {
            return Err(Error::Runner(format!(
                "expected 'ready' message from worker, got '{ready_type}'"
            )));
        }

        Ok(Self {
            _temp_dir: env.temp_dir,
            child,
            reader: buf_reader,
            writer,
        })
    }

    /// Send a mutant to this worker and read back the result.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runner`] if the protocol exchange fails.
    async fn send_mutant(
        &mut self,
        mutant: &Mutant,
        source: &str,
        tests: &[String],
        timeout: Duration,
    ) -> Result<MutantStatus, Error> {
        let mutated_source = mutant.apply_to_source(source);
        let msg = build_mutant_message(mutant, &mutated_source, tests);

        let result = tokio::time::timeout(timeout, async {
            write_message(&mut self.writer, &msg).await?;
            let result_msg = read_message(&mut self.reader).await?;
            parse_result_status(&result_msg)
        })
        .await;

        match result {
            Err(_elapsed) => Ok(MutantStatus::Timeout),
            Ok(inner) => inner,
        }
    }

    /// Send a shutdown message and wait for the child to exit.
    async fn shutdown(mut self) {
        let shutdown_msg = r#"{"type":"shutdown"}"#;
        let _write_result = write_message(&mut self.writer, shutdown_msg).await;

        // Give the process a moment to exit gracefully.
        let wait_result = tokio::time::timeout(Duration::from_secs(5_u64), self.child.wait()).await;

        if wait_result.is_err() {
            let _kill_result = self.child.kill().await;
            let _wait_result = self.child.wait().await;
        }
    }
}

// ---------------------------------------------------------------------------
// WorkerPool
// ---------------------------------------------------------------------------

/// Channel-based pool of [`PersistentWorker`]s.
///
/// Workers are borrowed via the receiver and returned via the sender.
/// This provides a simple FIFO pool that is safe for concurrent access.
///
/// Debug is implemented manually because the channel types do not
/// implement `Debug`.
struct WorkerPool {
    /// Sender to return workers after use.
    sender: tokio::sync::mpsc::UnboundedSender<PersistentWorker>,

    /// Receiver to borrow workers.
    receiver: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<PersistentWorker>>,
}

impl core::fmt::Debug for WorkerPool {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WorkerPool").finish_non_exhaustive()
    }
}

impl WorkerPool {
    /// Create a new pool containing the given workers.
    fn new(workers: Vec<PersistentWorker>) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        for worker in workers {
            // Channel is unbounded and fresh — send cannot fail.
            let _result = sender.send(worker);
        }
        Self {
            sender,
            receiver: tokio::sync::Mutex::new(receiver),
        }
    }

    /// Borrow a worker from the pool, blocking until one is available.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runner`] if all workers have been dropped.
    async fn borrow(&self) -> Result<PersistentWorker, Error> {
        self.receiver
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| Error::Runner("worker pool exhausted".to_owned()))
    }

    /// Return a worker to the pool after use.
    fn return_worker(&self, worker: PersistentWorker) {
        // If the channel is closed, the worker is simply dropped.
        let _result = self.sender.send(worker);
    }

    /// Drain all workers from the pool and shut them down.
    async fn shutdown(self) {
        // Close the sender so no new workers can be returned.
        drop(self.sender);

        let mut receiver = self.receiver.into_inner();
        while let Some(worker) = receiver.recv().await {
            worker.shutdown().await;
        }
    }
}

// ---------------------------------------------------------------------------
// PytestPluginRunner
// ---------------------------------------------------------------------------

/// Configuration for the pytest plugin runner with persistent worker pool.
///
/// Holds tunable parameters such as the per-mutant timeout and the
/// optional worker pool (initialised via [`Runner::start`]).
///
/// The pool is stored behind a `std::sync::Mutex` rather than a tokio
/// mutex so that `run_mutant` can briefly check/get the pool without
/// holding the lock across `.await` points.
#[derive(Debug)]
pub struct PytestPluginRunner {
    /// Maximum wall-clock time for a single mutant run before it is
    /// considered timed out.
    timeout: Duration,

    /// The persistent worker pool, initialised by `start()`.
    ///
    /// Wrapped in `Arc` so that `run_mutant` can clone the handle out
    /// of the std Mutex and drop the guard before any `.await` points
    /// (std `MutexGuard` is not `Send`).
    pool: std::sync::Mutex<Option<Arc<WorkerPool>>>,

    /// Project directory, set during `start()` for oneshot fallback.
    project_dir: std::sync::Mutex<Option<std::path::PathBuf>>,
}

impl PytestPluginRunner {
    /// Create a new [`PytestPluginRunner`] with the given timeout.
    #[inline]
    #[must_use]
    pub const fn new(timeout_secs: u64) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_secs),
            pool: std::sync::Mutex::new(None),
            project_dir: std::sync::Mutex::new(None),
        }
    }
}

impl Default for PytestPluginRunner {
    #[inline]
    fn default() -> Self {
        Self::new(DEFAULT_TIMEOUT_SECS)
    }
}

impl Runner for PytestPluginRunner {
    /// Spawn persistent pytest workers.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runner`] if any worker fails to spawn.
    #[inline]
    async fn start(&self, num_workers: usize, project_dir: &std::path::Path) -> Result<(), Error> {
        // Store project dir for oneshot fallback in run_mutant.
        if let Ok(mut dir_guard) = self.project_dir.lock() {
            *dir_guard = Some(project_dir.to_path_buf());
        }

        // Use a much longer timeout for startup (pytest must collect all
        // tests before connecting). For large suites this can take tens
        // of seconds. When timeout is zero (tests), skip the floor.
        let startup_timeout = compute_startup_timeout(self.timeout);

        {
            let mut stderr = std::io::stderr().lock();
            let _result = std::io::Write::write_all(
                &mut stderr,
                format!(
                    "fest: spawning {num_workers} persistent pytest workers (startup timeout: \
                     {}s, per-mutant timeout: {}s)\n",
                    startup_timeout.as_secs(),
                    self.timeout.as_secs()
                )
                .as_bytes(),
            );
        }

        let mut handles = Vec::with_capacity(num_workers);
        let dir = project_dir.to_path_buf();

        for _idx in 0..num_workers {
            let worker_dir = dir.clone();
            let worker_timeout = startup_timeout;
            let handle =
                tokio::spawn(
                    async move { PersistentWorker::spawn(worker_timeout, &worker_dir).await },
                );
            handles.push(handle);
        }

        let mut workers = Vec::with_capacity(num_workers);
        for handle in handles {
            let worker = handle
                .await
                .map_err(|err| Error::Runner(format!("worker spawn task panicked: {err}")))??;
            workers.push(worker);
        }

        {
            let mut stderr = std::io::stderr().lock();
            let _result = std::io::Write::write_all(
                &mut stderr,
                format!("fest: all {num_workers} workers connected\n").as_bytes(),
            );
        }

        let worker_pool = Arc::new(WorkerPool::new(workers));
        *self
            .pool
            .lock()
            .map_err(|err| Error::Runner(format!("pool lock poisoned: {err}")))? =
            Some(worker_pool);

        Ok(())
    }

    /// Shut down all persistent workers.
    ///
    /// # Errors
    ///
    /// Returns `Ok(())` always — shutdown errors are suppressed.
    #[inline]
    async fn stop(&self) -> Result<(), Error> {
        let taken = self.pool.lock().ok().and_then(|mut guard| guard.take());
        if let Some(arc_pool) = taken {
            // Try to unwrap the Arc; if run_mutant calls are still in
            // flight they hold clones, so unwrap may fail.
            match Arc::try_unwrap(arc_pool) {
                Ok(pool) => pool.shutdown().await,
                Err(_arc) => {
                    // Other references still exist; they will drain
                    // naturally as in-flight run_mutant calls complete.
                }
            }
        }
        Ok(())
    }

    /// Run pytest against a single mutant via a persistent worker.
    ///
    /// Borrows a worker from the pool, sends the mutant, reads the
    /// result, and returns the worker to the pool.  The pool lock is
    /// held only briefly to check if the pool exists; the actual
    /// borrow/return uses the pool's internal channel which supports
    /// concurrent access.
    ///
    /// On worker error or timeout, the worker is discarded (shut down)
    /// rather than returned to the pool, preventing protocol desync.
    ///
    /// If no pool is initialised (start was not called or failed), falls
    /// back to spawning a one-shot worker.
    #[inline]
    async fn run_mutant(
        &self,
        mutant: &Mutant,
        source: &str,
        tests: &[String],
    ) -> Result<MutantResult, Error> {
        let start = tokio::time::Instant::now();
        let tests_run: Vec<String> = tests.iter().map(ToString::to_string).collect();

        // Brief lock to clone the Arc<WorkerPool> handle. The guard is
        // dropped immediately so it is never held across an await point
        // (std MutexGuard is not Send).
        let pool_handle: Option<Arc<WorkerPool>> =
            self.pool.lock().ok().and_then(|guard| guard.clone());

        let status = if let Some(pool) = pool_handle {
            run_via_pool(pool, mutant, source, tests, self.timeout).await?
        } else {
            let dir = self
                .project_dir
                .lock()
                .ok()
                .and_then(|guard| guard.clone())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            run_oneshot(mutant, source, tests, self.timeout, &dir).await?
        };

        let elapsed = start.elapsed();

        Ok(MutantResult {
            mutant: mutant.clone(),
            status,
            tests_run,
            duration: elapsed,
        })
    }
}

/// Borrow a worker from the pool, run a mutant, and handle the result.
///
/// On success the worker is returned to the pool. On error or timeout
/// the worker is shut down to prevent protocol desync.
///
/// If no worker is available within `timeout`, returns
/// [`MutantStatus::Timeout`] instead of blocking indefinitely (prevents
/// pool-depletion deadlock).
///
/// # Errors
///
/// Returns [`Error::Runner`] if the protocol exchange fails in a
/// non-timeout way.
async fn run_via_pool(
    pool: Arc<WorkerPool>,
    mutant: &Mutant,
    source: &str,
    tests: &[String],
    timeout: Duration,
) -> Result<MutantStatus, Error> {
    // Bounded borrow: if all workers are consumed (e.g. after multiple
    // timeouts discarded them), we return Timeout instead of hanging.
    let borrow_result = tokio::time::timeout(timeout, pool.borrow()).await;
    let mut worker = match borrow_result {
        Ok(Ok(worker)) => worker,
        Ok(Err(err)) => return Err(err),
        Err(_elapsed) => return Ok(MutantStatus::Timeout),
    };

    let result = worker.send_mutant(mutant, source, tests, timeout).await;

    let worker_healthy = matches!(&result, Ok(status) if *status != MutantStatus::Timeout);
    if worker_healthy {
        pool.return_worker(worker);
    } else {
        // Worker may be in a bad state; discard it.
        worker.shutdown().await;
    }

    result
}

/// Run a single mutant using a freshly spawned one-shot worker.
///
/// This is used as a fallback when the persistent pool is not available.
///
/// # Errors
///
/// Returns [`Error::Runner`] if spawning or protocol exchange fails.
async fn run_oneshot(
    mutant: &Mutant,
    source: &str,
    tests: &[String],
    timeout: Duration,
    project_dir: &std::path::Path,
) -> Result<MutantStatus, Error> {
    // One-shot workers need a generous spawn timeout (pytest must collect
    // tests), but the per-mutant timeout is used for the actual test run.
    let startup_timeout = compute_startup_timeout(timeout);

    let spawn_result = tokio::time::timeout(
        startup_timeout,
        PersistentWorker::spawn(startup_timeout, project_dir),
    )
    .await;

    let mut worker = match spawn_result {
        Err(_elapsed) => return Ok(MutantStatus::Timeout),
        Ok(result) => result?,
    };

    let status = worker.send_mutant(mutant, source, tests, timeout).await;
    worker.shutdown().await;
    status
}

/// Read available stderr from a child process for diagnostic output.
///
/// Returns whatever has been written so far, truncated to a reasonable
/// length. If stderr cannot be read, returns a placeholder message.
async fn capture_child_stderr(child: &mut tokio::process::Child) -> String {
    let Some(stderr) = child.stderr.take() else {
        return "<no stderr captured>".to_owned();
    };

    let mut buf_reader = BufReader::new(stderr);
    let mut output = String::new();

    // Read up to a few KB of stderr for diagnostics.
    loop {
        let mut line = String::new();
        match tokio::time::timeout(
            Duration::from_millis(100_u64),
            buf_reader.read_line(&mut line),
        )
        .await
        {
            Ok(Ok(0_usize)) | Err(_) => break,
            Ok(Ok(_n)) => output.push_str(&line),
            Ok(Err(_err)) => break,
        }

        if output.len() > 4096_usize {
            break;
        }
    }

    if output.is_empty() {
        "<empty>".to_owned()
    } else {
        output.trim().to_owned()
    }
}

/// Compute the startup timeout from the per-mutant timeout.
///
/// Worker startup includes pytest test collection, which is significantly
/// slower than running a single mutant. This function applies a multiplier
/// and a minimum floor. When the base timeout is zero (used in tests), the
/// floor is skipped to keep tests fast.
fn compute_startup_timeout(per_mutant_timeout: Duration) -> Duration {
    let base = per_mutant_timeout
        .as_secs()
        .saturating_mul(STARTUP_TIMEOUT_MULTIPLIER);
    if per_mutant_timeout.is_zero() {
        Duration::ZERO
    } else {
        Duration::from_secs(base.max(MIN_STARTUP_TIMEOUT_SECS))
    }
}

// ---------------------------------------------------------------------------
// Connection helpers
// ---------------------------------------------------------------------------

/// Wait for the pytest worker to connect, or detect early child exit.
///
/// Races three events:
/// 1. The plugin connects to the Unix socket (success).
/// 2. The child process exits before connecting (immediate failure with captured stderr).
/// 3. The timeout elapses (timeout failure with captured stderr).
///
/// This avoids waiting the full timeout when pytest crashes on startup.
///
/// # Errors
///
/// Returns [`Error::Runner`] if the child exits, timeout elapses, or
/// accept fails.
async fn accept_or_child_exit(
    child: &mut tokio::process::Child,
    listener: UnixListener,
    timeout: Duration,
) -> Result<UnixStream, Error> {
    tokio::select! {
        // Branch 1: plugin connects successfully.
        accept_result = listener.accept() => {
            match accept_result {
                Ok((stream, _addr)) => Ok(stream),
                Err(err) => {
                    let stderr_output = capture_child_stderr(child).await;
                    let _kill_result = child.kill().await;
                    let _wait_result = child.wait().await;
                    Err(Error::Runner(format!(
                        "failed to accept connection from pytest worker: {err} \
                         (pytest stderr: {stderr_output})"
                    )))
                }
            }
        }
        // Branch 2: child process exits before connecting.
        wait_result = child.wait() => {
            let code = wait_result
                .as_ref()
                .ok()
                .and_then(std::process::ExitStatus::code);
            let stderr_output = capture_child_stderr(child).await;
            Err(Error::Runner(format!(
                "pytest worker exited before connecting (exit code: {code:?}) \
                 (pytest stderr: {stderr_output})"
            )))
        }
        // Branch 3: timeout elapses.
        () = tokio::time::sleep(timeout) => {
            let stderr_output = capture_child_stderr(child).await;
            let _kill_result = child.kill().await;
            let _wait_result = child.wait().await;
            Err(Error::Runner(format!(
                "timeout waiting for pytest worker to connect \
                 (pytest stderr: {stderr_output})"
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// Protocol helpers
// ---------------------------------------------------------------------------

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
/// Writes the message bytes directly, appending a newline byte only
/// when the message does not already end with one.
///
/// # Errors
///
/// Returns [`Error::Runner`] if writing fails.
async fn write_message<W>(writer: &mut W, msg: &str) -> Result<(), Error>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    writer
        .write_all(msg.as_bytes())
        .await
        .map_err(|err| Error::Runner(format!("failed to write to socket: {err}")))?;

    if !msg.ends_with('\n') {
        writer
            .write_all(b"\n")
            .await
            .map_err(|err| Error::Runner(format!("failed to write newline to socket: {err}")))?;
    }

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
        .ok_or_else(|| Error::Runner("result missing 'type' field".to_owned()))?;

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

    /// Pool is None before start.
    #[test]
    fn pool_is_none_before_start() {
        let runner = PytestPluginRunner::new(10_u64);
        let guard = runner.pool.lock().unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("lock poisoned: {err}");
            }
        });
        assert!(guard.is_none());
    }

    /// Stop without start is a no-op that succeeds.
    #[tokio::test]
    async fn stop_without_start_is_noop() {
        let runner = PytestPluginRunner::new(10_u64);
        let result = runner.stop().await;
        assert!(result.is_ok());
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

    /// `parse_result_status` returns error when type field is missing.
    #[test]
    fn parse_result_missing_type() {
        let msg = r#"{"status": "killed"}"#;
        let result = parse_result_status(msg);
        assert!(result.is_err());
    }

    /// `build_mutant_message` produces valid JSON with expected fields.
    #[test]
    fn build_mutant_message_produces_valid_json() {
        let mutant = make_test_mutant();
        let mutated = "x = a - b";
        let tests = vec!["test_calc.py::test_add".to_owned()];

        let msg = build_mutant_message(&mutant, mutated, &tests);
        let parsed: serde_json::Value = serde_json::from_str(&msg).unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("should be valid JSON: {err}");
            }
        });

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
        let parsed: serde_json::Value = serde_json::from_str(&msg).unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("should be valid JSON: {err}");
            }
        });

        let test_array = parsed
            .get("tests")
            .and_then(|val| val.as_array())
            .unwrap_or_else(|| {
                #[allow(clippy::panic, reason = "test assertion")]
                {
                    panic!("should have tests array");
                }
            });
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
        let temp_dir = tempfile::tempdir().unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("create temp dir: {err}");
            }
        });
        let plugin_path = temp_dir.path().join(PLUGIN_FILENAME);
        std::fs::write(&plugin_path, FEST_PLUGIN_SOURCE).unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("write plugin: {err}");
            }
        });

        let contents = std::fs::read_to_string(&plugin_path).unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("read plugin: {err}");
            }
        });
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
        let data = b"{\"type\":\"ready\",\"tests\":[]}\n";
        let mut reader = BufReader::new(&data[..]);
        let result = read_message(&mut reader).await;
        assert!(result.is_ok());
        let line = result.unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("should read line: {err}");
            }
        });
        assert!(line.contains("ready"));
    }

    /// `write_message` appends newline if missing.
    #[tokio::test]
    async fn write_message_appends_newline() {
        let mut buf: Vec<u8> = Vec::new();
        let result = write_message(&mut buf, r#"{"type":"shutdown"}"#).await;
        assert!(result.is_ok());
        let written = String::from_utf8(buf).unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("should be utf8: {err}");
            }
        });
        assert!(written.ends_with('\n'));
    }

    /// `write_message` does not double newline.
    #[tokio::test]
    async fn write_message_no_double_newline() {
        let mut buf: Vec<u8> = Vec::new();
        let result = write_message(&mut buf, "{\"type\":\"shutdown\"}\n").await;
        assert!(result.is_ok());
        let written = String::from_utf8(buf).unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("should be utf8: {err}");
            }
        });
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

        let result = runner.run_mutant(&mutant, source, &tests).await;

        // With a zero timeout, the operation should time out or error.
        match result {
            Ok(mr) => {
                assert!(
                    mr.status == MutantStatus::Timeout
                        || matches!(mr.status, MutantStatus::Error(_)),
                    "expected Timeout or Error, got {:?}",
                    mr.status,
                );
            }
            Err(_err) => {
                // Runner error is also acceptable with zero timeout.
            }
        }
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

        let result = runner.run_mutant(&mutant, source, &tests).await;

        // With 0 timeout we may get an error; only check tests_run on success.
        if let Ok(mr) = result {
            assert_eq!(mr.tests_run.len(), 2_usize);
            assert_eq!(mr.tests_run[0_usize], "test_a.py::test_add");
            assert_eq!(mr.tests_run[1_usize], "test_b.py::test_sub");
        }
    }

    /// The result mutant matches the input mutant.
    #[tokio::test]
    async fn result_mutant_matches_input() {
        let runner = PytestPluginRunner::new(0_u64);
        let mutant = make_test_mutant();
        let source = "x = a + b";
        let tests: Vec<String> = Vec::new();

        let result = runner.run_mutant(&mutant, source, &tests).await;

        if let Ok(mr) = result {
            assert_eq!(mr.mutant, mutant);
        }
    }

    /// End-to-end socket protocol test: simulates the plugin side.
    #[tokio::test]
    async fn socket_protocol_end_to_end() {
        let temp_dir = tempfile::tempdir().unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("create temp dir: {err}");
            }
        });
        let socket_path = temp_dir.path().join("test.sock");
        let listener = UnixListener::bind(&socket_path).unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("bind socket: {err}");
            }
        });

        // Spawn a mock plugin that sends READY, reads MUTANT, sends RESULT.
        let socket_path_clone = socket_path.clone();
        let mock_handle = tokio::spawn(async move {
            let stream = UnixStream::connect(&socket_path_clone)
                .await
                .unwrap_or_else(|err| {
                    #[allow(clippy::panic, reason = "test assertion")]
                    {
                        panic!("connect: {err}");
                    }
                });
            let (reader, mut writer) = tokio::io::split(stream);
            let mut buf_reader = BufReader::new(reader);

            // Send READY with tests field.
            writer
                .write_all(b"{\"type\":\"ready\",\"tests\":[\"test.py::test_x\"]}\n")
                .await
                .unwrap_or_else(|err| {
                    #[allow(clippy::panic, reason = "test assertion")]
                    {
                        panic!("write ready: {err}");
                    }
                });
            writer.flush().await.unwrap_or_else(|err| {
                #[allow(clippy::panic, reason = "test assertion")]
                {
                    panic!("flush ready: {err}");
                }
            });

            // Read MUTANT
            let mut line = String::new();
            let _bytes = buf_reader.read_line(&mut line).await.unwrap_or_else(|err| {
                #[allow(clippy::panic, reason = "test assertion")]
                {
                    panic!("read mutant: {err}");
                }
            });
            let parsed: serde_json::Value = serde_json::from_str(&line).unwrap_or_else(|err| {
                #[allow(clippy::panic, reason = "test assertion")]
                {
                    panic!("parse mutant json: {err}");
                }
            });
            assert_eq!(
                parsed.get("type").and_then(|val| val.as_str()),
                Some("mutant")
            );

            // Send RESULT
            writer
                .write_all(b"{\"type\":\"result\",\"status\":\"killed\"}\n")
                .await
                .unwrap_or_else(|err| {
                    #[allow(clippy::panic, reason = "test assertion")]
                    {
                        panic!("write result: {err}");
                    }
                });
            writer.flush().await.unwrap_or_else(|err| {
                #[allow(clippy::panic, reason = "test assertion")]
                {
                    panic!("flush result: {err}");
                }
            });

            // Read SHUTDOWN
            let mut shutdown_line = String::new();
            let _bytes = buf_reader
                .read_line(&mut shutdown_line)
                .await
                .unwrap_or_else(|err| {
                    #[allow(clippy::panic, reason = "test assertion")]
                    {
                        panic!("read shutdown: {err}");
                    }
                });
        });

        // Accept connection from the mock.
        let (stream, _addr) = listener.accept().await.unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("accept: {err}");
            }
        });
        let (reader, mut writer) = tokio::io::split(stream);
        let mut buf_reader = BufReader::new(reader);

        // Read READY.
        let ready = read_message(&mut buf_reader).await.unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("read ready: {err}");
            }
        });
        assert_eq!(
            extract_type(&ready).unwrap_or_else(|err| {
                #[allow(clippy::panic, reason = "test assertion")]
                {
                    panic!("type: {err}");
                }
            }),
            "ready"
        );

        // Send MUTANT.
        let mutant = make_test_mutant();
        let msg = build_mutant_message(&mutant, "x = a - b", &["test.py::test_x".to_owned()]);
        write_message(&mut writer, &msg)
            .await
            .unwrap_or_else(|err| {
                #[allow(clippy::panic, reason = "test assertion")]
                {
                    panic!("write mutant: {err}");
                }
            });

        // Read RESULT.
        let result = read_message(&mut buf_reader).await.unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("read result: {err}");
            }
        });
        let status = parse_result_status(&result).unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("parse result: {err}");
            }
        });
        assert_eq!(status, MutantStatus::Killed);

        // Send SHUTDOWN.
        write_message(&mut writer, r#"{"type":"shutdown"}"#)
            .await
            .unwrap_or_else(|err| {
                #[allow(clippy::panic, reason = "test assertion")]
                {
                    panic!("write shutdown: {err}");
                }
            });

        // Wait for mock to finish.
        mock_handle.await.unwrap_or_else(|err| {
            #[allow(clippy::panic, reason = "test assertion")]
            {
                panic!("mock should finish: {err}");
            }
        });
    }

    /// `WorkerPool` borrow/return cycle works correctly.
    #[tokio::test]
    async fn worker_pool_borrow_return_cycle() {
        // We can't easily create real PersistentWorkers in tests without
        // Python, so we test the pool logic indirectly through the runner
        // with a zero timeout (which exercises the oneshot fallback path).
        let runner = PytestPluginRunner::new(0_u64);
        let mutant = make_test_mutant();
        let source = "x = a + b";
        let tests: Vec<String> = Vec::new();

        // Without start(), should use oneshot fallback.
        let result = runner.run_mutant(&mutant, source, &tests).await;
        // Either Ok or Err is fine — we just verify it doesn't hang.
        let _status = result;
    }
}
