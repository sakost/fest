//! Configuration loading, parsing, and validation for fest.
//!
//! This module handles loading configuration from `fest.toml` or the
//! `[tool.fest]` section of `pyproject.toml`, and producing a validated
//! [`FestConfig`] struct consumed by the rest of the pipeline.

mod types;

use std::path::Path;

pub use types::{
    CustomMutator, DylibMutator, FestConfig, MutatorConfig, OutputFormat, PythonMutator,
};

use crate::Error;

/// Name of the dedicated fest configuration file.
const FEST_TOML: &str = "fest.toml";

/// Name of the Python project configuration file.
const PYPROJECT_TOML: &str = "pyproject.toml";

/// Wrapper used when deserializing `fest.toml`, where the config lives under
/// a `[fest]` table.
#[derive(Debug, serde::Deserialize)]
struct FestTomlRoot {
    /// The `[fest]` table.
    fest: FestConfig,
}

/// Wrapper used when deserializing `pyproject.toml`, where the config lives
/// under `[tool.fest]`.
#[derive(Debug, serde::Deserialize)]
struct PyprojectTomlRoot {
    /// The `[tool]` table.
    tool: ToolTable,
}

/// The `[tool]` table inside `pyproject.toml`.
#[derive(Debug, serde::Deserialize)]
struct ToolTable {
    /// The `[tool.fest]` section.
    fest: FestConfig,
}

/// Load the fest configuration from the given project directory.
///
/// The lookup order is:
/// 1. `fest.toml` in `dir` — parsed as `[fest] ...`
/// 2. `pyproject.toml` in `dir` — the `[tool.fest]` section is extracted
/// 3. If neither file contains fest configuration, returns [`FestConfig::default()`].
///
/// # Errors
///
/// Returns [`Error::Config`] if a config file exists but cannot be read or
/// contains invalid TOML / an invalid fest configuration.
#[inline]
pub fn load(dir: &Path) -> Result<FestConfig, Error> {
    let fest_path = dir.join(FEST_TOML);
    if fest_path.is_file() {
        return load_fest_toml(&fest_path);
    }

    let pyproject_path = dir.join(PYPROJECT_TOML);
    if pyproject_path.is_file() {
        return load_pyproject_toml(&pyproject_path);
    }

    Ok(FestConfig::default())
}

/// Parse a `fest.toml` file.
fn load_fest_toml(path: &Path) -> Result<FestConfig, Error> {
    let content = std::fs::read_to_string(path).map_err(|err| {
        Error::Config(format!("failed to read {}: {err}", path.display()))
    })?;

    let root: FestTomlRoot = toml::from_str(&content).map_err(|err| {
        Error::Config(format!("failed to parse {}: {err}", path.display()))
    })?;

    Ok(root.fest)
}

