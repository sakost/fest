//! Child-process lifecycle helpers shared by the runner backends.
//!
//! Every pytest process fest spawns may fork children of its own (a test
//! that shells out, an xdist worker, a server started under test). Killing
//! only the direct child on a timeout orphans those grandchildren, which
//! then keep running — and keep allocating — after fest has moved on or
//! even exited (issue #15). On Unix each child is therefore started as the
//! leader of its own process group so the whole tree can be signalled at
//! once; Windows has no equivalent here and falls back to terminating the
//! direct child.
//!
//! Isolating the group also means the terminal's `SIGINT` no longer reaches
//! the children. Live groups are tracked in a [`ProcessRegistry`] shared
//! with the signal handler, which takes them all down on cancellation via
//! [`ProcessRegistry::kill_all`].

extern crate alloc;

use alloc::{collections::BTreeSet, sync::Arc};
#[cfg(unix)]
use core::time::Duration;
use std::{
    io,
    process::ExitStatus,
    sync::{Mutex, MutexGuard, PoisonError},
};

use tokio::process::{Child, Command};

/// Grace period between `SIGTERM` and `SIGKILL` when tearing a tree down.
///
/// Long enough for pytest to exit on its own; short enough that a timed-out
/// mutant does not hold a worker slot noticeably longer.
#[cfg(unix)]
const TERM_GRACE: Duration = Duration::from_secs(2);

/// Process groups (keyed by leader pid) that fest has spawned and not yet
/// reaped.
///
/// Cloning shares the underlying set: the runner registers children, the
/// signal handler kills them. Stays empty on Windows, where there is
/// nothing to signal by group.
#[derive(Debug, Clone, Default)]
pub struct ProcessRegistry {
    /// Shared set of live group ids.
    groups: Arc<Mutex<BTreeSet<i32>>>,
}

impl ProcessRegistry {
    /// `SIGKILL` every live process group in the registry.
    ///
    /// Called on cancellation: an abort must not leave a runaway mutant's
    /// test process behind. Owners still reap their leaders through
    /// [`IsolatedChild::wait`]; this only stops the trees.
    #[inline]
    pub fn kill_all(&self) {
        #[cfg(unix)]
        for &pgid in self.groups().iter() {
            let _kill = signal_group(pgid, nix::sys::signal::Signal::SIGKILL);
        }
    }

    /// Whether `pgid` is currently registered as a live group.
    #[cfg(all(test, unix))]
    fn contains(&self, pgid: i32) -> bool {
        self.groups().contains(&pgid)
    }

    /// Lock the set, tolerating poisoning (the set stays usable).
    fn groups(&self) -> MutexGuard<'_, BTreeSet<i32>> {
        self.groups.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Send `signal` to the whole process group `pgid`.
#[cfg(unix)]
fn signal_group(pgid: i32, signal: nix::sys::signal::Signal) -> nix::Result<()> {
    nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid), signal)
}

/// A child spawned as the leader of its own process group.
///
/// The group is registered for as long as the leader is un-reaped. Dropping
/// the handle without reaping kills the whole tree so a cancelled future
/// cannot leak it.
#[derive(Debug)]
pub(super) struct IsolatedChild {
    /// The tokio child handle for the group leader.
    child: Child,
    /// Group id while the leader is registered; `None` once reaped.
    pgid: Option<i32>,
    /// Registry the group was recorded in.
    registry: ProcessRegistry,
}

impl IsolatedChild {
    /// Spawn `cmd` in its own process group and record it in `registry`.
    ///
    /// # Errors
    ///
    /// Returns the spawn error if the interpreter cannot be started.
    pub(super) fn spawn(cmd: &mut Command, registry: &ProcessRegistry) -> io::Result<Self> {
        #[cfg(unix)]
        let _cmd = cmd.process_group(0_i32);
        let child = cmd.spawn()?;
        // With `process_group(0)` the leader's pid doubles as the pgid.
        let pgid = if cfg!(unix) {
            child.id().and_then(|pid| i32::try_from(pid).ok())
        } else {
            None
        };
        if let Some(id) = pgid {
            let _inserted = registry.groups().insert(id);
        }
        Ok(Self {
            child,
            pgid,
            registry: registry.clone(),
        })
    }

    /// The process group id, while the leader is still registered.
    #[cfg(all(test, unix))]
    pub(super) const fn group_id(&self) -> Option<i32> {
        self.pgid
    }

