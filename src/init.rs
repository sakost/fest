//! Project scaffolding — `fest init` generates a starter configuration.
//!
//! Inspects the current directory to detect the Python project layout and
//! writes a `fest.toml` (or appends `[tool.fest]` to `pyproject.toml`) with
//! meaningful defaults.

use std::{io::Write as _, path::Path};

use crate::Error;

/// Format for the generated configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum InitFormat {
    /// Standalone `fest.toml` file.
    #[default]
    FestToml,
    /// Append `[tool.fest]` section to `pyproject.toml`.
    Pyproject,
}

/// Arguments for the `init` subcommand.
#[derive(Debug, clap::Args)]
pub struct InitArgs {
    /// Configuration format to generate.
    #[arg(short, long, default_value = "fest-toml")]
    pub format: InitFormat,

    /// Overwrite an existing configuration file.
    #[arg(long)]
    pub force: bool,
}

/// Detected Python project layout.
#[derive(Debug)]
struct ProjectLayout {
    /// Source glob patterns discovered.
    source_patterns: Vec<String>,
    /// Suggested exclude patterns.
    exclude_patterns: Vec<String>,
    /// Whether `pyproject.toml` already exists.
    has_pyproject: bool,
    /// Whether `fest.toml` already exists.
    has_fest_toml: bool,
}

/// Run the `init` subcommand.
///
/// # Errors
///
/// Returns [`Error::Config`] if the target file already exists (and `--force`
/// was not given) or if writing fails.
#[inline]
pub fn run(args: &InitArgs, dir: &Path) -> Result<(), Error> {
    let layout = detect_layout(dir);

    // Check for existing config
    if !args.force {
        check_existing_config(args.format, &layout)?;
    }

    let config_toml = generate_config(&layout);

    match args.format {
        InitFormat::FestToml => write_fest_toml(dir, &config_toml),
        InitFormat::Pyproject => write_pyproject_section(dir, &config_toml, layout.has_pyproject),
    }
}

/// Detect the Python project layout by inspecting the filesystem.
fn detect_layout(dir: &Path) -> ProjectLayout {
    let source_patterns = detect_source_patterns(dir);
    let exclude_patterns = detect_exclude_patterns(dir);
    let has_pyproject = dir.join("pyproject.toml").is_file();
    let has_fest_toml = dir.join("fest.toml").is_file();

    ProjectLayout {
        source_patterns,
        exclude_patterns,
        has_pyproject,
        has_fest_toml,
    }
}

/// Detect source glob patterns by inspecting common Python project layouts.
///
/// Checks (in order):
/// 1. `src/<package>/` layout (PEP 517 src-layout)
/// 2. Top-level package directories (flat layout)
/// 3. Fallback to `src/**/*.py`
fn detect_source_patterns(dir: &Path) -> Vec<String> {
    // 1. src-layout: src/<something>/__init__.py
    let src_dir = dir.join("src");
    if src_dir.is_dir() {
        let packages = find_packages_in(&src_dir);
        if !packages.is_empty() {
            return packages
                .iter()
                .map(|pkg| format!("src/{pkg}/**/*.py"))
                .collect();
        }
        // src/ exists but no packages — use broad glob
        return vec!["src/**/*.py".to_owned()];
    }

    // 2. Flat layout: top-level package directories
    let packages = find_packages_in(dir);
    let top_level: Vec<String> = packages
        .into_iter()
        .filter(|name| !is_non_source_dir(name))
        .collect();

    if !top_level.is_empty() {
        return top_level
            .iter()
            .map(|pkg| format!("{pkg}/**/*.py"))
            .collect();
    }

    // 3. Fallback
    vec!["src/**/*.py".to_owned()]
}

/// Find Python package directories (containing `__init__.py`) in `parent`.
fn find_packages_in(parent: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };

    let mut packages = Vec::new();
    for dir_entry in entries {
        let Ok(valid_entry) = dir_entry else {
            continue;
        };
        let path = valid_entry.path();
        if path.is_dir()
            && path.join("__init__.py").exists()
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
            && !name.starts_with('.')
        {
            packages.push(name.to_owned());
        }
    }

    packages.sort();
    packages
}

