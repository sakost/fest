//! Structured diff IR for mutations dispatched to the plugin backend.

use serde::{Deserialize, Serialize};

/// One unit of structural change derived from a [`crate::mutation::Mutant`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MutationDiff {
    /// Function body changed. `qualname` may be dotted for nested functions.
    /// `new_source` is the raw `def` block with decorators stripped.
    FunctionBody {
        /// Dotted qualified name of the function (e.g. `outer.inner`).
        qualname: String,
        /// Full `def` block source with decorators stripped.
        new_source: String,
    },

    /// Module-level `NAME = expr` binding changed value.
    ConstantBind {
        /// Name of the module-level binding.
        name: String,
        /// New expression text for the right-hand side.
        new_expr: String,
    },

    /// Class method body changed. `method_name` uses dotted suffix
    /// (`x.fget` / `.fset` / `.fdel`) for property accessors.
    ClassMethod {
        /// Dotted qualified name of the class (e.g. `Outer.Inner`).
        class_qualname: String,
        /// Name of the method, including property accessor suffix where applicable.
        method_name: String,
        /// Full `def` block source for the mutated method.
        new_source: String,
    },

    /// Class-level non-method attribute changed.
    ClassAttr {
        /// Dotted qualified name of the class containing the attribute.
        class_qualname: String,
        /// Name of the class attribute.
        name: String,
        /// New expression text for the attribute value.
        new_expr: String,
    },

    /// Module-level binding requiring statement-mode compilation
    /// (decorator removal, class re-definition).
    ModuleAttr {
        /// Name of the module-level binding.
        name: String,
        /// Full source of the new statement (e.g. decorated def or class body).
        new_source: String,
    },
}
