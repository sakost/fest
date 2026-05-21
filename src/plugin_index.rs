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
    /// Module → static `__all__` literal contents, when expressible as a
    /// list/tuple of string literals. Modules with dynamic `__all__` or no
    /// `__all__` are absent.
    #[serde(default)]
    pub module_exports: std::collections::HashMap<String, Vec<String>>,
    /// Star-imports awaiting runtime resolution (source module had no
    /// static `__all__` we could parse). Populated by Task 13.
    #[serde(default)]
    pub pending_star_imports: Vec<PendingStarImport>,
}

/// A `from X import *` binding that couldn't be statically resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingStarImport {
    /// Dotted name of the consumer (the file containing `from X import *`).
    pub consumer_module: String,
    /// Resolved source module name.
    pub target_module: String,
}

impl PluginIndex {
    /// Merge another index into this one.
    #[inline]
    pub fn merge(&mut self, other: Self) {
        self.import_bindings.extend(other.import_bindings);
        self.reload_warnings.extend(other.reload_warnings);
        self.module_exports.extend(other.module_exports);
        self.pending_star_imports.extend(other.pending_star_imports);
    }

    /// Walk every recorded `from X import *` placeholder and either synthesize
    /// one named [`ImportBinding`] per name in the target's static `__all__`
    /// (when [`PluginIndex::module_exports`] has it), or convert the
    /// placeholder to a [`PendingStarImport`] for the plugin to resolve at
    /// runtime.
    #[inline]
    pub fn resolve_star_imports(&mut self) {
        // Partition into placeholders and real bindings without borrowing
        // self twice. Two owned Vecs, then write back.
        let owned = core::mem::take(&mut self.import_bindings);
        let mut placeholders: Vec<ImportBinding> = Vec::new();
        let mut kept: Vec<ImportBinding> = Vec::with_capacity(owned.len());
        for binding in owned {
            if binding.target_name == "*" {
                placeholders.push(binding);
            } else {
                kept.push(binding);
            }
        }
        self.import_bindings = kept;
        for ph in placeholders {
            if let Some(names) = self.module_exports.get(&ph.target_module).cloned() {
                for name in names {
                    self.import_bindings.push(ImportBinding {
                        consumer_module: ph.consumer_module.clone(),
                        consumer_key: name.clone(),
                        target_module: ph.target_module.clone(),
                        target_name: name,
                    });
                }
            } else {
                self.pending_star_imports.push(PendingStarImport {
                    consumer_module: ph.consumer_module,
                    target_module: ph.target_module,
                });
            }
        }
    }
}

/// Parse a single source file and emit its [`PluginIndex`] contribution.
#[inline]
#[must_use]
pub fn scan_source(
    source: &str,
    consumer_module: &str,
    file_path: &std::path::Path,
) -> PluginIndex {
    let Ok(parsed) = parse_module(source) else {
        return PluginIndex::default();
    };
    let ast: ModModule = parsed.into_syntax();
    let mut out = PluginIndex::default();
    for stmt in &ast.body {
        collect_from_stmt(stmt, source, consumer_module, file_path, &mut out);
    }
    if let Some(names) = extract_static_all(&ast) {
        let _prev = out.module_exports.insert(consumer_module.to_owned(), names);
    }
    out
}

