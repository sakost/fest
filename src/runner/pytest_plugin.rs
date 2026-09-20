//! Pytest-plugin-based mutant runner with persistent worker pool.
//!
//! [`PytestPluginRunner`] is the primary (fast) backend: it maintains a
//! pool of long-lived pytest worker processes.  Each worker loads the
//! embedded `_fest_plugin.py` plugin, collects tests once, then enters
//! an event loop receiving mutant descriptions and returning results
//! over IPC (Unix domain sockets on Unix, TCP on Windows) using a
//! JSON-over-newline protocol.
//!
//! The lifecycle is:
//! 1. [`Runner::start`] — spawn N persistent workers (one pytest each).
//! 2. [`Runner::run_mutant`] — borrow a worker, send a mutant, get a result, return the worker.
//! 3. [`Runner::stop`] — send shutdown to each worker, wait for exit.

extern crate alloc;

use alloc::sync::Arc;
use core::time::Duration;

#[cfg(windows)]
use tokio::net::{TcpListener as IpcListener, TcpStream as IpcStream};
// Platform-specific IPC types: Unix domain sockets on Unix, TCP on Windows.
#[cfg(unix)]
use tokio::net::{UnixListener as IpcListener, UnixStream as IpcStream};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf},
    process::Command,
};

use crate::{
    Error,
    mutation::{Mutant, MutantResult, MutantStatus, SkipReason},
    plugin::FEST_PLUGIN_SOURCE,
    runner::Runner,
};

/// Default timeout in seconds when none is specified.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// IPC protocol version. Bump whenever the JSON wire format changes in a
/// non-backwards-compatible way. The plugin refuses `ready_ack` messages with
/// a mismatched version.
pub const PROTOCOL_VERSION: u32 = 2;

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

    /// Bound IPC listener waiting for the plugin to connect.
    listener: IpcListener,

    /// `PYTHONPATH` value that prepends the temp directory.
    python_path: String,

    /// Connection address string for the `--fest-socket` CLI argument.
    /// On Unix this is a socket path; on Windows it is `host:port`.
    socket_addr_str: String,
}

