use ruff_macros::{ViolationMetadata, derive_message_formats};
use ruff_python_ast::{self as ast, PythonVersion, Stmt};
use ruff_python_semantic::{Imported, ScopeKind};
use ruff_text_size::Ranged;

use crate::codes::Category;
use crate::fix;
use crate::preview::is_import_outside_top_level_fix_enabled;
use crate::rules::flake8_tidy_imports::rules::BannedModuleImportPolicies;
use crate::{Fix, FixAvailability, Violation};
use crate::{
    checkers::ast::Checker, codes::Rule, rules::flake8_tidy_imports::matchers::NameMatchPolicy,
};

/// ## What it does
/// Checks for `import` statements outside of a module's top-level scope, such
/// as within a function or class definition.
///
/// ## Why is this bad?
/// [PEP 8] recommends placing imports not only at the top-level of a module,
/// but at the very top of the file, "just after any module comments and
/// docstrings, and before module globals and constants."
///
/// `import` statements have effects that are global in scope; defining them at
/// the top level has a number of benefits. For example, it makes it easier to
/// identify the dependencies of a module, and ensures that any invalid imports
/// are caught regardless of whether a specific function is called or class is
/// instantiated.
///
/// An import statement would typically be placed within a function only to
/// avoid a circular dependency, to defer a costly module load, or to avoid
/// loading a dependency altogether in a certain runtime environment.
///
/// ## Example
/// ```python
/// def print_python_version():
///     import platform
///
///     print(platform.python_version())
/// ```
///
/// Use instead:
/// ```python
/// import platform
///
///
/// def print_python_version():
///     print(platform.python_version())
/// ```
///
/// On Python 3.15 and later, a [lazy import] defers loading the module until
/// it is first used:
/// ```python
/// lazy import platform
///
///
/// def print_python_version():
///     print(platform.python_version())
/// ```
///
/// ## Fix safety
/// This rule's fix is marked as unsafe, as moving an import changes when the
/// module is loaded. This can run import side effects earlier, introduce a
/// circular import, or fail at module load time if the dependency is missing.
///
/// The fix is only offered for an absolute import placed directly in the body
/// of a function, and only when the imported names are not bound anywhere else
/// in that function, in an enclosing scope, or at the top level (other than by
/// the same import). When targeting Python 3.15 or later, the import is moved
/// as a lazy import.
///
/// ## See also
/// This rule will ignore import statements configured in
/// [`lint.flake8-tidy-imports.banned-module-level-imports`][banned-module-level-imports]
/// if the rule [`banned-module-level-imports`][TID253] is enabled.
///
/// ## Options
///
/// - `lint.flake8-tidy-imports.ban-lazy`
///
/// [banned-module-level-imports]: https://docs.astral.sh/ruff/settings/#lint_flake8-tidy-imports_banned-module-level-imports
/// [TID253]: https://docs.astral.sh/ruff/rules/banned-module-level-imports/
/// [PEP 8]: https://peps.python.org/pep-0008/#imports
/// [lazy import]: https://peps.python.org/pep-0810/
#[derive(ViolationMetadata)]
#[violation_metadata(stable_since = "0.12.0", category = Category::Pedantic)]
pub(crate) struct ImportOutsideTopLevel {
    fix_kind: Option<FixKind>,
}

impl Violation for ImportOutsideTopLevel {
    const FIX_AVAILABILITY: FixAvailability = FixAvailability::Sometimes;

    #[derive_message_formats]
    fn message(&self) -> String {
        "`import` should be at the top-level of a file".to_string()
    }