/// Extract a static `__all__` list from a parsed module body.
///
/// Returns `Some(names)` only when `__all__` is assigned a list or tuple
/// literal whose elements are all string literals. Any other shape
/// (function call, list comprehension, conditional, non-string elements)
/// returns `None`, and the caller treats the module's `__all__` as dynamic.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt and &Expr references from AST iteration; adding & is more verbose"
)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "only List and Tuple are valid static __all__ shapes; all other Expr variants \
              continue"
)]
fn extract_static_all(ast: &ModModule) -> Option<Vec<String>> {
    use ruff_python_ast::{Expr, Stmt};
    for stmt in &ast.body {
        let (target, value): (&Expr, &Expr) = match stmt {
            Stmt::Assign(assign) => {
                // Single bare-Name target __all__ only.
                let [target] = assign.targets.as_slice() else {
                    continue;
                };
                (target, &*assign.value)
            }
            Stmt::AnnAssign(ann) => {
                // `__all__: list[str] = [...]` — value must be present.
                let Some(value) = ann.value.as_deref() else {
                    continue;
                };
                (&*ann.target, value)
            }
            _ => continue,
        };
        let Expr::Name(name_expr) = target else {
            continue;
        };
        if name_expr.id.as_str() != "__all__" {
            continue;
        }
        let elts: &[Expr] = match value {
            Expr::List(list) => &list.elts,
            Expr::Tuple(tup) => &tup.elts,
            _ => continue,
        };
        let mut names = Vec::with_capacity(elts.len());
        for elt in elts {
            let Expr::StringLiteral(s) = elt else {
                return None;
            };
            names.push(s.value.to_str().to_owned());
        }
        return Some(names);
    }
    None
}

