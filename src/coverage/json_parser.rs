//! JSON-based coverage data parser.
//!
//! Parses the JSON output produced by `coverage json --show-contexts`, which
//! maps source files and line numbers to the test contexts that exercised them.

use std::{collections::HashMap, path::PathBuf};

use serde::Deserialize;

use super::CoverageMap;
use crate::Error;

/// Root structure of the `coverage json --show-contexts` output.
#[derive(Debug, Deserialize)]
struct CoverageJson {
    /// Per-file coverage data keyed by relative file path.
    files: HashMap<String, FileCoverage>,
}

/// Coverage data for a single source file.
#[derive(Debug, Deserialize)]
struct FileCoverage {
    /// Mapping from line number (as a string key) to the list of test context
    /// names that executed that line.
    contexts: HashMap<String, Vec<String>>,
}

/// Parse a `coverage json --show-contexts` JSON file into a [`CoverageMap`].
///
/// Each entry in the resulting map associates a `(file, line)` pair with the
/// list of test IDs (context names) that covered that line. Empty-context
/// entries (lines with no identified test) are excluded.
///
/// # Errors
///
/// Returns [`Error::Coverage`] if the file cannot be read or the JSON is
/// malformed.
pub(super) fn parse_coverage_json(
    path: &std::path::Path,
    project_dir: &std::path::Path,
) -> Result<CoverageMap, Error> {
    let content = std::fs::read_to_string(path).map_err(|err| {
        Error::Coverage(format!(
            "failed to read coverage JSON {}: {err}",
            path.display()
        ))
    })?;

    parse_coverage_json_str(&content, project_dir)
}

