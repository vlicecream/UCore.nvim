use std::collections::HashSet;

use super::scope::{ScopeId, ScopeKind};
use super::symbol::SymbolId;
use super::types::{TemplateArg, TypeId, TypeKind};
use super::{symbol::SymbolKind, SemaContext};

pub fn lookup_name(ctx: &SemaContext, scope: ScopeId, name: &str) -> Vec<SymbolId> {
    let mut current = Some(scope);
    while let Some(scope_id) = current {
        let Some(scope_ref) = ctx.scopes.get(scope_id) else {
            break;
        };

        if let Some(ids) = scope_ref.symbols.get(name) {
            return ids.clone();
        }

        let via_using = lookup_via_using_decls(ctx, scope_id, name);
        if !via_using.is_empty() {
            return via_using;
        }

        current = scope_ref.parent;
    }

    Vec::new()
}

pub fn lookup_call_name(
    ctx: &SemaContext,
    scope: ScopeId,
    name: &str,
    arg_types: &[TypeId],
) -> Vec<SymbolId> {
    let mut resolved = lookup_name(ctx, scope, name);
    let mut seen = resolved.iter().copied().collect::<HashSet<_>>();

    for adl_scope in collect_adl_associated_scopes(ctx, arg_types) {
        let Some(scope_ref) = ctx.scopes.get(adl_scope) else {
            continue;
        };

        if let Some(ids) = scope_ref.symbols.get(name) {
            append_unique(&mut resolved, &mut seen, ids.iter().copied());
        }

        append_unique(
            &mut resolved,
            &mut seen,
            lookup_via_using_decls(ctx, adl_scope, name).into_iter(),
        );
    }

    resolved
}

pub fn lookup_qualified_name(
    ctx: &SemaContext,
    scope: ScopeId,
    segments: &[&str],
) -> Vec<SymbolId> {
    let Some((first, rest)) = segments.split_first() else {
        return Vec::new();
    };

    let mut current = lookup_name(ctx, scope, first);
    if current.is_empty() {
        current = lookup_name(ctx, ctx.scopes.global, first);
    }

    for segment in rest {
        let mut next = Vec::new();
        for symbol_id in current {
            next.extend(lookup_child_symbol(ctx, symbol_id, segment));
        }
        if next.is_empty() {
            return Vec::new();
        }
        current = next;
    }

    current
}

fn lookup_child_symbol(ctx: &SemaContext, symbol_id: SymbolId, name: &str) -> Vec<SymbolId> {
    let Some(symbol) = ctx.symbols.get(symbol_id) else {
        return Vec::new();
    };

    let child_scope = match &symbol.kind {
        SymbolKind::Namespace { children } => Some(*children),
        SymbolKind::Class { class_id, .. } => ctx.symbols.class_scope(*class_id),
        _ => None,
    };

    let Some(child_scope) = child_scope else {
        return Vec::new();
    };
    let Some(scope) = ctx.scopes.get(child_scope) else {
        return Vec::new();
    };

    scope.symbols.get(name).cloned().unwrap_or_default()
}

fn lookup_via_using_decls(ctx: &SemaContext, scope_id: ScopeId, name: &str) -> Vec<SymbolId> {
    let Some(scope) = ctx.scopes.get(scope_id) else {
        return Vec::new();
    };

    for using_decl in &scope.using_decls {
        if let Some(path) = using_decl.strip_prefix("namespace ") {
            let mut segments = path
                .split("::")
                .map(str::trim)
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>();
            segments.push(name);
            let resolved = lookup_qualified_name(ctx, ctx.scopes.global, &segments);
            if !resolved.is_empty() {
                return resolved;
            }
            continue;
        }

        let segments = using_decl
            .split("::")
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        if segments.last().copied() != Some(name) {
            continue;
        }
        let resolved = if segments.len() == 1 {
            ctx.scopes
                .get(ctx.scopes.global)
                .and_then(|global| global.symbols.get(name).cloned())
                .unwrap_or_default()
        } else {
            lookup_qualified_name(ctx, ctx.scopes.global, &segments)
        };
        if !resolved.is_empty() {
            return resolved;
        }
    }

    Vec::new()
}

fn collect_adl_associated_scopes(ctx: &SemaContext, arg_types: &[TypeId]) -> Vec<ScopeId> {
    let mut scopes = Vec::new();
    let mut seen_scopes = HashSet::new();
    let mut seen_classes = HashSet::new();

    for type_id in arg_types {
        collect_adl_scopes_for_type(
            ctx,
            *type_id,
            &mut scopes,
            &mut seen_scopes,
            &mut seen_classes,
        );
    }

    scopes
}

fn collect_adl_scopes_for_type(
    ctx: &SemaContext,
    type_id: TypeId,
    scopes: &mut Vec<ScopeId>,
    seen_scopes: &mut HashSet<ScopeId>,
    seen_classes: &mut HashSet<super::symbol::ClassId>,
) {
    let Some(kind) = ctx.types.get(type_id) else {
        return;
    };

    match kind {
        TypeKind::Pointer { pointee, .. } => {
            collect_adl_scopes_for_type(ctx, *pointee, scopes, seen_scopes, seen_classes);
        }
        TypeKind::Reference { referent, .. } => {
            collect_adl_scopes_for_type(ctx, *referent, scopes, seen_scopes, seen_classes);
        }
        TypeKind::Array { elem, .. } => {
            collect_adl_scopes_for_type(ctx, *elem, scopes, seen_scopes, seen_classes);
        }
        TypeKind::Typedef { aliased, .. } => {
            collect_adl_scopes_for_type(ctx, *aliased, scopes, seen_scopes, seen_classes);
        }
        TypeKind::Template { base, args } => {
            if let Some(base_type) = ctx.resolve_existing_type_text(base) {
                collect_adl_scopes_for_type(ctx, base_type, scopes, seen_scopes, seen_classes);
            }
            for arg in args {
                if let TemplateArg::Type(type_id) = arg {
                    collect_adl_scopes_for_type(ctx, *type_id, scopes, seen_scopes, seen_classes);
                }
            }
        }
        TypeKind::Class(class_id) => {
            if !seen_classes.insert(*class_id) {
                return;
            }
            if let Some(class_scope) = ctx.symbols.class_scope(*class_id) {
                collect_enclosing_namespace_scopes(ctx, class_scope, scopes, seen_scopes);
            }
            for parent in ctx.symbols.class_parents(*class_id) {
                collect_adl_scopes_for_type(ctx, *parent, scopes, seen_scopes, seen_classes);
            }
        }
        _ => {}
    }
}

fn collect_enclosing_namespace_scopes(
    ctx: &SemaContext,
    scope_id: ScopeId,
    scopes: &mut Vec<ScopeId>,
    seen_scopes: &mut HashSet<ScopeId>,
) {
    let mut current = ctx.scopes.get(scope_id).and_then(|scope| scope.parent);
    while let Some(current_scope) = current {
        let Some(scope) = ctx.scopes.get(current_scope) else {
            break;
        };
        if matches!(scope.kind, ScopeKind::Namespace) && seen_scopes.insert(current_scope) {
            scopes.push(current_scope);
        }
        current = scope.parent;
    }
}

fn append_unique(
    target: &mut Vec<SymbolId>,
    seen: &mut HashSet<SymbolId>,
    ids: impl Iterator<Item = SymbolId>,
) {
    for symbol_id in ids {
        if seen.insert(symbol_id) {
            target.push(symbol_id);
        }
    }
}