/// Returns `true` for directory names that are unlikely to be source code.
fn is_non_source_dir(name: &str) -> bool {
    matches!(
        name,
        "tests"
            | "test"
            | "docs"
            | "doc"
            | "examples"
            | "benchmarks"
            | "bench"
            | "scripts"
            | "tools"
            | "vendor"
            | "venv"
            | "env"
            | "node_modules"
            | "__pycache__"
    )
}

/// Detect common exclude patterns based on what exists in the project.
fn detect_exclude_patterns(dir: &Path) -> Vec<String> {
    let mut excludes = Vec::new();

    // Always exclude test files and conftest
    excludes.push("**/test_*.py".to_owned());
    excludes.push("**/conftest.py".to_owned());

    // Django migrations
    if dir.join("manage.py").exists() {
        excludes.push("**/migrations/**/*.py".to_owned());
    }

    // Common directories
    for name in ["docs", "examples", "scripts"] {
        if dir.join(name).is_dir() {
            excludes.push(format!("{name}/**/*.py"));
        }
    }

    excludes
}

/// Generate the TOML configuration content (without the wrapping table header).
fn generate_config(layout: &ProjectLayout) -> String {
    let mut lines = Vec::new();

    // Source patterns
    let sources: Vec<String> = layout
        .source_patterns
        .iter()
        .map(|pat| format!("\"{pat}\""))
        .collect();
    lines.push(format!("source = [{}]", sources.join(", ")));

    // Exclude patterns
    if !layout.exclude_patterns.is_empty() {
        let excludes: Vec<String> = layout
            .exclude_patterns
            .iter()
            .map(|pat| format!("\"{pat}\""))
            .collect();
        lines.push(format!("exclude = [{}]", excludes.join(", ")));
    }

    // Timeout — 30s is a reasonable starting point for most projects
    lines.push("timeout = 30".to_owned());

    // Session file for resume support
    lines.push("session = \".fest-session.db\"".to_owned());

    lines.join("\n")
}

/// Check whether writing would overwrite existing configuration.
fn check_existing_config(format: InitFormat, layout: &ProjectLayout) -> Result<(), Error> {
    match format {
        InitFormat::FestToml => {
            if layout.has_fest_toml {
                return Err(Error::Config(
                    "fest.toml already exists (use --force to overwrite)".to_owned(),
                ));
            }
        }
        InitFormat::Pyproject => {
            if layout.has_pyproject && pyproject_has_fest_section(Path::new("pyproject.toml")) {
                return Err(Error::Config(
                    "[tool.fest] already exists in pyproject.toml (use --force to overwrite)"
                        .to_owned(),
                ));
            }
        }
    }
    Ok(())
}

/// Check if `pyproject.toml` already contains a `[tool.fest]` section.
fn pyproject_has_fest_section(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return false;
    };
    table
        .get("tool")
        .and_then(toml::Value::as_table)
        .and_then(|tool| tool.get("fest"))
        .is_some()
}

/// Write a standalone `fest.toml` file.
fn write_fest_toml(dir: &Path, config_body: &str) -> Result<(), Error> {
    let path = dir.join("fest.toml");
    let content = format!("[fest]\n{config_body}\n");

    let mut file = std::fs::File::create(&path)
        .map_err(|err| Error::Config(format!("failed to create fest.toml: {err}")))?;

    file.write_all(content.as_bytes())
        .map_err(|err| Error::Config(format!("failed to write fest.toml: {err}")))?;

    Ok(())
}