/// Parse coverage JSON from an in-memory string.
///
/// File paths in the coverage JSON are relative to the project directory.
/// They are resolved to absolute paths so that they match the absolute paths
/// produced by [`crate::mutation::discover_files`].
fn parse_coverage_json_str(
    json_str: &str,
    project_dir: &std::path::Path,
) -> Result<CoverageMap, Error> {
    let data: CoverageJson = serde_json::from_str(json_str)
        .map_err(|err| Error::Coverage(format!("failed to parse coverage JSON: {err}")))?;

    let mut map = CoverageMap::new();

    for (file_path_str, file_cov) in &data.files {
        let raw_path = PathBuf::from(file_path_str);
        let file_path = if raw_path.is_relative() {
            project_dir.join(&raw_path)
        } else {
            raw_path
        };

        for (line_str, contexts) in &file_cov.contexts {
            let line_number: u32 = line_str.parse::<u32>().map_err(|err| {
                Error::Coverage(format!(
                    "invalid line number '{line_str}' in coverage JSON: {err}"
                ))
            })?;

            // Filter out empty context strings (coverage.py uses "" for
            // lines executed outside any test context).
            let test_ids: Vec<String> = contexts
                .iter()
                .filter(|ctx| !ctx.is_empty())
                .cloned()
                .collect();

            if !test_ids.is_empty() {
                let _prev = map.insert((file_path.clone(), line_number), test_ids);
            }
        }
    }

    Ok(map)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid JSON with one file and two lines.
    #[test]
    fn parse_minimal_json() {
        let json = r#"{
            "meta": {},
            "files": {
                "src/app.py": {
                    "executed_lines": [1, 2, 3],
                    "contexts": {
                        "1": ["test_app.py::test_hello"],
                        "2": ["test_app.py::test_hello", "test_app.py::test_world"],
                        "3": [""]
                    }
                }
            }
        }"#;

        let project_dir = std::path::Path::new("/project");
        let map = parse_coverage_json_str(json, project_dir).expect("should parse valid JSON");

        let key_line1 = (PathBuf::from("/project/src/app.py"), 1_u32);
        let key_line2 = (PathBuf::from("/project/src/app.py"), 2_u32);
        let key_line3 = (PathBuf::from("/project/src/app.py"), 3_u32);

        assert_eq!(map.get(&key_line1).expect("line 1 present").len(), 1_usize);
        assert_eq!(map.get(&key_line2).expect("line 2 present").len(), 2_usize);
        // Line 3 had only an empty context, so it should be absent.
        assert!(map.get(&key_line3).is_none());
    }

    /// Multiple files are parsed correctly.
    #[test]
    fn parse_multiple_files() {
        let json = r#"{
            "files": {
                "a.py": {
                    "executed_lines": [1],
                    "contexts": {
                        "1": ["test_a.py::test_one"]
                    }
                },
                "b.py": {
                    "executed_lines": [5],
                    "contexts": {
                        "5": ["test_b.py::test_five"]
                    }
                }
            }
        }"#;

        let project_dir = std::path::Path::new("/proj");
        let map = parse_coverage_json_str(json, project_dir).expect("should parse");
        assert_eq!(map.len(), 2_usize);

        let key_a = (PathBuf::from("/proj/a.py"), 1_u32);
        let key_b = (PathBuf::from("/proj/b.py"), 5_u32);
        assert!(map.contains_key(&key_a));
        assert!(map.contains_key(&key_b));
    }

    /// Empty files map yields an empty `CoverageMap`.
    #[test]
    fn parse_empty_files() {
        let json = r#"{ "files": {} }"#;
        let project_dir = std::path::Path::new("/proj");
        let map = parse_coverage_json_str(json, project_dir).expect("should parse");
        assert!(map.is_empty());
    }

    /// Invalid JSON returns an error.
    #[test]
    fn parse_invalid_json() {
        let project_dir = std::path::Path::new("/proj");
        let result = parse_coverage_json_str("not valid json {{{", project_dir);
        assert!(result.is_err());
    }

    /// Invalid line number string returns an error.
    #[test]
    fn parse_invalid_line_number() {
        let json = r#"{
            "files": {
                "src/app.py": {
                    "executed_lines": [],
                    "contexts": {
                        "not_a_number": ["test_a.py::test_one"]
                    }
                }
            }
        }"#;

        let project_dir = std::path::Path::new("/proj");
        let result = parse_coverage_json_str(json, project_dir);
        assert!(result.is_err());
    }

    /// Contexts with a mix of empty and non-empty entries only keep non-empty.
    #[test]
    fn parse_filters_empty_contexts() {
        let json = r#"{
            "files": {
                "mod.py": {
                    "executed_lines": [10],
                    "contexts": {
                        "10": ["", "test_mod.py::test_x", ""]
                    }
                }
            }
        }"#;

        let project_dir = std::path::Path::new("/proj");
        let map = parse_coverage_json_str(json, project_dir).expect("should parse");
        let key = (PathBuf::from("/proj/mod.py"), 10_u32);
        let tests = map.get(&key).expect("line 10 present");
        assert_eq!(tests.len(), 1_usize);
        assert_eq!(tests[0_usize], "test_mod.py::test_x");
    }

    /// Parsing from a non-existent file returns an error.
    #[test]
    fn parse_file_not_found() {
        let project_dir = std::path::Path::new("/proj");
        let result = parse_coverage_json(
            std::path::Path::new("/nonexistent/coverage.json"),
            project_dir,
        );
        assert!(result.is_err());
    }

    /// Round-trip: write JSON to a temp file and parse it back.
    #[test]
    fn parse_from_temp_file() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().expect("create temp dir");
        let json_path = dir.path().join("coverage.json");

        let json_content = r#"{
            "files": {
                "lib.py": {
                    "executed_lines": [7],
                    "contexts": {
                        "7": ["test_lib.py::test_func"]
                    }
                }
            }
        }"#;

        {
            let mut file = std::fs::File::create(&json_path).expect("create file");
            file.write_all(json_content.as_bytes()).expect("write file");
        }

        let map = parse_coverage_json(&json_path, dir.path()).expect("should parse from file");
        let key = (dir.path().join("lib.py"), 7_u32);
        assert!(map.contains_key(&key));
    }
}
