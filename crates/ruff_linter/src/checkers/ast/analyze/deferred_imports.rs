use crate::checkers::ast::Checker;
use crate::rules::pylint;

/// Run lint rules over all deferred import statements in the [`SemanticModel`].
pub(crate) fn deferred_imports(checker: &mut Checker) {
    let imports = std::mem::take(&mut checker.analyze.imports_outside_top_level);
    for snapshot in imports {
        checker.semantic.restore(snapshot);

        let checker = &*checker;
        pylint::rules::import_outside_top_level(checker, checker.semantic.current_statement());
    }
}
