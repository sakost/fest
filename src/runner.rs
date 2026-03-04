//! Test runner — executing the test suite against each mutant.
//!
//! This module orchestrates parallel test-suite execution, applying
//! mutants, capturing outcomes (killed / survived / timed-out), and
//! feeding results to the report module.
