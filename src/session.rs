//! Session management — `SQLite`-backed persistence for mutation testing runs.
//!
//! A session stores all generated mutants and their results in a `SQLite`
//! database, enabling stop/resume and incremental workflows.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

use crate::{
    Error,
    mutation::{Mutant, MutantResult, MutantStatus},
};

/// Schema version for migration compatibility.
const SCHEMA_VERSION: &str = "1";

/// A persistent session backed by a `SQLite` database.
#[derive(Debug)]
pub struct Session {
    /// Database connection.
    conn: Connection,
    /// Path to the database file.
    path: PathBuf,
}

impl Session {
    /// Open or create a session database at the given path.
    ///
    /// Creates the schema if the database is new. Verifies schema version
    /// on existing databases.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Session`] if the database cannot be opened or the
    /// schema version is incompatible.
    #[inline]
    pub fn open(path: &Path) -> Result<Self, Error> {
        let conn = Connection::open(path)
            .map_err(|err| Error::Session(format!("failed to open session DB: {err}")))?;

        // Enable WAL mode for better concurrent read performance.
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|err| Error::Session(format!("failed to set WAL mode: {err}")))?;

        let session = Self {
            conn,
            path: path.to_path_buf(),
        };
        session.ensure_schema()?;
        Ok(session)
    }

    /// Path to the session database file.
    #[inline]
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create tables if they don't exist.
    fn ensure_schema(&self) -> Result<(), Error> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS mutants (
                    id INTEGER PRIMARY KEY,
                    file_path TEXT NOT NULL,
                    line INTEGER NOT NULL,
                    col INTEGER NOT NULL,
                    byte_offset INTEGER NOT NULL,
                    byte_length INTEGER NOT NULL,
                    original_text TEXT NOT NULL,
                    mutated_text TEXT NOT NULL,
                    mutator_name TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending',
                    duration_ms INTEGER,
                    error_message TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    UNIQUE(file_path, byte_offset, mutator_name)
                );

                CREATE TABLE IF NOT EXISTS metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );",
            )
            .map_err(|err| Error::Session(format!("failed to create schema: {err}")))?;

        // Set schema version if not present.
        self.set_metadata_if_absent("schema_version", SCHEMA_VERSION)?;
        self.verify_schema_version()?;

        Ok(())
    }

    /// Verify that the schema version matches.
    fn verify_schema_version(&self) -> Result<(), Error> {
        let version = self.get_metadata("schema_version")?;
        match version {
            Some(ver) if ver == SCHEMA_VERSION => Ok(()),
            Some(ver) => Err(Error::Session(format!(
                "incompatible session schema version {ver} (expected {SCHEMA_VERSION})"
            ))),
            None => Err(Error::Session(
                "missing schema_version in session metadata".to_owned(),
            )),
        }
    }

    /// Store a metadata key-value pair, only if the key does not already exist.
    fn set_metadata_if_absent(&self, key: &str, value: &str) -> Result<(), Error> {
        let _rows = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO metadata (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .map_err(|err| Error::Session(format!("failed to set metadata '{key}': {err}")))?;
        Ok(())
    }

    /// Store or update a metadata key-value pair.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Session`] if the database operation fails.
    #[inline]
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<(), Error> {
        let _rows = self
            .conn
            .execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .map_err(|err| Error::Session(format!("failed to set metadata '{key}': {err}")))?;
        Ok(())
    }

    /// Retrieve a metadata value by key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Session`] if the database query fails.
    #[inline]
    pub fn get_metadata(&self, key: &str) -> Result<Option<String>, Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM metadata WHERE key = ?1")
            .map_err(|err| Error::Session(format!("failed to prepare metadata query: {err}")))?;

        let result = stmt.query_row(params![key], |row| row.get(0_usize)).ok();

        Ok(result)
    }

    /// Insert mutants as `pending` into the session, skipping duplicates.
    ///
    /// Uses `INSERT OR IGNORE` so that existing rows (identified by the unique
    /// key `(file_path, byte_offset, mutator_name)`) are preserved with their
    /// current status. This makes session resume safe — completed results
    /// from prior runs are not destroyed.
    ///
    /// The entire batch is wrapped in a transaction for performance.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Session`] if the database operation fails.
    #[inline]
    pub fn store_mutants(&self, mutants: &[Mutant]) -> Result<(), Error> {
        self.conn
            .execute_batch("BEGIN")
            .map_err(|err| Error::Session(format!("failed to begin transaction: {err}")))?;

        let insert_result = self.store_mutants_inner(mutants);

        if insert_result.is_ok() {
            self.conn
                .execute_batch("COMMIT")
                .map_err(|err| Error::Session(format!("failed to commit transaction: {err}")))?;
        } else {
            let _rollback = self.conn.execute_batch("ROLLBACK");
        }

        insert_result
    }

    /// Inner loop for [`store_mutants`] — separated so the caller can
    /// handle transaction commit/rollback.
    fn store_mutants_inner(&self, mutants: &[Mutant]) -> Result<(), Error> {
        let mut stmt = self
            .conn
            .prepare(
                "INSERT OR IGNORE INTO mutants (file_path, line, col, byte_offset, byte_length, \
                 original_text, mutated_text, mutator_name, status) VALUES (?1, ?2, ?3, ?4, ?5, \
                 ?6, ?7, ?8, 'pending')",
            )
            .map_err(|err| Error::Session(format!("failed to prepare insert: {err}")))?;

        for mutant in mutants {
            #[allow(
                clippy::cast_possible_wrap,
                reason = "byte_offset and byte_length fit comfortably in i64"
            )]
            let _inserted = stmt
                .execute(params![
                    mutant.file_path.display().to_string(),
                    mutant.line,
                    mutant.column,
                    mutant.byte_offset as i64,
                    mutant.byte_length as i64,
                    mutant.original_text,
                    mutant.mutated_text,
                    mutant.mutator_name,
                ])
                .map_err(|err| Error::Session(format!("failed to insert mutant: {err}")))?;
        }

        Ok(())
    }

    /// Delete all mutant rows from the session (full reset).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Session`] if the database operation fails.
    #[inline]
    pub fn delete_all_mutants(&self) -> Result<(), Error> {
        let _rows = self
            .conn
            .execute("DELETE FROM mutants", [])
            .map_err(|err| Error::Session(format!("failed to delete mutants: {err}")))?;
        Ok(())
    }

    /// Update a mutant's status after testing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Session`] if the database operation fails.
    #[allow(
        clippy::pattern_type_mismatch,
        reason = "matching on &MutantStatus requires this suppression"
    )]
    #[inline]
    pub fn update_result(&self, result: &MutantResult) -> Result<(), Error> {
        let status_str = status_to_str(&result.status);
        let duration_ms = result.duration.as_millis();
        let error_msg = match &result.status {
            MutantStatus::Error(msg) => Some(msg.as_str()),
            MutantStatus::Killed
            | MutantStatus::Survived
            | MutantStatus::Timeout
            | MutantStatus::NoCoverage => None,
        };

        #[allow(
            clippy::cast_possible_truncation,
            reason = "duration in ms fits in i64 for any realistic test run"
        )]
        let duration_i64 = duration_ms as i64;

        #[allow(
            clippy::cast_possible_wrap,
            reason = "byte_offset fits comfortably in i64"
        )]
        let _rows = self
            .conn
            .execute(
                "UPDATE mutants SET status = ?1, duration_ms = ?2, error_message = ?3 WHERE \
                 file_path = ?4 AND byte_offset = ?5 AND mutator_name = ?6",
                params![
                    status_str,
                    duration_i64,
                    error_msg,
                    result.mutant.file_path.display().to_string(),
                    result.mutant.byte_offset as i64,
                    result.mutant.mutator_name,
                ],
            )
            .map_err(|err| Error::Session(format!("failed to update result: {err}")))?;

        Ok(())
    }

    /// Load mutants that are still pending from the session.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Session`] if the database query fails.
    #[inline]
    pub fn load_pending_mutants(&self) -> Result<Vec<Mutant>, Error> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT file_path, line, col, byte_offset, byte_length, original_text, \
                 mutated_text, mutator_name FROM mutants WHERE status = 'pending' ORDER BY \
                 file_path, byte_offset",
            )
            .map_err(|err| Error::Session(format!("failed to prepare query: {err}")))?;

        let mutants = stmt
            .query_map([], |row| {
                let file_path_str: String = row.get(0_usize)?;
                let byte_offset_i64: i64 = row.get(3_usize)?;
                let byte_length_i64: i64 = row.get(4_usize)?;
                #[allow(
                    clippy::cast_sign_loss,
                    clippy::cast_possible_truncation,
                    reason = "stored values are non-negative and fit in usize"
                )]
                Ok(Mutant {
                    file_path: PathBuf::from(file_path_str),
                    line: row.get(1_usize)?,
                    column: row.get(2_usize)?,
                    byte_offset: byte_offset_i64 as usize,
                    byte_length: byte_length_i64 as usize,
                    original_text: row.get(5_usize)?,
                    mutated_text: row.get(6_usize)?,
                    mutator_name: row.get(7_usize)?,
                })
            })
            .map_err(|err| Error::Session(format!("failed to query pending mutants: {err}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| Error::Session(format!("failed to read mutant row: {err}")))?;

        Ok(mutants)
    }

    /// Load all completed results from the session.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Session`] if the database query fails.
    #[inline]
    pub fn load_completed_results(&self) -> Result<Vec<MutantResult>, Error> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT file_path, line, col, byte_offset, byte_length, original_text, \
                 mutated_text, mutator_name, status, duration_ms, error_message FROM mutants \
                 WHERE status != 'pending' ORDER BY file_path, byte_offset",
            )
            .map_err(|err| Error::Session(format!("failed to prepare query: {err}")))?;

        let results = stmt
            .query_map([], |row| {
                let file_path_str: String = row.get(0_usize)?;
                let byte_offset_i64: i64 = row.get(3_usize)?;
                let byte_length_i64: i64 = row.get(4_usize)?;
                let result_status: String = row.get(8_usize)?;
                let duration_ms: Option<i64> = row.get(9_usize)?;
                let error_message: Option<String> = row.get(10_usize)?;

                #[allow(
                    clippy::cast_sign_loss,
                    clippy::cast_possible_truncation,
                    reason = "stored values are non-negative and fit in usize"
                )]
                let mutant = Mutant {
                    file_path: PathBuf::from(file_path_str),
                    line: row.get(1_usize)?,
                    column: row.get(2_usize)?,
                    byte_offset: byte_offset_i64 as usize,
                    byte_length: byte_length_i64 as usize,
                    original_text: row.get(5_usize)?,
                    mutated_text: row.get(6_usize)?,
                    mutator_name: row.get(7_usize)?,
                };

                #[allow(clippy::cast_sign_loss, reason = "duration_ms is always non-negative")]
                let duration =
                    core::time::Duration::from_millis(duration_ms.unwrap_or(0_i64) as u64);

                Ok(MutantResult {
                    mutant,
                    status: str_to_status(&result_status, error_message),
                    tests_run: Vec::new(),
                    duration,
                })
            })
            .map_err(|err| Error::Session(format!("failed to query results: {err}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| Error::Session(format!("failed to read result row: {err}")))?;

        Ok(results)
    }

    /// Reset all mutant statuses back to `pending`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Session`] if the database operation fails.
    #[inline]
    pub fn reset(&self) -> Result<(), Error> {
        let _rows = self
            .conn
            .execute(
                "UPDATE mutants SET status = 'pending', duration_ms = NULL, error_message = NULL",
                [],
            )
            .map_err(|err| Error::Session(format!("failed to reset session: {err}")))?;
        Ok(())
    }

    /// Reset mutants to `pending` for the given changed files.
    ///
    /// Used for incremental mode: only re-test mutants in changed files.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Session`] if the database operation fails.
    #[inline]
    pub fn reset_stale_files(&self, changed_files: &[PathBuf]) -> Result<usize, Error> {
        if changed_files.is_empty() {
            return Ok(0_usize);
        }

        let mut count = 0_usize;
        for file in changed_files {
            let path_str = file.display().to_string();
            let affected = self
                .conn
                .execute(
                    "UPDATE mutants SET status = 'pending', duration_ms = NULL, error_message = \
                     NULL WHERE file_path = ?1 AND status != 'pending'",
                    params![path_str],
                )
                .map_err(|err| {
                    Error::Session(format!("failed to reset stale file {path_str}: {err}"))
                })?;
            count += affected;
        }

        Ok(count)
    }

    /// Count mutants by status.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Session`] if the database query fails.
    #[inline]
    pub fn count_by_status(&self) -> Result<SessionStats, Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT status, COUNT(*) FROM mutants GROUP BY status")
            .map_err(|err| Error::Session(format!("failed to prepare count query: {err}")))?;

        let mut stats = SessionStats::default();

        let rows = stmt
            .query_map([], |row| {
                let row_status: String = row.get(0_usize)?;
                let count_i64: i64 = row.get(1_usize)?;
                #[allow(
                    clippy::cast_sign_loss,
                    clippy::cast_possible_truncation,
                    reason = "COUNT(*) is always non-negative and fits in usize"
                )]
                let count = count_i64 as usize;
                Ok((row_status, count))
            })
            .map_err(|err| Error::Session(format!("failed to count by status: {err}")))?;

        for row in rows {
            let (row_status, count) =
                row.map_err(|err| Error::Session(format!("failed to read count row: {err}")))?;
            match row_status.as_str() {
                "pending" => stats.pending = count,
                "killed" => stats.killed = count,
                "survived" => stats.survived = count,
                "timeout" => stats.timeout = count,
                "no_coverage" => stats.no_coverage = count,
                "error" => stats.error = count,
                _ => {}
            }
        }

        Ok(stats)
    }

    /// Store run metadata (seed, version) into the session.
    ///
    /// Called at the start of each run to record the current configuration.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Session`] if the database operation fails.
    #[inline]
    pub fn store_run_metadata(&self, seed: Option<u64>, version: &str) -> Result<(), Error> {
        self.set_metadata("fest_version", version)?;
        if let Some(seed_val) = seed {
            self.set_metadata("seed", &seed_val.to_string())?;
        }
        Ok(())
    }
}

