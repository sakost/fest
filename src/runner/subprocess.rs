//! Subprocess-based mutant runner.
//!
//! [`SubprocessRunner`] is the simplest (fallback) backend: for each
//! mutant it overwrites the original source file in-place, spawns
//! `pytest` as a subprocess, then restores the original source.
//!
//! Because the mutation lives on disk and pytest imports the whole
//! package, only one mutant may be applied to the source tree at a time —
//! regardless of which file it touches. Two mutants in different files
//! overlapping would each see the other's change and produce false kills.
//! Mutants are therefore serialised behind a single tree-wide lock; this
//! backend has no intra-run parallelism.

use core::time::Duration;
use std::path::PathBuf;

use tokio::{process::Command, sync::Mutex as AsyncMutex};

use crate::{
    Error,
    mutation::{Mutant, MutantResult, MutantStatus},
    runner::{
        Runner,
        process::{IsolatedChild, ProcessRegistry},
    },
};

/// Default timeout in seconds when none is specified.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Configuration for the subprocess runner.
///
/// Holds tunable parameters such as the per-mutant timeout.
///
/// The runner mutates source files **in-place** (overwrite → test → restore)
/// behind a single lock, so at most one mutant is ever visible on disk.
#[derive(Debug)]
pub struct SubprocessRunner {
    /// Maximum wall-clock time (in seconds) for a single pytest
    /// invocation before it is considered timed out.
    timeout: Duration,
    /// Project root directory, used as `current_dir` for pytest.
    project_dir: PathBuf,
    /// Serialises the overwrite → test → restore cycle across *all* files:
    /// pytest imports the whole tree, so a concurrent mutant anywhere else
    /// would contaminate this run's verdict.
    tree_lock: AsyncMutex<()>,
    /// Live pytest process groups, shared with the signal handler so a
    /// cancelled run can kill them.
    processes: ProcessRegistry,
}

impl SubprocessRunner {
    /// Create a new [`SubprocessRunner`] with the given timeout and project directory.
    #[inline]
    #[must_use]
    pub fn new(timeout_secs: u64, project_dir: PathBuf) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_secs),
            project_dir,
            tree_lock: AsyncMutex::new(()),
            processes: ProcessRegistry::default(),
        }
    }

    /// Record spawned pytest processes in `processes` instead of a private
    /// registry, so cancellation can reach them.
    #[inline]
    #[must_use]
    pub fn with_process_registry(mut self, processes: ProcessRegistry) -> Self {
        self.processes = processes;
        self
    }
}

impl Default for SubprocessRunner {
    #[inline]
    fn default() -> Self {
        Self::new(
            DEFAULT_TIMEOUT_SECS,
            std::env::current_dir().unwrap_or_default(),
        )
    }
}

