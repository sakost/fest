//! Project-wide AST scan that produces inputs for the plugin's
//! reverse-import index and reload-warnings list.

use std::path::PathBuf;

use ruff_python_ast::{ModModule, Stmt};
use ruff_python_parser::parse_module;
use ruff_text_size::Ranged;
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

/// Parse a single source file and emit its [`PluginIndex`] contribution.
#[must_use]
pub fn scan_source(source: &str, consumer_module: &str, file_path: &std::path::Path) -> PluginIndex {
    let parsed = match parse_module(source) {
        Ok(p) => p,
        Err(_) => return PluginIndex::default(),
    };
    let ast: ModModule = parsed.into_syntax();
    let mut out = PluginIndex::default();
    for stmt in &ast.body {
        collect_from_stmt(stmt, source, consumer_module, file_path, &mut out);
    }
    out
}

/// Collect `from`-import bindings from a single top-level statement.
fn collect_from_stmt(
    stmt: &Stmt,
    _source: &str,
    consumer_module: &str,
    _file_path: &std::path::Path,
    out: &mut PluginIndex,
) {
    if let Stmt::ImportFrom(import) = stmt {
        let level = u32::from(import.level);
        let target_module = resolve_import_from(consumer_module, &import.module, level);
        for alias in &import.names {
            let target_name = alias.name.id.to_string();
            let consumer_key = alias
                .asname
                .as_ref()
                .map_or_else(|| target_name.clone(), |a| a.id.to_string());
            out.import_bindings.push(ImportBinding {
                consumer_module: consumer_module.to_owned(),
                consumer_key,
                target_module: target_module.clone(),
                target_name,
            });
        }
    }
}

/// Resolve a `from`-import's target module against the consumer's
/// dotted module name, taking relative-import dot-level into account.
fn resolve_import_from(
    consumer_module: &str,
    explicit: &Option<ruff_python_ast::Identifier>,
    level: u32,
) -> String {
    if level == 0 {
        return explicit.as_ref().map_or(String::new(), |id| id.id.to_string());
    }
    let parts: Vec<&str> = consumer_module.split('.').collect();
    let drop = level as usize;
    let prefix_end = parts.len().saturating_sub(drop);
    let mut prefix: String = parts[..prefix_end].join(".");
    if let Some(extra) = explicit {
        if !prefix.is_empty() {
            prefix.push('.');
        }
        prefix.push_str(extra.id.as_str());
    }
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_source_extracts_simple_from_import() {
        let src = "from foo import bar\n";
        let index = scan_source(src, "myproj.consumer", &PathBuf::from("c.py"));
        assert_eq!(index.import_bindings.len(), 1);
        assert_eq!(index.import_bindings[0].target_module, "foo");
        assert_eq!(index.import_bindings[0].target_name, "bar");
        assert_eq!(index.import_bindings[0].consumer_key, "bar");
    }

    #[test]
    fn scan_source_handles_alias() {
        let src = "from foo import bar as baz\n";
        let index = scan_source(src, "m.c", &PathBuf::from("c.py"));
        assert_eq!(index.import_bindings[0].consumer_key, "baz");
        assert_eq!(index.import_bindings[0].target_name, "bar");
    }

    #[test]
    fn scan_source_handles_multi_name_import() {
        let src = "from foo import a, b as c, d\n";
        let index = scan_source(src, "m.c", &PathBuf::from("c.py"));
        let keys: Vec<_> = index.import_bindings.iter().map(|b| b.consumer_key.clone()).collect();
        assert_eq!(keys, vec!["a", "c", "d"]);
    }

    #[test]
    fn scan_source_resolves_relative_one_dot() {
        let src = "from .sib import x\n";
        let index = scan_source(src, "myproj.subpkg.consumer", &PathBuf::from("c.py"));
        assert_eq!(index.import_bindings[0].target_module, "myproj.subpkg.sib");
    }
}
