//! Direct reader for coverage.py `.coverage` `SQLite` databases.
//!
//! The `.coverage` file produced by `pytest --cov` is a `SQLite` database with
//! tables `file`, `context`, and `line_bits`. Reading it directly avoids the
//! overhead of shelling out to `coverage json --show-contexts`.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use rusqlite::Connection;

use super::CoverageMap;
use crate::Error;

/// Parse a `.coverage` `SQLite` database into a [`CoverageMap`].
///
/// File paths stored in the database are resolved relative to `project_dir`
/// when they are not absolute, matching the behaviour of
/// [`super::json_parser::parse_coverage_json`].
///
/// # Errors
///
/// Returns [`Error::Coverage`] if the database cannot be opened or queried.
pub(super) fn parse_coverage_sqlite(path: &Path, project_dir: &Path) -> Result<CoverageMap, Error> {
    let conn = Connection::open(path).map_err(|err| {
        Error::Coverage(format!(
            "failed to open coverage database {}: {err}",
            path.display()
        ))
    })?;

    let files = load_file_table(&conn, project_dir)?;
    let contexts = load_context_table(&conn)?;
    build_coverage_map(&conn, &files, &contexts)
}

/// Load the `file` table, mapping each file ID to its absolute path.
fn load_file_table(conn: &Connection, project_dir: &Path) -> Result<HashMap<i64, PathBuf>, Error> {
    let mut stmt = conn
        .prepare("SELECT id, path FROM file")
        .map_err(|err| Error::Coverage(format!("failed to query file table: {err}")))?;

    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let path_str: String = row.get(1)?;
            Ok((id, path_str))
        })
        .map_err(|err| Error::Coverage(format!("failed to read file table: {err}")))?;

    let mut map = HashMap::new();
    for row in rows {
        let (id, path_str) =
            row.map_err(|err| Error::Coverage(format!("failed to read file row: {err}")))?;
        let raw_path = PathBuf::from(&path_str);
        let resolved = if raw_path.is_relative() {
            project_dir.join(&raw_path)
        } else {
            raw_path
        };
        let _prev = map.insert(id, resolved);
    }

    Ok(map)
}

/// Load the `context` table, mapping each context ID to its name with the
/// trailing `|run` / `|setup` / `|teardown` suffix stripped.
fn load_context_table(conn: &Connection) -> Result<HashMap<i64, String>, Error> {
    let mut stmt = conn
        .prepare("SELECT id, context FROM context")
        .map_err(|err| Error::Coverage(format!("failed to query context table: {err}")))?;

    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let context: String = row.get(1)?;
            Ok((id, context))
        })
        .map_err(|err| Error::Coverage(format!("failed to read context table: {err}")))?;

    let mut map = HashMap::new();
    for row in rows {
        let (id, context) =
            row.map_err(|err| Error::Coverage(format!("failed to read context row: {err}")))?;
        let stripped = strip_context_suffix(&context);
        let _prev = map.insert(id, stripped);
    }

    Ok(map)
}

/// Build a [`CoverageMap`] by iterating `line_bits` rows and decoding the
/// packed `numbits` bitsets into individual line numbers.
fn build_coverage_map(
    conn: &Connection,
    files: &HashMap<i64, PathBuf>,
    contexts: &HashMap<i64, String>,
) -> Result<CoverageMap, Error> {
    let mut stmt = conn
        .prepare("SELECT file_id, context_id, numbits FROM line_bits")
        .map_err(|err| Error::Coverage(format!("failed to query line_bits table: {err}")))?;

    let rows = stmt
        .query_map([], |row| {
            let file_id: i64 = row.get(0)?;
            let context_id: i64 = row.get(1)?;
            let numbits: Vec<u8> = row.get(2)?;
            Ok((file_id, context_id, numbits))
        })
        .map_err(|err| Error::Coverage(format!("failed to read line_bits table: {err}")))?;

    let mut map: CoverageMap = CoverageMap::new();

    for row in rows {
        let (file_id, context_id, numbits) =
            row.map_err(|err| Error::Coverage(format!("failed to read line_bits row: {err}")))?;

        // Skip rows referencing unknown files/contexts or empty contexts.
        let Some(file_path) = files.get(&file_id) else {
            continue;
        };
        let Some(context_name) = contexts.get(&context_id) else {
            continue;
        };
        if context_name.is_empty() {
            continue;
        }

        let lines = decode_numbits(&numbits);
        for line in lines {
            map.entry((file_path.clone(), line))
                .or_default()
                .push(context_name.clone());
        }
    }

    Ok(map)
}