/// Append `[tool.fest]` section to `pyproject.toml`.
fn write_pyproject_section(
    dir: &Path,
    config_body: &str,
    has_pyproject: bool,
) -> Result<(), Error> {
    let path = dir.join("pyproject.toml");

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .truncate(false)
        .open(&path)
        .map_err(|err| Error::Config(format!("failed to open pyproject.toml: {err}")))?;

    // Add a separator if the file already has content
    let prefix = if has_pyproject { "\n" } else { "" };
    let section = format!("{prefix}[tool.fest]\n{config_body}\n");

    file.write_all(section.as_bytes())
        .map_err(|err| Error::Config(format!("failed to write pyproject.toml: {err}")))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_src_layout() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let pkg_dir = dir.path().join("src/mypackage");
        std::fs::create_dir_all(&pkg_dir).expect("create dirs");
        std::fs::write(pkg_dir.join("__init__.py"), "").expect("write init");

        let patterns = detect_source_patterns(dir.path());
        assert_eq!(patterns, vec!["src/mypackage/**/*.py"]);
    }

    #[test]
    fn detect_src_layout_multiple_packages() {
        let dir = tempfile::tempdir().expect("create temp dir");
        for name in ["alpha", "beta"] {
            let pkg = dir.path().join(format!("src/{name}"));
            std::fs::create_dir_all(&pkg).expect("create dirs");
            std::fs::write(pkg.join("__init__.py"), "").expect("write init");
        }

        let patterns = detect_source_patterns(dir.path());
        assert_eq!(patterns, vec!["src/alpha/**/*.py", "src/beta/**/*.py"]);
    }

    #[test]
    fn detect_flat_layout() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let pkg = dir.path().join("mylib");
        std::fs::create_dir_all(&pkg).expect("create dirs");
        std::fs::write(pkg.join("__init__.py"), "").expect("write init");

        let patterns = detect_source_patterns(dir.path());
        assert_eq!(patterns, vec!["mylib/**/*.py"]);
    }

    #[test]
    fn detect_flat_layout_excludes_tests_dir() {
        let dir = tempfile::tempdir().expect("create temp dir");
        for name in ["mylib", "tests"] {
            let pkg = dir.path().join(name);
            std::fs::create_dir_all(&pkg).expect("create dirs");
            std::fs::write(pkg.join("__init__.py"), "").expect("write init");
        }

        let patterns = detect_source_patterns(dir.path());
        assert_eq!(patterns, vec!["mylib/**/*.py"]);
    }

    #[test]
    fn detect_fallback_when_empty() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let patterns = detect_source_patterns(dir.path());
        assert_eq!(patterns, vec!["src/**/*.py"]);
    }

    #[test]
    fn detect_excludes_django_migrations() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("manage.py"), "").expect("write manage.py");

        let excludes = detect_exclude_patterns(dir.path());
        assert!(excludes.iter().any(|pat| pat.contains("migrations")));
    }

    #[test]
    fn detect_excludes_always_has_test_patterns() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let excludes = detect_exclude_patterns(dir.path());
        assert!(excludes.contains(&"**/test_*.py".to_owned()));
        assert!(excludes.contains(&"**/conftest.py".to_owned()));
    }

    #[test]
    fn generate_config_includes_source_and_timeout() {
        let layout = ProjectLayout {
            source_patterns: vec!["src/myapp/**/*.py".to_owned()],
            exclude_patterns: vec!["**/test_*.py".to_owned()],
            has_pyproject: false,
            has_fest_toml: false,
        };

        let config = generate_config(&layout);
        assert!(config.contains("source = [\"src/myapp/**/*.py\"]"));
        assert!(config.contains("exclude = [\"**/test_*.py\"]"));
        assert!(config.contains("timeout = 30"));
        assert!(config.contains("session = \".fest-session.db\""));
    }

    #[test]
    fn write_fest_toml_creates_file() {
        let dir = tempfile::tempdir().expect("create temp dir");
        write_fest_toml(dir.path(), "source = [\"src/**/*.py\"]\ntimeout = 30")
            .expect("write should succeed");

        let content =
            std::fs::read_to_string(dir.path().join("fest.toml")).expect("read fest.toml");
        assert!(content.starts_with("[fest]\n"));
        assert!(content.contains("source = [\"src/**/*.py\"]"));
    }

    #[test]
    fn write_pyproject_appends_section() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let pyproject = dir.path().join("pyproject.toml");
        std::fs::write(&pyproject, "[project]\nname = \"myapp\"\n").expect("write pyproject");

        write_pyproject_section(dir.path(), "source = [\"src/**/*.py\"]", true)
            .expect("append should succeed");

        let content = std::fs::read_to_string(&pyproject).expect("read pyproject");
        assert!(content.contains("[project]"));
        assert!(content.contains("[tool.fest]"));
        assert!(content.contains("source = [\"src/**/*.py\"]"));
    }

    #[test]
    fn check_existing_fest_toml_errors() {
        let layout = ProjectLayout {
            source_patterns: vec![],
            exclude_patterns: vec![],
            has_pyproject: false,
            has_fest_toml: true,
        };
        let result = check_existing_config(InitFormat::FestToml, &layout);
        assert!(result.is_err());
    }

    #[test]
    fn is_non_source_dir_filters_correctly() {
        assert!(is_non_source_dir("tests"));
        assert!(is_non_source_dir("venv"));
        assert!(is_non_source_dir("docs"));
        assert!(!is_non_source_dir("mypackage"));
        assert!(!is_non_source_dir("flask"));
    }

    #[test]
    fn find_packages_in_empty_dir() {
        let dir = tempfile::tempdir().expect("create temp dir");
        assert!(find_packages_in(dir.path()).is_empty());
    }

    #[test]
    fn find_packages_skips_hidden_dirs() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let hidden = dir.path().join(".hidden");
        std::fs::create_dir_all(&hidden).expect("create dir");
        std::fs::write(hidden.join("__init__.py"), "").expect("write init");

        assert!(find_packages_in(dir.path()).is_empty());
    }

    #[test]
    fn init_end_to_end_fest_toml() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let pkg = dir.path().join("src/myapp");
        std::fs::create_dir_all(&pkg).expect("create dirs");
        std::fs::write(pkg.join("__init__.py"), "").expect("write init");

        let args = InitArgs {
            format: InitFormat::FestToml,
            force: false,
        };
        run(&args, dir.path()).expect("init should succeed");

        let content =
            std::fs::read_to_string(dir.path().join("fest.toml")).expect("read fest.toml");
        assert!(content.contains("[fest]"));
        assert!(content.contains("src/myapp/**/*.py"));
    }

    #[test]
    fn init_end_to_end_pyproject() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let pkg = dir.path().join("src/myapp");
        std::fs::create_dir_all(&pkg).expect("create dirs");
        std::fs::write(pkg.join("__init__.py"), "").expect("write init");
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"test\"\n",
        )
        .expect("write pyproject");

        let args = InitArgs {
            format: InitFormat::Pyproject,
            force: false,
        };
        run(&args, dir.path()).expect("init should succeed");

        let content = std::fs::read_to_string(dir.path().join("pyproject.toml"))
            .expect("read pyproject.toml");
        assert!(content.contains("[tool.fest]"));
        assert!(content.contains("src/myapp/**/*.py"));
    }

    #[test]
    fn init_refuses_overwrite_without_force() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("fest.toml"), "[fest]\n").expect("write existing");

        let args = InitArgs {
            format: InitFormat::FestToml,
            force: false,
        };
        assert!(run(&args, dir.path()).is_err());
    }

    #[test]
    fn init_overwrites_with_force() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("fest.toml"), "[fest]\n").expect("write existing");

        let pkg = dir.path().join("src/myapp");
        std::fs::create_dir_all(&pkg).expect("create dirs");
        std::fs::write(pkg.join("__init__.py"), "").expect("write init");

        let args = InitArgs {
            format: InitFormat::FestToml,
            force: true,
        };
        run(&args, dir.path()).expect("force init should succeed");

        let content =
            std::fs::read_to_string(dir.path().join("fest.toml")).expect("read fest.toml");
        assert!(content.contains("src/myapp/**/*.py"));
    }
}
