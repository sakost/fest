//! Project-wide AST scan that produces inputs for the plugin's
//! reverse-import index and reload-warnings list.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One `from X import Y [as Z]` binding seen in project source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportBinding {
    /// Dotted module name of the consumer (the file that contains the import).
    pub consumer_module: String,
    /// Local name in the consumer (the alias if `as Z` was used, else `Y`).
    pub consumer_key: String,
    /// Resolved absolute module name being imported from.
    pub target_module: String,
    /// The imported name as written in the target module.
    pub target_name: String,
}

/// One occurrence of a call that compromises plugin-backend accuracy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadWarning {
    /// Source file where the call appears.
    pub file: PathBuf,
    /// 1-based line number of the call.
    pub line: u32,
    /// Which call: `"reload"`, `"import_module"`, or `"__import__"`.
    pub kind: String,
}

/// Aggregated output of [`scan_project`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginIndex {
    /// All `from`-import bindings discovered in project source.
    pub import_bindings: Vec<ImportBinding>,
    /// All occurrences of `importlib.reload` / dynamic-import calls.
    pub reload_warnings: Vec<ReloadWarning>,
}
