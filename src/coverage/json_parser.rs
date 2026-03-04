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
pub(super) fn parse_coverage_json(path: &std::path::Path) -> Result<CoverageMap, Error> {
    let content = std::fs::read_to_string(path).map_err(|err| {
        Error::Coverage(format!(
            "failed to read coverage JSON {}: {err}",
            path.display()
        ))
    })?;

    parse_coverage_json_str(&content)
}

/// Parse coverage JSON from an in-memory string.
///
/// This is the core parser extracted so that tests can exercise it without
/// touching the filesystem.
fn parse_coverage_json_str(json_str: &str) -> Result<CoverageMap, Error> {
    let data: CoverageJson = serde_json::from_str(json_str)
        .map_err(|err| Error::Coverage(format!("failed to parse coverage JSON: {err}")))?;

    let mut map = CoverageMap::new();

    for (file_path_str, file_cov) in &data.files {
        let file_path = PathBuf::from(file_path_str);

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

        let map = parse_coverage_json_str(json).expect("should parse valid JSON");

        let key_line1 = (PathBuf::from("src/app.py"), 1_u32);
        let key_line2 = (PathBuf::from("src/app.py"), 2_u32);
        let key_line3 = (PathBuf::from("src/app.py"), 3_u32);

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

        let map = parse_coverage_json_str(json).expect("should parse");
        assert_eq!(map.len(), 2_usize);

        let key_a = (PathBuf::from("a.py"), 1_u32);
        let key_b = (PathBuf::from("b.py"), 5_u32);
        assert!(map.contains_key(&key_a));
        assert!(map.contains_key(&key_b));
    }

    /// Empty files map yields an empty `CoverageMap`.
    #[test]
    fn parse_empty_files() {
        let json = r#"{ "files": {} }"#;
        let map = parse_coverage_json_str(json).expect("should parse");
        assert!(map.is_empty());
    }

    /// Invalid JSON returns an error.
    #[test]
    fn parse_invalid_json() {
        let result = parse_coverage_json_str("not valid json {{{");
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

        let result = parse_coverage_json_str(json);
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

        let map = parse_coverage_json_str(json).expect("should parse");
        let key = (PathBuf::from("mod.py"), 10_u32);
        let tests = map.get(&key).expect("line 10 present");
        assert_eq!(tests.len(), 1_usize);
        assert_eq!(tests[0_usize], "test_mod.py::test_x");
    }

    /// Parsing from a non-existent file returns an error.
    #[test]
    fn parse_file_not_found() {
        let result = parse_coverage_json(std::path::Path::new("/nonexistent/coverage.json"));
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

        let map = parse_coverage_json(&json_path).expect("should parse from file");
        let key = (PathBuf::from("lib.py"), 7_u32);
        assert!(map.contains_key(&key));
    }
}