/// Decode a coverage.py `numbits` blob into a list of 1-based line numbers.
///
/// The format is a packed bitset: byte `i`, bit `j` (LSB = 0) corresponds to
/// line number `i * 8 + j`. Line 0 is not valid in Python source, so the
/// caller should note that coverage.py uses 1-based numbering and the blob
/// stores that directly.
fn decode_numbits(blob: &[u8]) -> Vec<u32> {
    let mut lines = Vec::new();

    for (byte_idx, &byte_val) in blob.iter().enumerate() {
        if byte_val == 0 {
            continue;
        }
        for bit in 0_u32..8_u32 {
            if byte_val & (1_u8 << bit) != 0 {
                #[allow(clippy::cast_possible_truncation, reason = "coverage blobs are small")]
                let line = (byte_idx as u32)
                    .checked_mul(8)
                    .and_then(|base| base.checked_add(bit));
                if let Some(line_number) = line {
                    lines.push(line_number);
                }
            }
        }
    }

    lines
}

/// Strip the `|run`, `|setup`, or `|teardown` suffix that coverage.py stores
/// in raw context names.
///
/// The JSON export already strips these, but the raw `SQLite` database keeps
/// them. We strip here for consistency.
fn strip_context_suffix(context: &str) -> String {
    if let Some(idx) = context.rfind('|') {
        let suffix = context.get(idx..);
        if matches!(suffix, Some("|run" | "|setup" | "|teardown")) {
            return context
                .get(..idx)
                .map_or_else(|| context.to_owned(), str::to_owned);
        }
    }
    context.to_owned()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- decode_numbits -------------------------------------------------------

    #[test]
    fn decode_numbits_empty_blob() {
        assert!(decode_numbits(&[]).is_empty());
    }

    #[test]
    fn decode_numbits_single_byte_all_bits() {
        // 0xFF = all 8 bits set -> lines 0..7
        let lines = decode_numbits(&[0xFF]);
        assert_eq!(lines, vec![0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn decode_numbits_sparse_bits() {
        // byte 0 = 0b0000_0010 -> line 1
        // byte 1 = 0b0000_0100 -> line 10 (1*8 + 2)
        let lines = decode_numbits(&[0x02, 0x04]);
        assert_eq!(lines, vec![1, 10]);
    }

    #[test]
    fn decode_numbits_all_zeros() {
        assert!(decode_numbits(&[0x00, 0x00, 0x00]).is_empty());
    }

    #[test]
    fn decode_numbits_high_line_numbers() {
        // byte 12 = 0b0000_0001 -> line 96 (12*8 + 0)
        let mut blob = vec![0_u8; 13];
        blob[12] = 0x01;
        let lines = decode_numbits(&blob);
        assert_eq!(lines, vec![96]);
    }

    // -- strip_context_suffix -------------------------------------------------

    #[test]
    fn strip_context_suffix_with_run() {
        assert_eq!(
            strip_context_suffix("test_app.py::test_hello|run"),
            "test_app.py::test_hello"
        );
    }

    #[test]
    fn strip_context_suffix_with_setup() {
        assert_eq!(
            strip_context_suffix("test_app.py::test_hello|setup"),
            "test_app.py::test_hello"
        );
    }

    #[test]
    fn strip_context_suffix_with_teardown() {
        assert_eq!(
            strip_context_suffix("test_app.py::test_hello|teardown"),
            "test_app.py::test_hello"
        );
    }

    #[test]
    fn strip_context_suffix_without_pipe() {
        assert_eq!(
            strip_context_suffix("test_app.py::test_hello"),
            "test_app.py::test_hello"
        );
    }

    #[test]
    fn strip_context_suffix_empty() {
        assert_eq!(strip_context_suffix(""), "");
    }

    #[test]
    fn strip_context_suffix_pipe_only() {
        // "|" alone — no recognized suffix after pipe, so kept as-is.
        assert_eq!(strip_context_suffix("|"), "|");
    }

    #[test]
    fn strip_context_suffix_unknown_suffix() {
        // "|other" is not a recognized suffix — kept as-is.
        assert_eq!(
            strip_context_suffix("test.py::test_x|other"),
            "test.py::test_x|other"
        );
    }

    // -- parse_coverage_sqlite ------------------------------------------------

    /// Helper: create a minimal `.coverage` SQLite database.
    fn create_test_db(dir: &Path) -> PathBuf {
        let db_path = dir.join(".coverage");
        let conn = Connection::open(&db_path).expect("open db");

        conn.execute_batch(
            "CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT);
             CREATE TABLE context (id INTEGER PRIMARY KEY, context TEXT);
             CREATE TABLE line_bits (file_id INTEGER, context_id INTEGER, numbits BLOB);
             INSERT INTO file (id, path) VALUES (1, 'src/app.py');
             INSERT INTO context (id, context) VALUES (1, 'test_app.py::test_hello|run');
             INSERT INTO context (id, context) VALUES (2, '');
             INSERT INTO line_bits (file_id, context_id, numbits) VALUES (1, 1, X'0A');
             INSERT INTO line_bits (file_id, context_id, numbits) VALUES (1, 2, X'02');",
        )
        .expect("create and populate db");
        // line_bits for context 1: lines 1 and 3
        //   line 1 -> byte 0 bit 1 = 0x02
        //   line 3 -> byte 0 bit 3 = 0x08
        //   combined: 0x02 | 0x08 = 0x0A
        // line_bits for empty context 2: should be ignored

        db_path
    }

    #[test]
    fn parse_coverage_sqlite_basic() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = create_test_db(dir.path());

        let map = parse_coverage_sqlite(&db_path, dir.path()).expect("should parse");

        let key_line1 = (dir.path().join("src/app.py"), 1_u32);
        let key_line3 = (dir.path().join("src/app.py"), 3_u32);

        assert!(map.contains_key(&key_line1));
        assert!(map.contains_key(&key_line3));

        let tests_line1 = map.get(&key_line1).expect("line 1 present");
        assert_eq!(tests_line1.len(), 1_usize);
        assert_eq!(tests_line1[0_usize], "test_app.py::test_hello");

        // Line 1 from empty context should NOT be in the map
        // (only one entry for line 1, from context 1)
    }

    #[test]
    fn parse_coverage_sqlite_empty_db() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join(".coverage");
        let conn = Connection::open(&db_path).expect("open db");
        conn.execute_batch(
            "CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT);
             CREATE TABLE context (id INTEGER PRIMARY KEY, context TEXT);
             CREATE TABLE line_bits (file_id INTEGER, context_id INTEGER, numbits BLOB);",
        )
        .expect("create tables");
        drop(conn);

        let map = parse_coverage_sqlite(&db_path, dir.path()).expect("should parse empty db");
        assert!(map.is_empty());
    }

    #[test]
    fn parse_coverage_sqlite_missing_file_id() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join(".coverage");
        let conn = Connection::open(&db_path).expect("open db");
        conn.execute_batch(
            "CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT);
             CREATE TABLE context (id INTEGER PRIMARY KEY, context TEXT);
             CREATE TABLE line_bits (file_id INTEGER, context_id INTEGER, numbits BLOB);
             INSERT INTO context (id, context) VALUES (1, 'test.py::test_x|run');
             INSERT INTO line_bits (file_id, context_id, numbits) VALUES (999, 1, X'02');",
        )
        .expect("create tables and insert data");
        drop(conn);

        let map = parse_coverage_sqlite(&db_path, dir.path()).expect("should handle gracefully");
        assert!(map.is_empty());
    }

    #[test]
    fn parse_coverage_sqlite_absolute_path() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join(".coverage");
        let conn = Connection::open(&db_path).expect("open db");
        conn.execute_batch(
            "CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT);
             CREATE TABLE context (id INTEGER PRIMARY KEY, context TEXT);
             CREATE TABLE line_bits (file_id INTEGER, context_id INTEGER, numbits BLOB);
             INSERT INTO file (id, path) VALUES (1, '/absolute/src/app.py');
             INSERT INTO context (id, context) VALUES (1, 'test.py::test_x|run');
             INSERT INTO line_bits (file_id, context_id, numbits) VALUES (1, 1, X'04');",
        )
        .expect("create and populate db");
        // line 2 -> byte 0, bit 2 = 0x04
        drop(conn);

        let map = parse_coverage_sqlite(&db_path, dir.path()).expect("should parse");
        let key = (PathBuf::from("/absolute/src/app.py"), 2_u32);
        assert!(map.contains_key(&key));
    }

    #[test]
    fn parse_coverage_sqlite_corrupt_db() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join(".coverage");
        std::fs::write(&db_path, "not a sqlite database").expect("write corrupt db");

        let result = parse_coverage_sqlite(&db_path, dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn parse_coverage_sqlite_nonexistent_file() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join("no_such_file.coverage");

        let result = parse_coverage_sqlite(&db_path, dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn parse_coverage_sqlite_multiple_contexts_same_line() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join(".coverage");
        let conn = Connection::open(&db_path).expect("open db");
        conn.execute_batch(
            "CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT);
             CREATE TABLE context (id INTEGER PRIMARY KEY, context TEXT);
             CREATE TABLE line_bits (file_id INTEGER, context_id INTEGER, numbits BLOB);
             INSERT INTO file (id, path) VALUES (1, 'mod.py');
             INSERT INTO context (id, context) VALUES (1, 'test.py::test_a|run');
             INSERT INTO context (id, context) VALUES (2, 'test.py::test_b|run');
             INSERT INTO line_bits (file_id, context_id, numbits) VALUES (1, 1, X'20');
             INSERT INTO line_bits (file_id, context_id, numbits) VALUES (1, 2, X'20');",
        )
        .expect("create and populate db");
        // Both contexts cover line 5 (byte 0, bit 5 = 0x20)
        drop(conn);

        let map = parse_coverage_sqlite(&db_path, dir.path()).expect("should parse");
        let key = (dir.path().join("mod.py"), 5_u32);
        let tests = map.get(&key).expect("line 5 present");
        assert_eq!(tests.len(), 2_usize);
    }

    #[test]
    fn parse_coverage_sqlite_missing_context_id() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db_path = dir.path().join(".coverage");
        let conn = Connection::open(&db_path).expect("open db");
        conn.execute_batch(
            "CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT);
             CREATE TABLE context (id INTEGER PRIMARY KEY, context TEXT);
             CREATE TABLE line_bits (file_id INTEGER, context_id INTEGER, numbits BLOB);
             INSERT INTO file (id, path) VALUES (1, 'mod.py');
             INSERT INTO line_bits (file_id, context_id, numbits) VALUES (1, 999, X'02');",
        )
        .expect("create and populate db");
        drop(conn);

        // line_bits references context_id=999 which doesn't exist
        let map = parse_coverage_sqlite(&db_path, dir.path()).expect("should handle gracefully");
        assert!(map.is_empty());
    }

    #[test]
    fn parse_coverage_sqlite_open_error() {
        // Passing a directory as the path should fail to open as a database.
        let dir = tempfile::tempdir().expect("create temp dir");
        let nested = dir
            .path()
            .join("nonexistent_parent/nonexistent_child/.coverage");

        let result = parse_coverage_sqlite(&nested, dir.path());
        assert!(result.is_err());
    }
}