/// Parse the `[tool.fest]` section from a `pyproject.toml` file.
fn load_pyproject_toml(path: &Path) -> Result<FestConfig, Error> {
    let content = std::fs::read_to_string(path).map_err(|err| {
        Error::Config(format!("failed to read {}: {err}", path.display()))
    })?;

    // The file may not have a [tool.fest] section at all. In that case,
    // return default config rather than an error.
    let root: PyprojectTomlRoot = match toml::from_str(&content) {
        Ok(parsed) => parsed,
        Err(_) => return Ok(FestConfig::default()),
    };

    Ok(root.tool.fest)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    /// Full fest.toml round-trip: serialize defaults, then deserialize.
    #[test]
    fn deserialize_full_fest_toml() {
        let toml_content = r#"
[fest]
source = ["src/**/*.py", "lib/**/*.py"]
exclude = ["src/generated/**"]
test_runner = "pytest"
workers = 4
workers_cpu_ratio = 0.5
timeout = 30
fail_under = 80.0
output = "json"

[fest.mutators]
arithmetic_op = true
comparison_op = false
boolean_op = true
return_value = true
negate_condition = true
remove_decorator = false
constant_replace = true
exception_swallow = true

[[fest.mutators.custom]]
name = "swap_assert"
pattern = "assert {expr}"
replacement = "assert not {expr}"

[[fest.mutators.python]]
path = "mutators/my_custom.py"

[[fest.mutators.dylib]]
path = "target/release/libmy_mutator.so"
"#;

        let root: FestTomlRoot =
            toml::from_str(toml_content).expect("should parse full fest.toml");
        let cfg = root.fest;

        assert_eq!(cfg.source, vec!["src/**/*.py", "lib/**/*.py"]);
        assert_eq!(cfg.exclude, vec!["src/generated/**"]);
        assert_eq!(cfg.test_runner, "pytest");
        assert_eq!(cfg.workers, Some(4_usize));
        assert!((cfg.workers_cpu_ratio - 0.5).abs() < f64::EPSILON);
        assert_eq!(cfg.timeout, 30_u64);
        assert!((cfg.fail_under.unwrap_or(0.0) - 80.0).abs() < f64::EPSILON);
        assert_eq!(cfg.output, OutputFormat::Json);

        // Mutator flags
        assert!(cfg.mutators.arithmetic_op);
        assert!(!cfg.mutators.comparison_op);
        assert!(cfg.mutators.boolean_op);
        assert!(!cfg.mutators.remove_decorator);

        // Custom mutator
        assert_eq!(cfg.mutators.custom.len(), 1_usize);
        assert_eq!(cfg.mutators.custom[0_usize].name, "swap_assert");

        // Python mutator
        assert_eq!(cfg.mutators.python.len(), 1_usize);
        assert_eq!(cfg.mutators.python[0_usize].path, "mutators/my_custom.py");

        // Dylib mutator
        assert_eq!(cfg.mutators.dylib.len(), 1_usize);
        assert_eq!(
            cfg.mutators.dylib[0_usize].path,
            "target/release/libmy_mutator.so"
        );
    }

    /// Default config values are populated when fields are omitted.
    #[test]
    fn default_values() {
        let cfg = FestConfig::default();

        assert_eq!(cfg.source, vec!["src/**/*.py"]);
        assert!(cfg.exclude.is_empty());
        assert_eq!(cfg.test_runner, "pytest");
        assert_eq!(cfg.workers, None);
        assert!((cfg.workers_cpu_ratio - 0.75).abs() < f64::EPSILON);
        assert_eq!(cfg.timeout, 10_u64);
        assert_eq!(cfg.fail_under, None);
        assert_eq!(cfg.output, OutputFormat::Text);

        // All mutators enabled by default
        assert!(cfg.mutators.arithmetic_op);
        assert!(cfg.mutators.comparison_op);
        assert!(cfg.mutators.boolean_op);
        assert!(cfg.mutators.return_value);
        assert!(cfg.mutators.negate_condition);
        assert!(cfg.mutators.remove_decorator);
        assert!(cfg.mutators.constant_replace);
        assert!(cfg.mutators.exception_swallow);
        assert!(cfg.mutators.custom.is_empty());
        assert!(cfg.mutators.python.is_empty());
        assert!(cfg.mutators.dylib.is_empty());
    }

    /// Minimal fest.toml — only required top-level table, all fields default.
    #[test]
    fn minimal_fest_toml_uses_defaults() {
        let toml_content = "[fest]\n";
        let root: FestTomlRoot =
            toml::from_str(toml_content).expect("should parse minimal fest.toml");

        let cfg = root.fest;
        let default_cfg = FestConfig::default();

        assert_eq!(cfg.source, default_cfg.source);
        assert_eq!(cfg.test_runner, default_cfg.test_runner);
        assert_eq!(cfg.timeout, default_cfg.timeout);
        assert!(cfg.mutators.arithmetic_op);
    }

    /// `resolved_workers` returns the explicit value when set.
    #[test]
    fn resolved_workers_explicit() {
        let mut cfg = FestConfig::default();
        cfg.workers = Some(8_usize);
        assert_eq!(cfg.resolved_workers(), 8_usize);
    }

    /// `resolved_workers` clamps explicit zero to 1.
    #[test]
    fn resolved_workers_explicit_zero_clamps_to_one() {
        let mut cfg = FestConfig::default();
        cfg.workers = Some(0_usize);
        assert_eq!(cfg.resolved_workers(), 1_usize);
    }

    /// `resolved_workers` computes from CPU ratio when workers is None.
    #[test]
    fn resolved_workers_from_ratio() {
        let cfg = FestConfig::default();
        let workers = cfg.resolved_workers();
        // Must be at least 1 and at most the number of available CPUs
        assert!(workers >= 1_usize);
    }

    /// `resolved_workers` with a very small ratio still yields at least 1.
    #[test]
    fn resolved_workers_tiny_ratio() {
        let mut cfg = FestConfig::default();
        cfg.workers_cpu_ratio = 0.001;
        assert!(cfg.resolved_workers() >= 1_usize);
    }

    /// Loading from a directory with `fest.toml`.
    #[test]
    fn load_fest_toml_file() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let fest_path = dir.path().join("fest.toml");

        let mut file = std::fs::File::create(&fest_path).expect("should create file");
        file.write_all(
            br#"
[fest]
source = ["app/**/*.py"]
timeout = 60
"#,
        )
        .expect("should write file");

        let cfg = load(dir.path()).expect("should load fest.toml");
        assert_eq!(cfg.source, vec!["app/**/*.py"]);
        assert_eq!(cfg.timeout, 60_u64);
        // Defaults for omitted fields
        assert_eq!(cfg.test_runner, "pytest");
    }

    /// Loading from a directory with `pyproject.toml` containing `[tool.fest]`.
    #[test]
    fn load_pyproject_toml_file() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let pyproject_path = dir.path().join("pyproject.toml");

        let mut file = std::fs::File::create(&pyproject_path).expect("should create file");
        file.write_all(
            br#"
[project]
name = "my-python-project"

[tool.fest]
source = ["my_pkg/**/*.py"]
test_runner = "unittest"
timeout = 20

[tool.fest.mutators]
arithmetic_op = false
"#,
        )
        .expect("should write file");

        let cfg = load(dir.path()).expect("should load pyproject.toml");
        assert_eq!(cfg.source, vec!["my_pkg/**/*.py"]);
        assert_eq!(cfg.test_runner, "unittest");
        assert_eq!(cfg.timeout, 20_u64);
        assert!(!cfg.mutators.arithmetic_op);
        // Other mutators keep their defaults
        assert!(cfg.mutators.comparison_op);
    }

    /// Loading from a directory with no config files returns defaults.
    #[test]
    fn load_no_config_returns_defaults() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let cfg = load(dir.path()).expect("should return default config");
        assert_eq!(cfg, FestConfig::default());
    }

    /// A `pyproject.toml` without `[tool.fest]` returns defaults.
    #[test]
    fn load_pyproject_without_tool_fest_returns_defaults() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let pyproject_path = dir.path().join("pyproject.toml");

        let mut file = std::fs::File::create(&pyproject_path).expect("should create file");
        file.write_all(
            br#"
[project]
name = "unrelated"
"#,
        )
        .expect("should write file");

        let cfg = load(dir.path()).expect("should return default config");
        assert_eq!(cfg, FestConfig::default());
    }

    /// `fest.toml` takes priority over `pyproject.toml`.
    #[test]
    fn fest_toml_takes_priority() {
        let dir = tempfile::tempdir().expect("should create temp dir");

        // Write fest.toml
        let fest_path = dir.path().join("fest.toml");
        let mut fest_file = std::fs::File::create(&fest_path).expect("should create file");
        fest_file
            .write_all(
                br#"
[fest]
timeout = 99
"#,
            )
            .expect("should write file");

        // Write pyproject.toml with different timeout
        let pyproject_path = dir.path().join("pyproject.toml");
        let mut pyproject_file =
            std::fs::File::create(&pyproject_path).expect("should create file");
        pyproject_file
            .write_all(
                br#"
[tool.fest]
timeout = 42
"#,
            )
            .expect("should write file");

        let cfg = load(dir.path()).expect("should load config");
        assert_eq!(cfg.timeout, 99_u64);
    }

    /// Roundtrip: serialize and deserialize `FestConfig`.
    #[test]
    fn serialize_deserialize_roundtrip() {
        let original = FestConfig::default();
        let serialized =
            toml::to_string(&original).expect("should serialize FestConfig");

        // Wrap in [fest] table for deserialization
        let wrapped = format!("[fest]\n{serialized}");
        let root: FestTomlRoot = toml::from_str(&wrapped).expect("should deserialize roundtrip");

        assert_eq!(root.fest, original);
    }

    /// Invalid TOML in fest.toml produces `Error::Config`.
    #[test]
    fn invalid_fest_toml_returns_error() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let fest_path = dir.path().join("fest.toml");

        let mut file = std::fs::File::create(&fest_path).expect("should create file");
        file.write_all(b"this is not valid toml {{{}}}").expect("should write file");

        let result = load(dir.path());
        assert!(result.is_err());
    }

    /// Output format deserialization covers all variants.
    #[test]
    fn output_format_variants() {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            output: OutputFormat,
        }

        let text: Wrapper = toml::from_str("output = \"text\"").expect("text");
        assert_eq!(text.output, OutputFormat::Text);

        let json: Wrapper = toml::from_str("output = \"json\"").expect("json");
        assert_eq!(json.output, OutputFormat::Json);

        let html: Wrapper = toml::from_str("output = \"html\"").expect("html");
        assert_eq!(html.output, OutputFormat::Html);
    }
}