    fn fix_title(&self) -> Option<String> {
        let title = match self.fix_kind? {
            FixKind::Move => "Move import to the top level",
            FixKind::MoveLazy => "Move import to the top level as a lazy import",
            FixKind::Remove => "Remove import already present at the top level",
        };
        Some(title.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FixKind {
    /// Move the import to the top level.
    Move,
    /// Move the import to the top level as a `lazy` import.
    MoveLazy,
    /// Remove the import, as the same import is already present at the top level.
    Remove,
}

/// PLC0415
pub(crate) fn import_outside_top_level(checker: &Checker, stmt: &Stmt) {
    if checker.semantic().current_scope().kind.is_module() {
        // "Top-level" imports are allowed
        return;
    }

    // Check if any of the non-top-level imports are banned by TID253
    // before emitting the diagnostic to avoid conflicts.
    let mut any_alias_banned = false;
    if checker.is_rule_enabled(Rule::BannedModuleLevelImports) {
        let mut all_aliases_banned = true;
        let mut has_alias = false;
        for (policy, node) in &BannedModuleImportPolicies::new(stmt, checker) {
            if node.is_alias() {
                has_alias = true;
                let is_banned = is_banned_module_level_import(&policy, checker);
                all_aliases_banned &= is_banned;
                any_alias_banned |= is_banned;
            }
            // If the entire import is banned
            else if is_banned_module_level_import(&policy, checker) {
                return;
            }
        }

        if has_alias && all_aliases_banned {
            return;
        }
    }

    // Don't move an import to the top level if TID253 bans any part of it there.
    let fix = if is_import_outside_top_level_fix_enabled(checker.settings()) && !any_alias_banned {
        fix_import(checker, stmt)
    } else {
        None
    };

    // Emit the diagnostic
    let mut diagnostic = checker.report_diagnostic(
        ImportOutsideTopLevel {
            fix_kind: fix.as_ref().map(|(_, kind)| *kind),
        },
        stmt.range(),
    );
    if let Some((fix, _)) = fix {
        diagnostic.set_fix(fix);
    }
}

fn is_banned_module_level_import(policy: &NameMatchPolicy, checker: &Checker) -> bool {
    policy
        .find(
            checker
                .settings()
                .flake8_tidy_imports
                .banned_module_level_imports(),
        )
        .is_some()
}

/// Generate a [`Fix`] that moves a function-level import to the top level of the module.
///
/// Returns `None` if moving the import could change which binding a name resolves to, or if the
/// import is in a position where moving it unconditionally could change behavior, as in:
/// ```python
/// def f():
///     try:
///         import numpy  # moving this out of the `try` would change behavior
///     except ImportError:
///         numpy = None
/// ```
fn fix_import(checker: &Checker, stmt: &Stmt) -> Option<(Fix, FixKind)> {
    let semantic = checker.semantic();

    // Only handle imports placed directly in the body of a function, where every enclosing
    // statement is a function or class definition. This excludes imports nested in `if`,
    // `try`, `with`, or loop statements, including `if TYPE_CHECKING:` blocks.
    let mut ancestors = semantic.current_statements().skip(1);
    if !ancestors.next().is_some_and(Stmt::is_function_def_stmt)
        || !ancestors
            .all(|ancestor| ancestor.is_function_def_stmt() || ancestor.is_class_def_stmt())
        || !semantic.current_scope().kind.is_function()
    {
        return None;
    }

    let bound_names: Vec<&str> = match stmt {
        Stmt::Import(ast::StmtImport { names, .. }) => names
            .iter()
            .map(|alias| match &alias.asname {
                Some(asname) => asname.as_str(),
                // `import a.b.c` binds `a`.
                None => alias.name.split('.').next().unwrap_or(&alias.name),
            })
            .collect(),
        Stmt::ImportFrom(ast::StmtImportFrom {
            module,
            names,
            level,
            ..
        }) => {
            // Skip relative imports, `__future__` imports, and star imports.
            if *level > 0
                || module.as_deref() == Some("__future__")
                || names.iter().any(|alias| &alias.name == "*")
            {
                return None;
            }
            names
                .iter()
                .map(|alias| alias.asname.as_ref().unwrap_or(&alias.name).as_str())
                .collect()
        }
        _ => return None,
    };

    let statement_id = semantic.current_statement_id()?;
    let scope = semantic.current_scope();
    let global_scope = semantic.global_scope();
    if global_scope.uses_star_imports() {
        return None;
    }

    let mut already_imported = 0;
    for name in &bound_names {
        // The import must be the only binding of the name in the function, so that the name
        // still refers to the import everywhere in the function once the import is moved. For
        // example, a later `del` or reassignment would make the name local to the function.
        let mut local_bindings = scope.get_all(name);
        let binding = semantic.binding(local_bindings.next()?);
        if local_bindings.next().is_some()
            || binding.source != Some(statement_id)
            || binding.is_global()
            || binding.is_nonlocal()
        {
            return None;
        }
        let local_import = binding.as_any_import()?;

        // The name must not be bound in an enclosing function or class, since the name would
        // then resolve to that binding instead of the moved import.
        if semantic
            .scopes
            .ancestors(semantic.scope_id)
            .skip(1)
            .filter(|ancestor| !matches!(ancestor.kind, ScopeKind::Module))
            .any(|ancestor| ancestor.has(name))
        {
            return None;
        }

        // If the name is already bound at the top level, only proceed if it is bound by a single,
        // unconditional runtime import of the same symbol, in which case the local import is
        // redundant. Any other top-level binding (including a builtin) would be shadowed or
        // replaced by the moved import.
        let mut global_bindings = global_scope.get_all(name);
        if let Some(global_id) = global_bindings.next() {
            let global_binding = semantic.binding(global_id);
            if global_bindings.next().is_some()
                || !global_binding.context.is_runtime()
                || global_binding
                    .source
                    .is_none_or(|source| semantic.parent_statement_id(source).is_some())
                || global_binding.as_any_import().is_none_or(|global_import| {
                    global_import.qualified_name() != local_import.qualified_name()
                })
            {
                return None;
            }
            already_imported += 1;
        }
    }

    let delete_edit = fix::edits::delete_stmt(
        stmt,
        semantic.current_statement_parent(),
        checker.locator(),
        checker.indexer(),
    );

    // Every name is already imported at the top level: the local import can simply be removed.
    if already_imported == bound_names.len() {
        return Some((Fix::unsafe_edit(delete_edit), FixKind::Remove));
    }
    // Some, but not all, names are already imported at the top level. Splitting the import is
    // not supported.
    if already_imported > 0 {
        return None;
    }

    let is_lazy = can_be_lazy(checker, stmt);
    let mut top_level_import = stmt.clone();
    match &mut top_level_import {
        Stmt::Import(import) => import.is_lazy = is_lazy,
        Stmt::ImportFrom(import_from) => import_from.is_lazy = is_lazy,
        _ => return None,
    }
    let add_edit = checker
        .importer()
        .add_import_statement(&checker.generator().stmt(&top_level_import), stmt.start());

    let kind = if is_lazy {
        FixKind::MoveLazy
    } else {
        FixKind::Move
    };
    Some((Fix::unsafe_edits(add_edit, [delete_edit]), kind))
}

/// Returns `true` if the import should be moved to the top level as a `lazy` import, i.e., if
/// the target version supports lazy imports and `lint.flake8-tidy-imports.ban-lazy` allows it.
fn can_be_lazy(checker: &Checker, stmt: &Stmt) -> bool {
    if checker.target_version() < PythonVersion::PY315 {
        return false;
    }

    let ban_lazy = &checker.settings().flake8_tidy_imports.ban_lazy;
    for (policy, node) in &BannedModuleImportPolicies::new(stmt, checker) {
        if ban_lazy.includes_all() && stmt.is_import_from_stmt() && node.is_alias() {
            continue;
        }
        if ban_lazy.find(&policy).is_some() {
            return false;
        }
    }
    true
}
