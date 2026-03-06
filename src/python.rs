//! Python interpreter discovery.
//!
//! Resolves the path to the Python binary by checking, in order:
//! 1. The `VIRTUAL_ENV` environment variable (set by `source .venv/bin/activate`).
//! 2. A `.venv/` directory in the project root.
//! 3. Falls back to `"python"` (relies on `PATH`).

use std::path::{Path, PathBuf};

/// Resolve the Python interpreter for the given project directory.
///
/// Checks `VIRTUAL_ENV`, then `<project_dir>/.venv`, then falls back
/// to the bare `python` command (found via `PATH`).
#[inline]
#[must_use]
pub fn resolve_python(project_dir: &Path) -> PathBuf {
    resolve_python_inner(project_dir, std::env::var("VIRTUAL_ENV").ok().as_deref())
}

/// Inner implementation that accepts the `VIRTUAL_ENV` value explicitly,
/// making it testable without mutating process environment variables.
fn resolve_python_inner(project_dir: &Path, virtual_env: Option<&str>) -> PathBuf {
    // 1. Honour VIRTUAL_ENV if set (user activated a venv).
    if let Some(venv) = virtual_env {
        let bin = venv_bin_dir(Path::new(venv));
        let python = bin.join(python_exe_name());
        if python.exists() {
            return python;
        }
    }

    // 2. Check for a local .venv in the project directory.
    let local_venv = project_dir.join(".venv");
    if local_venv.is_dir() {
        let bin = venv_bin_dir(&local_venv);
        let python = bin.join(python_exe_name());
        if python.exists() {
            return python;
        }
    }

    // 3. Fallback to bare "python" on PATH.
    PathBuf::from(python_exe_name())
}

/// Return the binary subdirectory inside a virtualenv.
///
/// `Scripts` on Windows, `bin` on Unix.
#[inline]
fn venv_bin_dir(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts")
    } else {
        venv.join("bin")
    }
}

/// Return the Python executable name for the current platform.
#[inline]
const fn python_exe_name() -> &'static str {
    if cfg!(windows) {
        "python.exe"
    } else {
        "python"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// When no venv is present, falls back to "python".
    #[test]
    fn fallback_to_bare_python() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let result = resolve_python_inner(dir.path(), None);
        assert_eq!(result, PathBuf::from(python_exe_name()));
    }

    /// Detects a local .venv directory.
    #[test]
    fn detects_local_venv() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let bin = venv_bin_dir(&dir.path().join(".venv"));
        std::fs::create_dir_all(&bin).expect("create bin dir");
        let python = bin.join(python_exe_name());
        std::fs::write(&python, "").expect("create fake python");

        let result = resolve_python_inner(dir.path(), None);
        assert_eq!(result, python);
    }

    /// VIRTUAL_ENV takes precedence over local .venv.
    #[test]
    fn virtual_env_takes_precedence() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let venv_dir = dir.path().join("my_venv");
        let bin = venv_bin_dir(&venv_dir);
        std::fs::create_dir_all(&bin).expect("create bin dir");
        let python = bin.join(python_exe_name());
        std::fs::write(&python, "").expect("create fake python");

        let result = resolve_python_inner(dir.path(), Some(&venv_dir.display().to_string()));
        assert_eq!(result, python);
    }
}
