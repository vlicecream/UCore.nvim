use super::scope::ScopeId;
use super::symbol::SymbolId;
use super::SemaContext;

pub fn lookup_name(ctx: &SemaContext, scope: ScopeId, name: &str) -> Vec<SymbolId> {
    let mut current = Some(scope);
    while let Some(scope_id) = current {
        let Some(scope_ref) = ctx.scopes.get(scope_id) else {
            break;
        };

        if let Some(ids) = scope_ref.symbols.get(name) {
            return ids.clone();
        }

        current = scope_ref.parent;
    }

    Vec::new()
}