/// Collect `from`-import bindings AND reload/dynamic-import warnings
/// from a single top-level statement.
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Stmt reference; adding & to pattern is more verbose"
)]
fn collect_from_stmt(
    stmt: &Stmt,
    source: &str,
    consumer_module: &str,
    file_path: &std::path::Path,
    out: &mut PluginIndex,
) {
    if let Stmt::ImportFrom(import) = stmt {
        let level = import.level;
        let target_module = resolve_import_from(consumer_module, import.module.as_ref(), level);
        for alias in &import.names {
            let target_name = alias.name.id.to_string();
            // `from X import *` — emit a star placeholder for later resolution.
            if target_name == "*" {
                out.import_bindings.push(ImportBinding {
                    consumer_module: consumer_module.to_owned(),
                    consumer_key: "*".to_owned(),
                    target_module: target_module.clone(),
                    target_name: "*".to_owned(),
                });
                continue;
            }
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
    struct CallVisitor<'src> {
        /// Source for line-number computation.
        source: &'src str,
        /// File path stored in [`ReloadWarning::file`].
        file: &'src std::path::Path,
        /// Output list to which warnings are appended.
        out: &'src mut PluginIndex,
    }

    impl<'src> Visitor<'src> for CallVisitor<'src> {
        #[allow(
            clippy::pattern_type_mismatch,
            reason = "matching on &Expr reference from visitor; adding & to pattern is more \
                      verbose"
        )]
        fn visit_expr(&mut self, expr: &'src ruff_python_ast::Expr) {
            if let ruff_python_ast::Expr::Call(call) = expr
                && let Some(kind) = classify_call(&call.func)
            {
                let line = line_at(self.source, call.range().start().to_usize());
                self.out.reload_warnings.push(ReloadWarning {
                    file: self.file.to_path_buf(),
                    line,
                    kind: kind.to_owned(),
                });
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
#[allow(
    clippy::pattern_type_mismatch,
    reason = "matching on &Expr references; adding & to each arm is more verbose"
)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "only Attribute and Name variants are relevant for import classification; all others \
              return None"
)]
fn classify_call(callee: &ruff_python_ast::Expr) -> Option<&'static str> {
    match callee {
        ruff_python_ast::Expr::Attribute(attr) => {
            let leaf = attr.attr.id.as_str();
            #[allow(
                clippy::pattern_type_mismatch,
                reason = "matching on &Expr reference from Box<Expr>; adding & is more verbose"
            )]
            #[allow(
                clippy::wildcard_enum_match_arm,
                reason = "only Name is meaningful as importlib base; all others return None"
            )]
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
    explicit: Option<&ruff_python_ast::Identifier>,
    level: u32,
) -> String {
    if level == 0 {
        return explicit.map_or(String::new(), |id| id.id.to_string());
    }
    let parts: Vec<&str> = consumer_module.split('.').collect();
    let drop = level as usize;
    let prefix_end = parts.len().saturating_sub(drop);
    let mut prefix: String = parts.get(..prefix_end).unwrap_or_default().join(".");
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
#[inline]
pub fn scan_project(root: &std::path::Path) -> std::io::Result<PluginIndex> {
    let mut out = PluginIndex::default();
    walk_dir(root, root, &mut out)?;
    out.resolve_star_imports();
    Ok(out)
}

/// Recursive worker for [`scan_project`].
///
/// Skips `__pycache__` directories (no `.py` files) and symlinked
/// directories (avoids cycles in monorepo layouts).
fn walk_dir(
    root: &std::path::Path,
    cur: &std::path::Path,
    out: &mut PluginIndex,
) -> std::io::Result<()> {
    for dir_entry in std::fs::read_dir(cur)? {
        let entry = dir_entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("__pycache__") {
                continue;
            }
            walk_dir(root, &path, out)?;
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("py") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let module_name = path_to_module(root, &path);
        let scanned = scan_source(&source, &module_name, &path);
        out.merge(scanned);
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
    fn aliased_reexport_records_target_and_local_names_separately() {
        let src = "from pkg.models import User as MyUser\n";
        let index = scan_source(src, "pkg.api", &PathBuf::from("pkg/api.py"));
        assert_eq!(
            index.import_bindings.len(),
            1,
            "expected one binding for aliased import, got {:?}",
            index.import_bindings
        );
        let b = &index.import_bindings[0];
        assert_eq!(b.consumer_module, "pkg.api");
        assert_eq!(
            b.consumer_key, "MyUser",
            "consumer_key must be the alias (MyUser)"
        );
        assert_eq!(b.target_module, "pkg.models");
        assert_eq!(
            b.target_name, "User",
            "target_name must be the original name (User)"
        );
    }

    #[test]
    fn aliased_reexport_without_as_keeps_names_equal() {
        let src = "from pkg.models import User\n";
        let index = scan_source(src, "pkg.api", &PathBuf::from("pkg/api.py"));
        let b = &index.import_bindings[0];
        assert_eq!(b.consumer_key, "User");
        assert_eq!(b.target_name, "User");
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

    #[test]
    fn scan_project_skips_pycache_directories() {
        use tempfile::tempdir;
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("pkg/__pycache__")).unwrap();
        std::fs::write(root.join("pkg/__init__.py"), "from foo import bar\n").unwrap();
        std::fs::write(
            root.join("pkg/__pycache__/cached.py"),
            "from polluted import zzz\n",
        )
        .unwrap();

        let index = scan_project(root).expect("scan ok");

        // Only the real source is scanned; the __pycache__ entry is skipped.
        assert_eq!(index.import_bindings.len(), 1);
        assert_eq!(index.import_bindings[0].target_module, "foo");
    }

    #[test]
    fn scan_source_detects_static_all_literal() {
        let src = r#"
__all__ = ["User", "Group"]
class User: pass
class Group: pass
class _Private: pass
"#;
        let index = scan_source(src, "pkg.models", std::path::Path::new("pkg/models.py"));
        assert_eq!(
            index.module_exports.get("pkg.models"),
            Some(&vec!["User".to_owned(), "Group".to_owned()]),
            "static __all__ list literal should populate module_exports",
        );
    }

    #[test]
    fn scan_source_detects_static_all_tuple_literal() {
        let src = r#"__all__ = ("a", "b")
a = 1
b = 2
"#;
        let index = scan_source(src, "pkg.x", std::path::Path::new("pkg/x.py"));
        assert_eq!(
            index.module_exports.get("pkg.x"),
            Some(&vec!["a".to_owned(), "b".to_owned()]),
            "static __all__ tuple literal should populate module_exports",
        );
    }

    #[test]
    fn scan_source_detects_annotated_all_literal() {
        let src = "__all__: list[str] = [\"A\", \"B\"]\nA = 1\nB = 2\n";
        let index = scan_source(src, "pkg.ann", std::path::Path::new("pkg/ann.py"));
        assert_eq!(
            index.module_exports.get("pkg.ann"),
            Some(&vec!["A".to_owned(), "B".to_owned()]),
            "annotated __all__ list literal should populate module_exports",
        );
    }

    #[test]
    fn scan_source_skips_dynamic_all() {
        let src = "__all__ = [n for n in dir() if not n.startswith('_')]\n";
        let index = scan_source(src, "pkg.dyn", std::path::Path::new("pkg/dyn.py"));
        assert!(
            index.module_exports.get("pkg.dyn").is_none(),
            "dynamic __all__ (list comp) should NOT produce static exports"
        );
    }

    #[test]
    fn scan_source_skips_non_string_all() {
        // Mixed-type or non-string literals are ignored.
        let src = "__all__ = [1, 2]\n";
        let index = scan_source(src, "pkg.bad", std::path::Path::new("pkg/bad.py"));
        assert!(
            index.module_exports.get("pkg.bad").is_none(),
            "non-string literal __all__ should be ignored"
        );
    }

    #[test]
    fn scan_source_no_all_leaves_module_exports_empty() {
        let src = "class A: pass\nclass B: pass\n";
        let index = scan_source(src, "pkg.noall", std::path::Path::new("pkg/noall.py"));
        assert!(
            index.module_exports.is_empty(),
            "modules without __all__ should not appear in module_exports"
        );
    }

    #[test]
    fn star_import_resolves_via_static_all() {
        let api = "from .models import *\n";
        let models = "__all__ = [\"User\", \"Group\"]\nclass User: pass\nclass Group: pass\nclass \
                      _P: pass\n";
        let mut index = scan_source(api, "pkg.api", std::path::Path::new("pkg/api.py"));
        let mods = scan_source(models, "pkg.models", std::path::Path::new("pkg/models.py"));
        index.merge(mods);
        index.resolve_star_imports();

        let names: Vec<String> = index
            .import_bindings
            .iter()
            .filter(|b| b.consumer_module == "pkg.api" && b.target_module == "pkg.models")
            .map(|b| b.target_name.clone())
            .collect();
        assert_eq!(names, vec!["User".to_owned(), "Group".to_owned()]);
        assert!(
            index.pending_star_imports.is_empty(),
            "static resolution should leave no pending entries"
        );
    }

    #[test]
    fn star_import_without_all_falls_through_to_pending() {
        let api = "from .models import *\n";
        let models = "class Open: pass\nclass _Hidden: pass\n"; // no __all__
        let mut index = scan_source(api, "pkg.api", std::path::Path::new("pkg/api.py"));
        let mods = scan_source(models, "pkg.models", std::path::Path::new("pkg/models.py"));
        index.merge(mods);
        index.resolve_star_imports();

        assert!(
            !index.pending_star_imports.is_empty(),
            "missing __all__ should leave a pending entry for runtime resolution"
        );
        assert_eq!(index.pending_star_imports[0].consumer_module, "pkg.api");
        assert_eq!(index.pending_star_imports[0].target_module, "pkg.models");
        // The placeholder should not still be in import_bindings.
        let star_count = index
            .import_bindings
            .iter()
            .filter(|b| b.target_name == "*")
            .count();
        assert_eq!(
            star_count, 0,
            "star placeholders should be removed after resolve"
        );
    }

    #[test]
    fn star_import_resolution_is_idempotent() {
        let api = "from .models import *\n";
        let models = "__all__ = [\"A\"]\nclass A: pass\n";
        let mut index = scan_source(api, "pkg.api", std::path::Path::new("pkg/api.py"));
        let mods = scan_source(models, "pkg.models", std::path::Path::new("pkg/models.py"));
        index.merge(mods);
        index.resolve_star_imports();
        let count_after_first = index.import_bindings.len();
        index.resolve_star_imports();
        assert_eq!(
            index.import_bindings.len(),
            count_after_first,
            "second resolve should be a no-op"
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_project_does_not_follow_symlinked_directories() {
        use tempfile::tempdir;
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::write(root.join("real/a.py"), "from foo import bar\n").unwrap();

        // Create a symlink to a directory.  If the walker followed it,
        // it would scan a.py twice.
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();

        let index = scan_project(root).expect("scan ok");

        // Exactly one binding — the symlinked directory was not followed.
        assert_eq!(index.import_bindings.len(), 1);
    }
}