    /// Wait for the leader to exit and unregister its group.
    ///
    /// # Errors
    ///
    /// Propagates the underlying `waitpid` error.
    pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait().await;
        self.unregister();
        status
    }

    /// Kill the leader together with every process in its group, then reap it.
    ///
    /// Unix: `SIGTERM` to the group, wait up to [`TERM_GRACE`], then `SIGKILL`
    /// the group unconditionally — grandchildren that ignored `SIGTERM` must
    /// not outlive the leader. Windows: terminate the direct child only.
    pub(super) async fn kill_tree(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            let _term = signal_group(pgid, nix::sys::signal::Signal::SIGTERM);
            let exited = tokio::time::timeout(TERM_GRACE, self.child.wait())
                .await
                .is_ok();
            let _kill = signal_group(pgid, nix::sys::signal::Signal::SIGKILL);
            if !exited {
                let _wait = self.child.wait().await;
            }
            self.unregister();
            return;
        }
        let _kill = self.child.kill().await;
        let _wait = self.child.wait().await;
        self.unregister();
    }

    /// Forget the group once the leader has been reaped.
    fn unregister(&mut self) {
        if let Some(pgid) = self.pgid.take() {
            let _removed = self.registry.groups().remove(&pgid);
        }
    }
}

impl Drop for IsolatedChild {
    /// An un-reaped leader means the owner gave up on the tree: kill it.
    ///
    /// `Drop` cannot await, so the leader is left for tokio's orphan reaper;
    /// the grandchildren are what matter here.
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            let _kill = signal_group(pgid, nix::sys::signal::Signal::SIGKILL);
        }
        self.unregister();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Returns `true` while a process with `pid` still exists (zombies included).
    fn process_alive(pid: i32) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
    }

    /// Spawn `sh` forking a 30 s grandchild whose pid is written to `pid_file`.
    fn spawn_forking_shell(
        pid_file: &std::path::Path,
        registry: &ProcessRegistry,
    ) -> IsolatedChild {
        let mut cmd = Command::new("sh");
        let _cmd = cmd
            .arg("-c")
            .arg(format!(
                "sleep 30 & echo $! > '{}'; wait",
                pid_file.display()
            ))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        IsolatedChild::spawn(&mut cmd, registry).expect("spawn sh")
    }

    /// Block until the grandchild has announced its pid.
    async fn read_grandchild_pid(pid_file: &std::path::Path) -> i32 {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5_u64);
        loop {
            if let Ok(text) = std::fs::read_to_string(pid_file)
                && let Ok(pid) = text.trim().parse::<i32>()
            {
                return pid;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "grandchild never wrote its pid"
            );
            tokio::time::sleep(Duration::from_millis(20_u64)).await;
        }
    }

    /// Poll until `pid` is gone, failing after a few seconds.
    async fn assert_dies(pid: i32, what: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5_u64);
        while process_alive(pid) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50_u64)).await;
        }
        assert!(!process_alive(pid), "{what} {pid} is still alive");
    }

    /// A signal-driven abort must take every in-flight process tree with it,
    /// not only the direct children (issue #15).
    #[tokio::test]
    async fn kill_all_kills_grandchildren() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("pid");
        let registry = ProcessRegistry::default();
        let mut child = spawn_forking_shell(&pid_file, &registry);
        let grandchild = read_grandchild_pid(&pid_file).await;
        let pgid = child.group_id().expect("pgid known");
        assert!(registry.contains(pgid), "spawn must register the group");

        registry.kill_all();

        assert_dies(grandchild, "grandchild").await;
        let exit = child.wait().await.expect("reap leader");
        assert!(!exit.success(), "leader must have been signalled");
    }

    /// A normally reaped child leaves no stale registry entry behind, so a
    /// later abort cannot signal a recycled pid.
    #[tokio::test]
    async fn wait_unregisters_group() {
        let mut cmd = Command::new("sh");
        let _cmd = cmd
            .args(["-c", "true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let registry = ProcessRegistry::default();
        let mut child = IsolatedChild::spawn(&mut cmd, &registry).expect("spawn sh");
        let pgid = child.group_id().expect("pgid known before reap");
        assert!(registry.contains(pgid), "spawn must register the group");

        let exit = child.wait().await.expect("reap");
        assert!(exit.success());
        assert!(!registry.contains(pgid), "wait must unregister the group");
    }

    /// Dropping an un-reaped handle (e.g. a cancelled future) kills the tree
    /// rather than leaking it.
    #[tokio::test]
    async fn drop_without_wait_kills_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("pid");
        let registry = ProcessRegistry::default();
        let child = spawn_forking_shell(&pid_file, &registry);
        let grandchild = read_grandchild_pid(&pid_file).await;
        let pgid = child.group_id().expect("pgid known before drop");

        drop(child);

        assert_dies(grandchild, "grandchild").await;
        assert!(!registry.contains(pgid), "drop must unregister the group");
    }

    /// `kill_tree` escalates to `SIGKILL` for a leader that ignores `SIGTERM`.
    #[tokio::test]
    async fn kill_tree_escalates_past_ignored_sigterm() {
        let mut cmd = Command::new("sh");
        let _cmd = cmd
            .args(["-c", "trap '' TERM; sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let registry = ProcessRegistry::default();
        let mut child = IsolatedChild::spawn(&mut cmd, &registry).expect("spawn sh");
        let pgid = child.group_id().expect("pgid known");

        child.kill_tree().await;

        assert_dies(pgid, "leader").await;
        assert!(
            !registry.contains(pgid),
            "kill_tree must unregister the group"
        );
    }
}