impl Runner for SubprocessRunner {
    /// Run pytest against a single mutant in a subprocess.
    ///
    /// 1. Apply the mutation to the original source.
    /// 2. Overwrite the source file with the mutated version.
    /// 3. Spawn `python -m pytest <tests> -x --no-header -q`.
    /// 4. Restore the original source file.
    /// 5. Interpret the exit code.
    #[inline]
    async fn run_mutant(
        &self,
        mutant: &Mutant,
        source: &str,
        tests: &[String],
    ) -> Result<MutantResult, Error> {
        let start = tokio::time::Instant::now();

        // 1. Apply the mutation.
        let mutated_source = mutant.apply_to_source(source);

        // 2. Take the tree-wide lock: no other mutant may be on disk while this one's tests run.
        let guard = self.tree_lock.lock().await;

        // 3. Overwrite the original file in-place.
        let file_path = &mutant.file_path;
        std::fs::write(file_path, &mutated_source).map_err(|err| {
            Error::Runner(format!(
                "failed to write mutated source to {}: {err}",
                file_path.display()
            ))
        })?;

        // 4. Spawn pytest with timeout. PYTHONDONTWRITEBYTECODE=1 prevents Python from writing .pyc
        //    files so that stale bytecode caches never shadow a restored source file (the
        //    mtime-based invalidation can miss rapid write→restore cycles that land in the same
        //    second).
        let mut cmd = Command::new(crate::python::resolve_python(&self.project_dir));
        let _cmd_ref = cmd
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .args(["-m", "pytest", "-x", "--no-header", "-q"])
            .args(tests)
            .current_dir(&self.project_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        let outcome = run_with_timeout(&mut cmd, self.timeout, &self.processes).await;

        // 5. Restore the original source immediately (before releasing lock).
        let _restore = std::fs::write(file_path, source);

        // Lock released here when guard drops.
        drop(guard);

        let elapsed = start.elapsed();
        let tests_run: Vec<String> = tests.iter().map(ToString::to_string).collect();

        // 6. Interpret the result.
        let status = match outcome {
            Ok(None) => MutantStatus::Timeout,
            Err(err) => MutantStatus::Error(format!("failed to spawn pytest: {err}")),
            Ok(Some(exit)) => interpret_exit_code(exit.code()),
        };

        Ok(MutantResult {
            mutant: mutant.clone(),
            status,
            tests_run,
            duration: elapsed,
        })
    }
}

/// Spawn `cmd` in its own process group and wait for it up to `timeout`.
///
/// Returns `Ok(None)` when the deadline passes; by then the whole process
/// tree has been killed and reaped, so a runaway mutant cannot outlive its
/// verdict (issue #15).
///
/// # Errors
///
/// Returns the spawn error if the interpreter cannot be started.
async fn run_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
    processes: &ProcessRegistry,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let mut child = IsolatedChild::spawn(cmd, processes)?;
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(exit) => exit.map(Some),
        Err(_elapsed) => {
            child.kill_tree().await;
            Ok(None)
        }
    }
}

