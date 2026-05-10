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
pub fn scan_source(
    source: &str,
    consumer_module: &str,
    file_path: &std::path::Path,
) -> PluginIndex {
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

/// Collect `from`-import bindings AND reload/dynamic-import warnings
/// from a single top-level statement.
fn collect_from_stmt(
    stmt: &Stmt,
    source: &str,
    consumer_module: &str,
    file_path: &std::path::Path,
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
    walk_stmt_for_calls(stmt, source, file_path, out);
}

/// Recursively walk a statement looking for reload / dynamic-import
/// calls anywhere inside its expression tree.
fn walk_stmt_for_calls(
    stmt: &Stmt,
    source: &str,
    file_path: &std::path::Path,
    out: &mut PluginIndex,
) {
    use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};

    /// Visitor that records every interesting call it sees.
    struct CallVisitor<'a> {
        /// Source for line-number computation.
        source: &'a str,
        /// File path stored in [`ReloadWarning::file`].
        file: &'a std::path::Path,
        /// Output list to which warnings are appended.
        out: &'a mut PluginIndex,
    }

    impl<'a> Visitor<'a> for CallVisitor<'a> {
        fn visit_expr(&mut self, expr: &'a ruff_python_ast::Expr) {
            if let ruff_python_ast::Expr::Call(call) = expr {
                if let Some(kind) = classify_call(&call.func) {
                    let line = line_at(self.source, call.range().start().to_usize());
                    self.out.reload_warnings.push(ReloadWarning {
                        file: self.file.to_path_buf(),
                        line,
                        kind: kind.to_owned(),
                    });
                }
            }
            walk_expr(self, expr);
        }
    }

    let mut visitor = CallVisitor {
        source,
        file: file_path,
        out,
    };
    walk_stmt(&mut visitor, stmt);
}

/// Classify a call expression as `reload` / `import_module` / `__import__`,
/// or `None` for unrelated calls.
fn classify_call(callee: &ruff_python_ast::Expr) -> Option<&'static str> {
    match callee {
        ruff_python_ast::Expr::Attribute(attr) => {
            let leaf = attr.attr.id.as_str();
            let base = match attr.value.as_ref() {
                ruff_python_ast::Expr::Name(n) => n.id.as_str(),
                _ => return None,
            };
            if base == "importlib" && leaf == "reload" {
                Some("reload")
            } else if base == "importlib" && leaf == "import_module" {
                Some("import_module")
            } else {
                None
            }
        }
        ruff_python_ast::Expr::Name(n) if n.id.as_str() == "__import__" => Some("__import__"),
        _ => None,
    }
}

/// Compute 1-based line number of the given byte offset in `source`.
fn line_at(source: &str, byte_offset: usize) -> u32 {
    let upto = source.get(..byte_offset).unwrap_or("");
    u32::try_from(upto.matches('\n').count() + 1).unwrap_or(1)
}

/// Resolve a `from`-import's target module against the consumer's
/// dotted module name, taking relative-import dot-level into account.
fn resolve_import_from(
    consumer_module: &str,
    explicit: &Option<ruff_python_ast::Identifier>,
    level: u32,
) -> String {
    if level == 0 {
        return explicit
            .as_ref()
            .map_or(String::new(), |id| id.id.to_string());
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

/// Walk all `.py` files under `root` and return aggregated [`PluginIndex`].
///
/// # Errors
///
/// Returns [`std::io::Error`] from filesystem operations.
pub fn scan_project(root: &std::path::Path) -> std::io::Result<PluginIndex> {
    let mut out = PluginIndex::default();
    walk_dir(root, root, &mut out)?;
    Ok(out)
}

/// Recursive worker for [`scan_project`].
fn walk_dir(
    root: &std::path::Path,
    cur: &std::path::Path,
    out: &mut PluginIndex,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(cur)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_dir(root, &path, out)?;
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("py") {
            continue;
        }
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let module_name = path_to_module(root, &path);
        let scanned = scan_source(&source, &module_name, &path);
        out.import_bindings.extend(scanned.import_bindings);
        out.reload_warnings.extend(scanned.reload_warnings);
    }
    Ok(())
}

/// Convert a file path under `root` to its dotted Python module name.
fn path_to_module(root: &std::path::Path, file: &std::path::Path) -> String {
    let rel = file.strip_prefix(root).unwrap_or(file);
    let stem = rel.with_extension("");
    let mut parts: Vec<String> = stem
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(ToOwned::to_owned))
        .collect();
    if parts.last().map(String::as_str) == Some("__init__") {
        drop(parts.pop());
    }
    parts.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_source_resolves_bare_relative_one_dot() {
        let src = "from . import sibling\n";
        let index = scan_source(src, "myproj.subpkg.consumer", &PathBuf::from("c.py"));
        assert_eq!(index.import_bindings.len(), 1);
        assert_eq!(index.import_bindings[0].target_module, "myproj.subpkg");
        assert_eq!(index.import_bindings[0].target_name, "sibling");
        assert_eq!(index.import_bindings[0].consumer_key, "sibling");
    }

    #[test]
    fn scan_source_resolves_relative_two_dots() {
        let src = "from ..sibling_pkg import thing\n";
        let index = scan_source(src, "myproj.subpkg.consumer", &PathBuf::from("c.py"));
        assert_eq!(index.import_bindings.len(), 1);
        assert_eq!(index.import_bindings[0].target_module, "myproj.sibling_pkg");
        assert_eq!(index.import_bindings[0].target_name, "thing");
    }

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
        let keys: Vec<_> = index
            .import_bindings
            .iter()
            .map(|b| b.consumer_key.clone())
            .collect();
        assert_eq!(keys, vec!["a", "c", "d"]);
    }

    #[test]
    fn scan_source_resolves_relative_one_dot() {
        let src = "from .sib import x\n";
        let index = scan_source(src, "myproj.subpkg.consumer", &PathBuf::from("c.py"));
        assert_eq!(index.import_bindings[0].target_module, "myproj.subpkg.sib");
    }

    #[test]
    fn scan_source_detects_importlib_reload() {
        let src = "import importlib\nimportlib.reload(foo)\n";
        let index = scan_source(src, "m.c", &PathBuf::from("c.py"));
        assert_eq!(index.reload_warnings.len(), 1);
        assert_eq!(index.reload_warnings[0].kind, "reload");
        assert_eq!(index.reload_warnings[0].line, 2);
    }

    #[test]
    fn scan_source_detects_dynamic_import_module() {
        let src = "import importlib\nimportlib.import_module('foo')\n";
        let index = scan_source(src, "m.c", &PathBuf::from("c.py"));
        assert!(
            index
                .reload_warnings
                .iter()
                .any(|w| w.kind == "import_module")
        );
    }

    #[test]
    fn scan_source_detects_dunder_import() {
        let src = "__import__('foo')\n";
        let index = scan_source(src, "m.c", &PathBuf::from("c.py"));
        assert!(index.reload_warnings.iter().any(|w| w.kind == "__import__"));
    }

    #[test]
    fn scan_project_walks_all_py_files() {
        use tempfile::tempdir;
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        std::fs::write(root.join("pkg/__init__.py"), "").unwrap();
        std::fs::write(root.join("pkg/a.py"), "from pkg.b import x\n").unwrap();
        std::fs::write(root.join("pkg/b.py"), "x = 1\n").unwrap();

        let index = scan_project(root).expect("scan ok");

        assert_eq!(index.import_bindings.len(), 1);
        assert_eq!(index.import_bindings[0].target_module, "pkg.b");
        assert_eq!(index.import_bindings[0].consumer_module, "pkg.a");
    }
}