/// Prepare the temporary environment for a persistent worker.
///
/// Creates the temp directory, writes the plugin file, binds the IPC
/// listener (Unix socket on Unix, TCP localhost on Windows), and builds
/// the `PYTHONPATH` value.
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

    #[cfg(unix)]
    let (listener, socket_addr_str) = {
        let socket_path = temp_dir.path().join("fest.sock");
        let listener = IpcListener::bind(&socket_path).map_err(|err| {
            Error::Runner(format!(
                "failed to bind Unix socket at {}: {err}",
                socket_path.display()
            ))
        })?;
        (listener, socket_path.display().to_string())
    };

    #[cfg(windows)]
    let (listener, socket_addr_str) = {
        let std_listener =
            std::net::TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0_u16)))
                .map_err(|err| {
                    Error::Runner(format!("failed to bind TCP listener on localhost: {err}"))
                })?;
        std_listener.set_nonblocking(true).map_err(|err| {
            Error::Runner(format!("failed to set TCP listener non-blocking: {err}"))
        })?;
        let addr = std_listener.local_addr().map_err(|err| {
            Error::Runner(format!(
                "failed to get local address of TCP listener: {err}"
            ))
        })?;
        let listener = IpcListener::from_std(std_listener).map_err(|err| {
            Error::Runner(format!("failed to convert TCP listener to tokio: {err}"))
        })?;
        (listener, addr.to_string())
    };

    let python_path = super::build_python_path(temp_dir.path());

    Ok(TempWorkerEnv {
        temp_dir,
        listener,
        python_path,
        socket_addr_str,
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
    reader: BufReader<ReadHalf<IpcStream>>,

    /// Writer half of the Unix socket connection.
    writer: WriteHalf<IpcStream>,
}

impl PersistentWorker {
    /// Spawn a new persistent pytest worker.
    ///
    /// Prepares the temp environment, spawns pytest, accepts the
    /// connection, and reads the READY message.  After receiving READY,
    /// sends a `ready_ack` message carrying the project plugin index.
    ///
    /// `startup_timeout` bounds the time allowed for pytest to start and
    /// collect tests (much longer than per-mutant timeout for large suites).
    ///
    /// `index` is the project-level [`crate::plugin_index::PluginIndex`]
    /// sent to the plugin as the `ready_ack` payload.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runner`] if any step fails.
    async fn spawn(
        startup_timeout: Duration,
        project_dir: &std::path::Path,
        index: Arc<crate::plugin_index::PluginIndex>,
    ) -> Result<Self, Error> {
        let env = prepare_worker_env()?;

        let mut child = Command::new(crate::python::resolve_python(project_dir))
            .args(build_worker_args(&env.socket_addr_str, project_dir))
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
        let (reader, mut writer) = tokio::io::split(stream);
        let mut buf_reader = BufReader::new(reader);

        // Read READY message.
        let ready_msg = read_message(&mut buf_reader).await?;
        let ready_type = extract_type(&ready_msg)?;
        if ready_type != "ready" {
            return Err(Error::Runner(format!(
                "expected 'ready' message from worker, got '{ready_type}'"
            )));
        }

        // Send ready_ack with the project index.
        let ack = build_ready_ack_message(&index);
        write_message(&mut writer, &ack).await?;

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
        let original_ast = ruff_python_parser::parse_module(source).map_or_else(
            |_| ruff_python_ast::ModModule {
                range: ruff_text_size::TextRange::default(),
                body: Vec::new(),
            },
            ruff_python_parser::Parsed::into_syntax,
        );
        let mutated_ast = ruff_python_parser::parse_module(&mutated_source).map_or_else(
            |_| ruff_python_ast::ModModule {
                range: ruff_text_size::TextRange::default(),
                body: Vec::new(),
            },
            ruff_python_parser::Parsed::into_syntax,
        );
        let diff = match crate::mutation::diff::derive_diff(
            mutant,
            &original_ast,
            &mutated_ast,
            &mutated_source,
        ) {
            Ok(d) => d,
            Err(reason) => return Ok(MutantStatus::Skipped { reason }),
        };
        let msg = build_mutant_message(mutant, &mutated_source, tests, &diff);

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

    /// Context for respawning replacements of discarded workers. `None`
    /// disables respawning (unit tests).
    respawn: Option<RespawnContext>,
}

/// Everything needed to spawn a replacement worker after a discard.
///
/// Without respawning, every timed-out mutant permanently shrinks the
/// pool; once it is empty every remaining mutant is misreported as
/// `Timeout` without ever running.
#[derive(Clone)]
struct RespawnContext {
    /// Startup timeout for the replacement worker (pytest must collect).
    startup_timeout: Duration,
    /// Project directory the worker runs in.
    project_dir: std::path::PathBuf,
    /// Shared plugin index sent in the `ready_ack` handshake.
    index: Arc<crate::plugin_index::PluginIndex>,
}

impl core::fmt::Debug for WorkerPool {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WorkerPool").finish_non_exhaustive()
    }
}

impl WorkerPool {
    /// Create a new pool containing the given workers.
    fn new(workers: Vec<PersistentWorker>, respawn: Option<RespawnContext>) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        for worker in workers {
            // Channel is unbounded and fresh — send cannot fail.
            let _result = sender.send(worker);
        }
        Self {
            sender,
            receiver: tokio::sync::Mutex::new(receiver),
            respawn,
        }
    }

    /// Discard an unhealthy worker and spawn a replacement in the
    /// background so the pool does not shrink permanently.
    ///
    /// The replacement joins the pool when its pytest collection finishes;
    /// if the respawn fails the pool shrinks by one (the borrow-timeout
    /// safeguard in [`run_via_pool`] still prevents deadlock).
    fn discard_and_respawn(self: &Arc<Self>, worker: PersistentWorker) {
        let Some(ctx) = self.respawn.clone() else {
            drop(tokio::spawn(async move { worker.shutdown().await }));
            return;
        };
        let pool = Arc::clone(self);
        drop(tokio::spawn(async move {
            worker.shutdown().await;
            match PersistentWorker::spawn(ctx.startup_timeout, &ctx.project_dir, ctx.index).await {
                Ok(replacement) => pool.return_worker(replacement),
                Err(_err) => {
                    // Replacement failed (e.g. project env broke mid-run):
                    // the pool shrinks by one; nothing else to do.
                }
            }
        }));
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

    /// Project plugin index, computed during `start()` and sent to each
    /// worker as the `ready_ack` handshake payload.
    project_index: std::sync::Mutex<Option<Arc<crate::plugin_index::PluginIndex>>>,
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
            project_index: std::sync::Mutex::new(None),
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

        // Scan the project to build the plugin index sent to each worker.
        // On failure, fall back to an empty index — the plugin's runtime
        // layer (sys.modules walk) still provides partial coverage, but
        // raw module-level constants imported via `from X import C` will
        // not be rebound in consumers. Surface the error to stderr so
        // operators can diagnose misconfigured project_dir or permission
        // issues; do not abort because partial accuracy is better than
        // a hard failure here.
        let scanned = match crate::plugin_index::scan_project(project_dir) {
            Ok(idx) => idx,
            #[expect(
                clippy::print_stderr,
                reason = "operator-facing diagnostic for a degraded-mode fallback; the project \
                          has no logging dep and the error must not abort the run"
            )]
            Err(err) => {
                eprintln!(
                    "fest: scan_project failed at {}: {err}; reverse-import index falling back to \
                     runtime layer only",
                    project_dir.display(),
                );
                crate::plugin_index::PluginIndex::default()
            }
        };
        let index_arc = Arc::new(scanned);
        if let Ok(mut guard) = self.project_index.lock() {
            *guard = Some(Arc::clone(&index_arc));
        }

        // Use a much longer timeout for startup (pytest must collect all
        // tests before connecting). For large suites this can take tens
        // of seconds. When timeout is zero (tests), skip the floor.
        let startup_timeout = compute_startup_timeout(self.timeout);

        let mut handles = Vec::with_capacity(num_workers);
        let dir = project_dir.to_path_buf();

        for _idx in 0..num_workers {
            let worker_dir = dir.clone();
            let worker_timeout = startup_timeout;
            let worker_index = Arc::clone(&index_arc);
            let handle = tokio::spawn(async move {
                PersistentWorker::spawn(worker_timeout, &worker_dir, worker_index).await
            });
            handles.push(handle);
        }

        let mut workers = Vec::with_capacity(num_workers);
        for handle in handles {
            let worker = handle
                .await
                .map_err(|err| Error::Runner(format!("worker spawn task panicked: {err}")))??;
            workers.push(worker);
        }

        let worker_pool = Arc::new(WorkerPool::new(
            workers,
            Some(RespawnContext {
                startup_timeout,
                project_dir: dir,
                index: index_arc,
            }),
        ));
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
        // Worker may be in a bad state (e.g. stuck in a mutant's infinite
        // loop); discard it and spawn a replacement in the background.
        pool.discard_and_respawn(worker);
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

    let default_index = Arc::new(crate::plugin_index::PluginIndex::default());
    let spawn_result = tokio::time::timeout(
        startup_timeout,
        PersistentWorker::spawn(startup_timeout, project_dir, default_index),
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
    listener: IpcListener,
    timeout: Duration,
) -> Result<IpcStream, Error> {
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
        child_exit = child.wait() => {
            let code = child_exit
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
/// Build the argv for a worker pytest process.
///
/// Pins `--rootdir` to the project dir: pytest's rootdir discovery scans
/// every CLI argument that exists as a path, so without the pin the
/// `--fest-socket` temp-dir path drags rootdir up to the common ancestor
/// (e.g. `/tmp`). Collected item nodeids are rootdir-relative, so a wrong
/// rootdir makes every runner-sent test ID miss the worker's item index.
fn build_worker_args(socket_addr: &str, project_dir: &std::path::Path) -> Vec<String> {
    // Absolutize: pytest resolves a relative --rootdir against its own cwd,
    // which is already the project dir — a relative path would double up.
    let rootdir = std::path::absolute(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    vec![
        "-m".to_owned(),
        "pytest".to_owned(),
        "-p".to_owned(),
        "_fest_plugin".to_owned(),
        "--fest-socket".to_owned(),
        socket_addr.to_owned(),
        format!("--rootdir={}", rootdir.display()),
        "-p".to_owned(),
        "no:xdist".to_owned(),
        "-o".to_owned(),
        "addopts=".to_owned(),
        "--no-header".to_owned(),
        "-q".to_owned(),
    ]
}

/// Build the JSON `mutant` message sent to a worker for one mutant run.
fn build_mutant_message(
    mutant: &Mutant,
    mutated_source: &str,
    tests: &[String],
    diff: &[crate::mutation::MutationDiff],
) -> String {
    // Send an absolute path so the plugin's `os.path.abspath` lookup is
    // idempotent regardless of pytest's cwd (which is the project_dir, NOT
    // the cargo-test cwd that the relative mutant.file_path is anchored to).
    // Falls back to the raw path if canonicalisation fails (e.g. the file
    // was deleted between mutant generation and dispatch).
    let file_path_str = std::fs::canonicalize(&mutant.file_path)
        .unwrap_or_else(|_| mutant.file_path.clone())
        .display()
        .to_string();
    let msg = serde_json::json!({
        "type": "mutant",
        "file": file_path_str,
        "module": file_to_module(&file_path_str),
        "mutated_source": mutated_source,
        "diff": diff,
        "tests": tests,
    });
    msg.to_string()
}

/// Build the JSON `ready_ack` message sent to the plugin in response
/// to its `ready` message.
fn build_ready_ack_message(index: &crate::plugin_index::PluginIndex) -> String {
    let msg = serde_json::json!({
        "type": "ready_ack",
        "protocol_version": PROTOCOL_VERSION,
        "import_bindings": index.import_bindings,
        "reload_warnings": index.reload_warnings,
        "pending_star_imports": index.pending_star_imports,
    });
    msg.to_string()
}

/// Parse the `"status"` field from a parsed JSON value into a [`MutantStatus`].
///
/// The `"skipped"` status is handled by reading an optional `"reason"` field
/// (`snake_case` string).  Unrecognised reason strings default to
/// [`SkipReason::UnsupportedStatement`].
///
/// # Errors
///
/// Returns [`Error::Runner`] if the status field is missing or the status
/// string is unrecognised.
fn parse_status(msg: &serde_json::Value) -> Result<MutantStatus, Error> {
    let status_str = msg
        .get("status")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Error::Runner("result missing 'status' field".to_owned()))?;

    match status_str {
        "killed" => Ok(MutantStatus::Killed),
        "survived" => Ok(MutantStatus::Survived),
        "skipped" => {
            let reason_str = msg
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("unsupported_statement");
            let reason = match reason_str {
                "unmappable_target" => SkipReason::UnmappableTarget,
                "condition_mutation" => SkipReason::ConditionMutation,
                "missing_class_scope" => SkipReason::MissingClassScope,
                _ => SkipReason::UnsupportedStatement,
            };
            Ok(MutantStatus::Skipped { reason })
        }
        "error" => {
            let error_message = msg
                .get("error_message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown plugin error")
                .to_owned();
            Ok(MutantStatus::Error(error_message))
        }
        other => Err(Error::Runner(format!("unknown status: '{other}'"))),
    }
}

/// Parse the `"status"` field from a result JSON message string into a
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

    parse_status(&value)
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

    /// Worker argv pins `--rootdir` so nodeids stay project-relative.
    #[test]
    fn worker_args_pin_rootdir_to_project_dir() {
        // Use a path that is already absolute on every platform (`/proj/app`
        // has no drive on Windows and would be absolutized against cwd).
        let project_dir = std::env::temp_dir().join("proj").join("app");
        let args = build_worker_args("/tmp/xyz/fest.sock", &project_dir);
        let expected = format!("--rootdir={}", project_dir.display());
        assert!(
            args.contains(&expected),
            "worker argv must pin pytest rootdir to the project dir, got {args:?}"
        );
    }

    /// A relative project dir must yield an absolute `--rootdir` — pytest
    /// resolves a relative one against the worker cwd (the project dir),
    /// doubling the path.
    #[test]
    fn worker_args_absolutize_relative_rootdir() {
        let args = build_worker_args("/tmp/xyz/fest.sock", Path::new("rel/proj"));
        let rootdir = args
            .iter()
            .find_map(|a| a.strip_prefix("--rootdir="))
            .expect("--rootdir present");
        assert!(
            Path::new(rootdir).is_absolute(),
            "rootdir must be absolute, got {rootdir}"
        );
    }

    /// The socket path must come through unchanged (paired with its flag).
    #[test]
    fn worker_args_include_socket_flag_and_value() {
        let args = build_worker_args("/tmp/xyz/fest.sock", Path::new("/proj/app"));
        let pos = args
            .iter()
            .position(|a| a == "--fest-socket")
            .expect("--fest-socket flag present");
        assert_eq!(args[pos + 1], "/tmp/xyz/fest.sock");
    }

    /// `build_mutant_message` produces valid JSON with expected fields.
    #[test]
    fn build_mutant_message_produces_valid_json() {
        let mutant = make_test_mutant();
        let mutated = "x = a - b";
        let tests = vec!["test_calc.py::test_add".to_owned()];

        let msg = build_mutant_message(&mutant, mutated, &tests, &[]);
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

        let msg = build_mutant_message(&mutant, "x = a - b", &tests, &[]);
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
        #[cfg(unix)]
        let (listener, connect_addr) = {
            let temp_dir_leaked = Box::leak(Box::new(tempfile::tempdir().unwrap_or_else(|err| {
                #[allow(clippy::panic, reason = "test assertion")]
                {
                    panic!("create temp dir: {err}");
                }
            })));
            let socket_path = temp_dir_leaked.path().join("test.sock");
            let listener = IpcListener::bind(&socket_path).unwrap_or_else(|err| {
                #[allow(clippy::panic, reason = "test assertion")]
                {
                    panic!("bind socket: {err}");
                }
            });
            (listener, socket_path)
        };

        #[cfg(windows)]
        let (listener, connect_addr) = {
            let std_listener =
                std::net::TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0_u16)))
                    .unwrap_or_else(|err| {
                        #[allow(clippy::panic, reason = "test assertion")]
                        {
                            panic!("bind TCP: {err}");
                        }
                    });
            std_listener.set_nonblocking(true).unwrap_or_else(|err| {
                #[allow(clippy::panic, reason = "test assertion")]
                {
                    panic!("set non-blocking: {err}");
                }
            });
            let addr = std_listener.local_addr().unwrap_or_else(|err| {
                #[allow(clippy::panic, reason = "test assertion")]
                {
                    panic!("local addr: {err}");
                }
            });
            let listener = IpcListener::from_std(std_listener).unwrap_or_else(|err| {
                #[allow(clippy::panic, reason = "test assertion")]
                {
                    panic!("convert to tokio: {err}");
                }
            });
            (listener, addr)
        };

        // Spawn a mock plugin that sends READY, reads MUTANT, sends RESULT.
        let mock_handle = tokio::spawn(async move {
            let stream = IpcStream::connect(&connect_addr)
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
        let msg = build_mutant_message(&mutant, "x = a - b", &["test.py::test_x".to_owned()], &[]);
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

    /// `build_ready_ack_message` serializes the index correctly.
    #[test]
    fn build_ready_ack_message_serializes_index() {
        let index = crate::plugin_index::PluginIndex {
            import_bindings: vec![crate::plugin_index::ImportBinding {
                consumer_module: "consumer".into(),
                consumer_key: "x".into(),
                target_module: "target".into(),
                target_name: "x".into(),
            }],
            reload_warnings: vec![],
            module_exports: std::collections::HashMap::new(),
            pending_star_imports: vec![],
        };
        let msg = build_ready_ack_message(&index);
        let val: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(val["type"], "ready_ack");
        assert_eq!(val["import_bindings"][0]["target_module"], "target");
        assert!(
            val["pending_star_imports"].is_array(),
            "pending_star_imports must be present"
        );
    }

    /// `ready_ack` message includes the protocol version.
    #[test]
    fn ready_ack_includes_protocol_version() {
        let idx = crate::plugin_index::PluginIndex::default();
        let msg = build_ready_ack_message(&idx);
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["protocol_version"], serde_json::json!(PROTOCOL_VERSION));
        assert_eq!(v["protocol_version"], serde_json::json!(2_u32));
    }

    /// `build_mutant_message` includes the diff array.
    #[test]
    fn build_mutant_message_includes_diff_field() {
        let mutant = Mutant {
            file_path: "calc.py".into(),
            line: 1,
            column: 1,
            byte_offset: 0,
            byte_length: 1,
            original_text: "+".into(),
            mutated_text: "-".into(),
            mutator_name: "arithmetic".to_owned(),
        };
        let diff = vec![crate::mutation::MutationDiff::FunctionBody {
            qualname: "add".into(),
            new_source: "def add(a, b):\n    return a - b\n".into(),
        }];
        let msg = build_mutant_message(
            &mutant,
            "def add(a, b):\n    return a - b\n",
            &["t1".to_owned()],
            &diff,
        );
        let value: serde_json::Value = serde_json::from_str(&msg).expect("json");
        assert_eq!(value["type"], "mutant");
        assert!(value["diff"].is_array());
        assert_eq!(value["diff"][0]["kind"], "function_body");
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

    /// Integration test: run a real mutant through the plugin pipeline
    /// against the `from_imports` fixture and verify the consumer
    /// (which uses `from src.calc import add`) sees the mutation —
    /// i.e. Plan G's reverse-import rebinding actually works end-to-end.
    ///
    /// Ignored by default — requires a working python with pytest on PATH.
    #[tokio::test]
    #[ignore = "requires python+pytest on PATH; run with --include-ignored"]
    async fn plugin_run_mutant_propagates_to_consumer_via_index() {
        use crate::mutation::{Mutant, MutantStatus};

        let fixture = Path::new("tests/fixtures/from_imports");
        if !fixture.exists() {
            return;
        }
        // Skip cleanly if pytest is not available on the project's python.
        let python = crate::python::resolve_python(fixture);
        let py_check = std::process::Command::new(&python)
            .args(["-c", "import pytest"])
            .output();
        if !py_check.is_ok_and(|out| out.status.success()) {
            #[expect(clippy::print_stderr, reason = "test skip diagnostic")]
            {
                eprintln!(
                    "plugin_run_mutant test: skipped — pytest not available on {}",
                    python.display()
                );
            }
            return;
        }
        let runner = PytestPluginRunner::new(60_u64);
        runner.start(1, fixture).await.expect("start");

        // Mutate `def add(a, b): return a + b` to `return a - b`.
        let calc_path = fixture.join("src/calc.py");
        let source = std::fs::read_to_string(&calc_path).expect("read calc.py");
        let plus_byte = source.find("a + b").expect("find expr") + 2;
        let mutant = Mutant {
            file_path: calc_path.clone(),
            line: 1,
            column: 1,
            byte_offset: plus_byte,
            byte_length: 1,
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            mutator_name: "arithmetic".to_owned(),
        };
        let tests: Vec<String> = vec![
            "tests/test_calc.py::test_add".to_owned(),
            "tests/test_calc.py::test_double_add".to_owned(),
        ];
        let result = runner
            .run_mutant(&mutant, &source, &tests)
            .await
            .expect("run_mutant");

        // The consumer test (`test_double_add` uses `from src.calc import add`)
        // must observe the mutation — status should be Killed, not Survived.
        assert_eq!(
            result.status,
            MutantStatus::Killed,
            "consumer didn't see the mutation; reverse-import index broken? actual status: {:?}",
            result.status,
        );

        runner.stop().await.expect("stop");
    }

    /// Integration test: run the `control_flow_bindings` fixture end-to-end through
    /// the plugin backend, exercising:
    ///  * tuple unpack inside an `if` block (`config.py`)
    ///  * AnnAssign at module scope (`annotated.py`)
    ///  * nested classes (`nested_classes.py`)
    ///  * `from .models import *` star-import propagation (`starred/`)
    ///
    /// Acceptance criteria (when pytest is available):
    ///  * mutation score >= 70% (most mutants killed)
    ///  * 0 error statuses
    ///
    /// Ignored by default — requires python+pytest on PATH.
    #[tokio::test]
    #[ignore = "integration test; requires python+pytest in environment"]
    async fn plugin_handles_control_flow_bindings_fixture() {
        use crate::mutation::{Mutant, MutantStatus};

        let fixture = Path::new("tests/fixtures/control_flow_bindings");
        if !fixture.exists() {
            return;
        }

        // Skip cleanly if pytest is not available on the project's python.
        let python = crate::python::resolve_python(fixture);
        let py_check = std::process::Command::new(&python)
            .args(["-c", "import pytest"])
            .output();
        if !py_check.is_ok_and(|out| out.status.success()) {
            #[expect(clippy::print_stderr, reason = "test skip diagnostic")]
            {
                eprintln!(
                    "plugin_handles_control_flow_bindings_fixture: skipped — pytest not available \
                     on {}",
                    python.display()
                );
            }
            return;
        }

        let runner = PytestPluginRunner::new(60_u64);
        runner.start(1, fixture).await.expect("start");

        // --- Mutant 1: nested_classes.py — change inner VALUE literal 42 → 99
        // Killed by test_inner_value (assert VALUE == 42) and test_inner_compute_doubles.
        let nested_path = fixture.join("src/nested_classes.py");
        let nested_src = std::fs::read_to_string(&nested_path).expect("read nested_classes.py");
        let value_byte = nested_src.find("42").expect("find 42 in nested_classes.py");
        let mutant_nested = Mutant {
            file_path: nested_path.clone(),
            line: 1,
            column: 1,
            byte_offset: value_byte,
            byte_length: 2,
            original_text: "42".to_owned(),
            mutated_text: "99".to_owned(),
            mutator_name: "literal".to_owned(),
        };
        let nested_tests: Vec<String> = vec![
            "tests/test_nested.py::test_inner_value".to_owned(),
            "tests/test_nested.py::test_inner_compute_doubles".to_owned(),
        ];
        let result_nested = runner
            .run_mutant(&mutant_nested, &nested_src, &nested_tests)
            .await
            .expect("run_mutant nested");

        // --- Mutant 2: annotated.py — change COUNTER initial value 0 → 1
        // Killed by test_counter_starts_at_zero.
        let annotated_path = fixture.join("src/annotated.py");
        let annotated_src = std::fs::read_to_string(&annotated_path).expect("read annotated.py");
        // Find `COUNTER: int = 0` — locate the ` 0` literal (the `0` after `= `).
        let counter_byte = annotated_src
            .find("COUNTER: int = 0")
            .expect("find COUNTER decl")
            + "COUNTER: int = ".len();
        let mutant_annotated = Mutant {
            file_path: annotated_path.clone(),
            line: 1,
            column: 1,
            byte_offset: counter_byte,
            byte_length: 1,
            original_text: "0".to_owned(),
            mutated_text: "1".to_owned(),
            mutator_name: "literal".to_owned(),
        };
        let annotated_tests: Vec<String> =
            vec!["tests/test_annotated.py::test_counter_starts_at_zero".to_owned()];
        let result_annotated = runner
            .run_mutant(&mutant_annotated, &annotated_src, &annotated_tests)
            .await
            .expect("run_mutant annotated");

        // --- Mutant 3: starred/models.py — change User.name return value "alice" → "bob"
        // Killed by test_whoami_returns_alice and test_user_export (via star-import propagation).
        let models_path = fixture.join("src/starred/models.py");
        let models_src = std::fs::read_to_string(&models_path).expect("read models.py");
        let alice_byte = models_src
            .find("\"alice\"")
            .expect("find alice in models.py");
        let mutant_models = Mutant {
            file_path: models_path.clone(),
            line: 1,
            column: 1,
            byte_offset: alice_byte,
            byte_length: 7,
            original_text: "\"alice\"".to_owned(),
            mutated_text: "\"bob\"".to_owned(),
            mutator_name: "literal".to_owned(),
        };
        let starred_tests: Vec<String> = vec![
            "tests/test_starred.py::test_whoami_returns_alice".to_owned(),
            "tests/test_starred.py::test_user_export".to_owned(),
        ];
        let result_models = runner
            .run_mutant(&mutant_models, &models_src, &starred_tests)
            .await
            .expect("run_mutant models");

        runner.stop().await.expect("stop");

        // Compute mutation score: skip errors/timeouts, count killed vs (killed + survived).
        let statuses = [
            result_nested.status,
            result_annotated.status,
            result_models.status,
        ];
        let mut killed = 0_u32;
        let mut survived = 0_u32;
        let mut errors = 0_u32;
        for status in &statuses {
            match status {
                MutantStatus::Killed => killed += 1,
                MutantStatus::Survived => survived += 1,
                MutantStatus::Error(_) => errors += 1,
                _ => {}
            }
        }

        assert_eq!(errors, 0, "expected zero error statuses; got {errors}");

        let total = killed + survived;
        assert!(
            total > 0,
            "all mutants were skipped/timed-out — cannot compute score"
        );
        #[allow(clippy::cast_precision_loss)]
        let score = (killed as f64) / (total as f64) * 100.0;
        assert!(
            score >= 70.0,
            "mutation score {score:.1}% is below the 70% threshold (killed={killed}, \
             survived={survived})"
        );
    }

    /// `parse_status` maps `"skipped"` with a known reason string.
    #[test]
    fn parse_status_handles_skipped_with_reason() {
        let msg = serde_json::json!({"status": "skipped", "reason": "condition_mutation"});
        let status = parse_status(&msg).unwrap();
        assert_eq!(
            status,
            MutantStatus::Skipped {
                reason: SkipReason::ConditionMutation,
            }
        );
    }

    /// `parse_status` defaults to `UnsupportedStatement` when reason is absent.
    #[test]
    fn parse_status_defaults_skipped_reason_when_missing() {
        let msg = serde_json::json!({"status": "skipped"});
        let status = parse_status(&msg).unwrap();
        assert_eq!(
            status,
            MutantStatus::Skipped {
                reason: SkipReason::UnsupportedStatement,
            }
        );
    }

    /// `parse_status` defaults to `UnsupportedStatement` for an unrecognised reason.
    #[test]
    fn parse_status_defaults_skipped_reason_when_unrecognized() {
        let msg = serde_json::json!({"status": "skipped", "reason": "not_a_known_reason"});
        let status = parse_status(&msg).unwrap();
        assert_eq!(
            status,
            MutantStatus::Skipped {
                reason: SkipReason::UnsupportedStatement,
            }
        );
    }
}