/// Summary statistics for a session.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SessionStats {
    /// Number of pending (not yet tested) mutants.
    pub pending: usize,
    /// Number of killed mutants.
    pub killed: usize,
    /// Number of survived mutants.
    pub survived: usize,
    /// Number of timed-out mutants.
    pub timeout: usize,
    /// Number of mutants with no coverage.
    pub no_coverage: usize,
    /// Number of errored mutants.
    pub error: usize,
}

/// Convert a [`MutantStatus`] to a database string.
const fn status_to_str(status: &MutantStatus) -> &'static str {
    match *status {
        MutantStatus::Killed => "killed",
        MutantStatus::Survived => "survived",
        MutantStatus::Timeout => "timeout",
        MutantStatus::NoCoverage => "no_coverage",
        MutantStatus::Error(_) => "error",
    }
}

/// Convert a database status string to a [`MutantStatus`].
fn str_to_status(status: &str, error_message: Option<String>) -> MutantStatus {
    match status {
        "killed" => MutantStatus::Killed,
        "survived" => MutantStatus::Survived,
        "timeout" => MutantStatus::Timeout,
        "no_coverage" => MutantStatus::NoCoverage,
        "error" => MutantStatus::Error(error_message.unwrap_or_default()),
        _ => MutantStatus::Error(format!("unknown status: {status}")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a test mutant.
    fn test_mutant() -> Mutant {
        Mutant {
            file_path: PathBuf::from("src/app.py"),
            line: 1_u32,
            column: 5_u32,
            byte_offset: 4_usize,
            byte_length: 1_usize,
            original_text: "+".to_owned(),
            mutated_text: "-".to_owned(),
            mutator_name: "arithmetic_op".to_owned(),
        }
    }

    /// Opening a session creates the database and schema.
    #[test]
    fn open_creates_schema() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let session = Session::open(&db_path).expect("open session");

        let version = session.get_metadata("schema_version").expect("get version");
        assert_eq!(version, Some(SCHEMA_VERSION.to_owned()));
    }

    /// Re-opening an existing session succeeds.
    #[test]
    fn reopen_existing_session() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");

        let _session1 = Session::open(&db_path).expect("first open");
        drop(_session1);

        let _session2 = Session::open(&db_path).expect("second open");
    }

    /// Store and retrieve pending mutants.
    #[test]
    fn store_and_load_pending() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let session = Session::open(&db_path).expect("open");

        let mutants = vec![test_mutant()];
        session.store_mutants(&mutants).expect("store");

        let pending = session.load_pending_mutants().expect("load");
        assert_eq!(pending.len(), 1_usize);
        assert_eq!(pending[0_usize].file_path, PathBuf::from("src/app.py"));
        assert_eq!(pending[0_usize].original_text, "+");
    }

    /// Update a result and verify it's no longer pending.
    #[test]
    fn update_result_removes_from_pending() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let session = Session::open(&db_path).expect("open");

        let mutant = test_mutant();
        session.store_mutants(&[mutant.clone()]).expect("store");

        let result = MutantResult {
            mutant,
            status: MutantStatus::Killed,
            tests_run: vec!["test_it".to_owned()],
            duration: core::time::Duration::from_millis(100_u64),
        };
        session.update_result(&result).expect("update");

        let pending = session.load_pending_mutants().expect("load");
        assert!(pending.is_empty());

        let completed = session.load_completed_results().expect("load completed");
        assert_eq!(completed.len(), 1_usize);
        assert_eq!(completed[0_usize].status, MutantStatus::Killed);
    }

    /// Reset puts all mutants back to pending.
    #[test]
    fn reset_clears_results() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let session = Session::open(&db_path).expect("open");

        let mutant = test_mutant();
        session.store_mutants(&[mutant.clone()]).expect("store");

        let result = MutantResult {
            mutant,
            status: MutantStatus::Survived,
            tests_run: Vec::new(),
            duration: core::time::Duration::from_secs(1_u64),
        };
        session.update_result(&result).expect("update");
        session.reset().expect("reset");

        let pending = session.load_pending_mutants().expect("load");
        assert_eq!(pending.len(), 1_usize);
    }

    /// Count by status reports correct numbers.
    #[test]
    fn count_by_status_works() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let session = Session::open(&db_path).expect("open");

        let m1 = test_mutant();
        let m2 = Mutant {
            byte_offset: 10_usize,
            ..test_mutant()
        };
        session.store_mutants(&[m1.clone(), m2]).expect("store");

        let result = MutantResult {
            mutant: m1,
            status: MutantStatus::Killed,
            tests_run: Vec::new(),
            duration: core::time::Duration::from_secs(0_u64),
        };
        session.update_result(&result).expect("update");

        let stats = session.count_by_status().expect("stats");
        assert_eq!(stats.pending, 1_usize);
        assert_eq!(stats.killed, 1_usize);
    }

    /// Metadata can be stored and retrieved.
    #[test]
    fn metadata_roundtrip() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let session = Session::open(&db_path).expect("open");

        session.set_metadata("seed", "42").expect("set");
        let val = session.get_metadata("seed").expect("get");
        assert_eq!(val, Some("42".to_owned()));
    }

    /// Missing metadata returns `None`.
    #[test]
    fn metadata_missing_returns_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let session = Session::open(&db_path).expect("open");

        let val = session.get_metadata("nonexistent").expect("get");
        assert_eq!(val, None);
    }

    /// Reset stale files only affects specified files.
    #[test]
    fn reset_stale_files_selective() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let session = Session::open(&db_path).expect("open");

        let m1 = test_mutant();
        let m2 = Mutant {
            file_path: PathBuf::from("src/other.py"),
            byte_offset: 10_usize,
            ..test_mutant()
        };
        session
            .store_mutants(&[m1.clone(), m2.clone()])
            .expect("store");

        // Complete both.
        for mutant in &[m1, m2] {
            let result = MutantResult {
                mutant: mutant.clone(),
                status: MutantStatus::Killed,
                tests_run: Vec::new(),
                duration: core::time::Duration::from_secs(0_u64),
            };
            session.update_result(&result).expect("update");
        }

        // Reset only one file.
        let changed = vec![PathBuf::from("src/app.py")];
        let count = session.reset_stale_files(&changed).expect("reset stale");
        assert_eq!(count, 1_usize);

        let pending = session.load_pending_mutants().expect("load");
        assert_eq!(pending.len(), 1_usize);
        assert_eq!(pending[0_usize].file_path, PathBuf::from("src/app.py"));
    }

    /// `status_to_str` and `str_to_status` roundtrip for all variants.
    #[test]
    fn status_roundtrip() {
        let cases = vec![
            MutantStatus::Killed,
            MutantStatus::Survived,
            MutantStatus::Timeout,
            MutantStatus::NoCoverage,
            MutantStatus::Error("oops".to_owned()),
        ];

        for case_status in cases {
            let s = status_to_str(&case_status);
            let error_msg = match &case_status {
                MutantStatus::Error(msg) => Some(msg.clone()),
                _ => None,
            };
            let back = str_to_status(s, error_msg);
            assert_eq!(back, case_status);
        }
    }

    /// `store_mutants` preserves completed results on re-insert (resume).
    #[test]
    fn store_mutants_preserves_completed_on_resume() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let session = Session::open(&db_path).expect("open");

        let mutant = test_mutant();
        session
            .store_mutants(&[mutant.clone()])
            .expect("first store");

        // Mark as killed.
        let result = MutantResult {
            mutant: mutant.clone(),
            status: MutantStatus::Killed,
            tests_run: Vec::new(),
            duration: core::time::Duration::from_millis(50_u64),
        };
        session.update_result(&result).expect("update");

        // Re-store the same mutant (simulating resume).
        session.store_mutants(&[mutant]).expect("second store");

        // The completed result should still be there (not reset to pending).
        let pending = session.load_pending_mutants().expect("load pending");
        assert!(pending.is_empty());

        let completed = session.load_completed_results().expect("load completed");
        assert_eq!(completed.len(), 1_usize);
        assert_eq!(completed[0_usize].status, MutantStatus::Killed);
    }

    /// `delete_all_mutants` clears all rows.
    #[test]
    fn delete_all_mutants_clears_session() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let session = Session::open(&db_path).expect("open");

        session.store_mutants(&[test_mutant()]).expect("store");
        session.delete_all_mutants().expect("delete");

        let pending = session.load_pending_mutants().expect("load");
        assert!(pending.is_empty());
    }

    /// `store_run_metadata` persists seed and version.
    #[test]
    fn store_run_metadata_persists() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let session = Session::open(&db_path).expect("open");

        session
            .store_run_metadata(Some(42_u64), "0.1.0")
            .expect("store metadata");

        let seed = session.get_metadata("seed").expect("get seed");
        assert_eq!(seed, Some("42".to_owned()));

        let version = session.get_metadata("fest_version").expect("get version");
        assert_eq!(version, Some("0.1.0".to_owned()));
    }

    /// `store_run_metadata` without seed does not store a seed key.
    #[test]
    fn store_run_metadata_no_seed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let session = Session::open(&db_path).expect("open");

        session
            .store_run_metadata(None, "0.1.0")
            .expect("store metadata");

        let seed = session.get_metadata("seed").expect("get seed");
        assert_eq!(seed, None);
    }
}