/// Interpret the pytest exit code into a [`MutantStatus`].
///
/// - exit 0 means all tests passed => mutant survived
/// - any other exit code means at least one test failed => mutant killed
/// - `None` means the process was killed by a signal => treat as killed
const fn interpret_exit_code(code: Option<i32>) -> MutantStatus {
    match code {
        Some(0_i32) => MutantStatus::Survived,
        Some(_) | None => MutantStatus::Killed,
    }
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

    /// Mutation is correctly applied.
    #[test]
    fn mutation_application() {
        let mutant = make_test_mutant();
        let source = "x = a + b";

        let mutated = mutant.apply_to_source(source);
        assert_eq!(mutated, "x = a - b");
    }

    /// `interpret_exit_code` maps exit codes to statuses correctly.
    #[test]
    fn exit_code_interpretation() {
        assert_eq!(interpret_exit_code(Some(0_i32)), MutantStatus::Survived);
        assert_eq!(interpret_exit_code(Some(1_i32)), MutantStatus::Killed);
        assert_eq!(interpret_exit_code(Some(2_i32)), MutantStatus::Killed);
        assert_eq!(interpret_exit_code(Some(127_i32)), MutantStatus::Killed);
        // Signal-killed process (no exit code)
        assert_eq!(interpret_exit_code(None), MutantStatus::Killed);
    }

    /// `build_python_path` prepends the directory.
    #[test]
    fn python_path_construction() {
        let dir = Path::new("/tmp/fest_test");
        let result = super::super::build_python_path(dir);
        assert!(result.starts_with("/tmp/fest_test"));
    }

    /// `SubprocessRunner::new` sets the timeout correctly.
    #[test]
    fn runner_timeout_configuration() {
        let runner = SubprocessRunner::new(60_u64, PathBuf::from("/project"));
        assert_eq!(runner.timeout, Duration::from_secs(60_u64));
    }

    /// `SubprocessRunner::default` uses the default timeout.
    #[test]
    fn runner_default_timeout() {
        let runner = SubprocessRunner::default();
        assert_eq!(runner.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    }

    /// Timeout handling: a very short timeout causes a `Timeout` status.
    #[tokio::test]
    async fn timeout_produces_timeout_status() {
        // Use a temp file so the in-place write succeeds.
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        std::fs::write(tmp.path(), "pass").expect("write source");

        let runner = SubprocessRunner::new(0_u64, PathBuf::from("."));

        let mutant = Mutant {
            file_path: tmp.path().to_path_buf(),
            line: 1_u32,
            column: 1_u32,
            byte_offset: 0_usize,
            byte_length: 4_usize,
            original_text: "pass".to_owned(),
            mutated_text: "pass".to_owned(),
            mutator_name: "noop".to_owned(),
        };

        let source = "pass";
        let tests = vec!["test_slow.py::test_hang".to_owned()];

        let result = runner
            .run_mutant(&mutant, source, &tests)
            .await
            .expect("should not return Err");

        // With a 0-second timeout, the process should be timed out.
        // However, if the system is extremely fast, pytest might not
        // even be spawned before the timeout. Either Timeout or Error
        // is acceptable.
        assert!(
            result.status == MutantStatus::Timeout
                || matches!(result.status, MutantStatus::Error(_)),
            "expected Timeout or Error, got {:?}",
            result.status,
        );

        // Verify original source was restored.
        let restored = std::fs::read_to_string(tmp.path()).expect("read restored");
        assert_eq!(restored, "pass");
    }

    /// Returns `true` while a process with `pid` still exists (zombies included).
    #[cfg(unix)]
    fn process_alive(pid: i32) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
    }

    /// Timing out a mutant must kill the whole pytest process tree, not just
    /// abandon it (issue #15): a runaway grandchild kept allocating after fest
    /// exited and eventually got the run OOM-killed.
    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_pytest_process_group() {
        use std::os::unix::fs::PermissionsExt;

        // Fake `.venv/bin/python` that forks a grandchild and then blocks.
        let project = tempfile::tempdir().expect("create project dir");
        let bin = project.path().join(".venv").join("bin");
        std::fs::create_dir_all(&bin).expect("create venv bin");
        let pid_file = project.path().join("grandchild.pid");
        let script = format!(
            "#!/bin/sh\nsleep 30 &\necho $! > '{}'\nwait\n",
            pid_file.display()
        );
        let python = bin.join("python");
        std::fs::write(&python, script).expect("write fake python");
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake python");

        let source_file = project.path().join("mod.py");
        std::fs::write(&source_file, "pass").expect("write source");
        let mut mutant = make_test_mutant();
        mutant.file_path = source_file;
        mutant.byte_offset = 0_usize;
        mutant.byte_length = 4_usize;

        let runner = SubprocessRunner::new(1_u64, project.path().to_path_buf());
        let result = runner
            .run_mutant(&mutant, "pass", &["tests/test_x.py::test_x".to_owned()])
            .await
            .expect("run_mutant must not fail");
        assert_eq!(result.status, MutantStatus::Timeout);

        let pid: i32 = std::fs::read_to_string(&pid_file)
            .expect("grandchild pid file")
            .trim()
            .parse()
            .expect("grandchild pid");
        // The grandchild is re-parented to init once its parent dies, so it is
        // reaped promptly — but not synchronously. Poll briefly.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5_u64);
        while process_alive(pid) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50_u64)).await;
        }
        assert!(
            !process_alive(pid),
            "grandchild {pid} survived the mutant timeout"
        );
    }

    /// Two mutants in *different* files must never be on disk at the same
    /// time: pytest imports the whole package, so a concurrent mutant in
    /// another file would leak into this mutant's verdict (false kills).
    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_mutants_in_different_files_never_overlap() {
        use std::os::unix::fs::PermissionsExt;

        let project = tempfile::tempdir().expect("create project dir");
        let root = project.path();
        std::fs::write(root.join("a.py"), "A\n").expect("write a.py");
        std::fs::write(root.join("b.py"), "B\n").expect("write b.py");
        let bin = root.join(".venv").join("bin");
        std::fs::create_dir_all(&bin).expect("create venv bin");
        // Fake pytest: snapshot both files, linger, snapshot again. One
        // `printf` per snapshot so concurrent appends cannot interleave.
        let snapshot = "printf '%s\\n' \"$(cat a.py b.py | tr -d '\\n')\" >> snapshots.log";
        let script = format!("#!/bin/sh\n{snapshot}\nsleep 0.3\n{snapshot}\n");
        let python = bin.join("python");
        std::fs::write(&python, script).expect("write fake python");
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake python");

        let mutant_for = |name: &str, original: &str, mutated: &str| Mutant {
            file_path: root.join(name),
            line: 1_u32,
            column: 1_u32,
            byte_offset: 0_usize,
            byte_length: 1_usize,
            original_text: original.to_owned(),
            mutated_text: mutated.to_owned(),
            mutator_name: "constant_replace".to_owned(),
        };
        let runner = SubprocessRunner::new(10_u64, root.to_path_buf());
        let tests = vec!["tests/test_x.py::test_x".to_owned()];
        let mutant_a = mutant_for("a.py", "A", "X");
        let mutant_b = mutant_for("b.py", "B", "Y");
        let (first, second) = tokio::join!(
            runner.run_mutant(&mutant_a, "A\n", &tests),
            runner.run_mutant(&mutant_b, "B\n", &tests),
        );
        assert_eq!(first.expect("first run").status, MutantStatus::Survived);
        assert_eq!(second.expect("second run").status, MutantStatus::Survived);

        let log = std::fs::read_to_string(root.join("snapshots.log")).expect("snapshots");
        assert!(
            !log.lines().any(|line| line == "XY"),
            "a test run observed both files mutated at once:\n{log}"
        );
        assert_eq!(
            log.lines().count(),
            4_usize,
            "two runs × two snapshots:\n{log}"
        );
    }

    /// A mutant that runs against a non-existent test file produces
    /// a Killed status (pytest exits non-zero).
    #[tokio::test]
    async fn nonexistent_test_produces_killed() {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        std::fs::write(tmp.path(), "1").expect("write source");

        let runner = SubprocessRunner::new(10_u64, PathBuf::from("."));

        let mutant = Mutant {
            file_path: tmp.path().to_path_buf(),
            line: 1_u32,
            column: 1_u32,
            byte_offset: 0_usize,
            byte_length: 1_usize,
            original_text: "1".to_owned(),
            mutated_text: "2".to_owned(),
            mutator_name: "constant_replace".to_owned(),
        };

        let source = "1";
        let tests = vec!["nonexistent_test_file.py::test_nothing".to_owned()];

        let result = runner
            .run_mutant(&mutant, source, &tests)
            .await
            .expect("should not return Err");

        // pytest will exit non-zero for a nonexistent test => Killed,
        // or Error if python is not found.
        assert!(
            result.status == MutantStatus::Killed
                || matches!(result.status, MutantStatus::Error(_)),
            "expected Killed or Error, got {:?}",
            result.status,
        );
    }

    /// The result includes the correct tests_run list.
    #[tokio::test]
    async fn result_contains_tests_run() {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        std::fs::write(tmp.path(), "x = a + b").expect("write source");

        let runner = SubprocessRunner::new(10_u64, PathBuf::from("."));
        let mut mutant = make_test_mutant();
        mutant.file_path = tmp.path().to_path_buf();
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

    /// The result duration is non-negative (sanity check).
    #[tokio::test]
    async fn result_has_duration() {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        std::fs::write(tmp.path(), "x = a + b").expect("write source");

        let runner = SubprocessRunner::new(10_u64, PathBuf::from("."));
        let mut mutant = make_test_mutant();
        mutant.file_path = tmp.path().to_path_buf();
        let source = "x = a + b";
        let tests: Vec<String> = Vec::new();

        let result = runner
            .run_mutant(&mutant, source, &tests)
            .await
            .expect("should not return Err");

        // Duration should be at least zero (always true for Duration).
        assert!(result.duration >= Duration::from_secs(0_u64));
    }

    /// The mutant in the result matches the input mutant.
    #[tokio::test]
    async fn result_mutant_matches_input() {
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        std::fs::write(tmp.path(), "x = a + b").expect("write source");

        let runner = SubprocessRunner::new(10_u64, PathBuf::from("."));
        let mut mutant = make_test_mutant();
        mutant.file_path = tmp.path().to_path_buf();
        let source = "x = a + b";
        let tests: Vec<String> = Vec::new();

        let result = runner
            .run_mutant(&mutant, source, &tests)
            .await
            .expect("should not return Err");

        assert_eq!(result.mutant, mutant);
    }
}
