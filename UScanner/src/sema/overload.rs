use super::symbol::SymbolId;
use super::types::{compat_rank, Compat, TypeId, TypeKind};
use super::{MemberCallableSignature, SemaContext};

pub enum CallResult {
    Ok(SymbolId),
    Ambiguous(Vec<SymbolId>),
    NoMatch { reasons: Vec<String> },
}

pub fn resolve_call(_ctx: &SemaContext, callee_set: &[SymbolId], _arg_types: &[TypeId]) -> CallResult {
    resolve_call_with_args(_ctx, callee_set, _arg_types)
}

pub fn resolve_call_with_args(ctx: &SemaContext, callee_set: &[SymbolId], arg_types: &[TypeId]) -> CallResult {
    let callable_set = callee_set
        .iter()
        .filter_map(|symbol_id| {
            let type_id = ctx.symbol_type(*symbol_id)?;
            let signature = function_signature(ctx, type_id)?;
            Some((*symbol_id, signature))
        })
        .collect::<Vec<_>>();
    resolve_call_with_signatures(ctx, &callable_set, arg_types)
}

pub fn resolve_call_with_signatures(
    ctx: &SemaContext,
    callee_set: &[(SymbolId, MemberCallableSignature)],
    arg_types: &[TypeId],
) -> CallResult {
    let mut best_score: Option<Vec<u8>> = None;
    let mut best = Vec::<SymbolId>::new();
    let mut reasons = Vec::<String>::new();

    for (symbol_id, signature) in callee_set {
        let params = &signature.params;
        let min_arity = signature.min_arity;
        let is_variadic = signature.is_variadic;

        if arg_types.len() < min_arity {
            reasons.push(format!("not enough arguments for candidate {:?}", symbol_id.0));
            continue;
        }
        if !is_variadic && arg_types.len() > params.len() {
            reasons.push(format!("arity mismatch for candidate {:?}", symbol_id.0));
            continue;
        }
        if is_variadic && arg_types.len() < min_arity {
            reasons.push(format!("not enough arguments for variadic candidate {:?}", symbol_id.0));
            continue;
        }

        let mut score = Vec::new();
        let mut compatible = true;
        for (arg, param) in arg_types.iter().zip(params.iter()) {
            let compat = ctx.check_compat(*arg, *param);
            if compat == Compat::Incompatible {
                compatible = false;
                break;
            }
            score.push(compat_rank(compat));
        }

        if !compatible {
            reasons.push(format!("type mismatch for candidate {:?}", symbol_id.0));
            continue;
        }

        match &best_score {
            None => {
                best_score = Some(score);
                best = vec![*symbol_id];
            }
            Some(current) if score > *current => {
                best_score = Some(score);
                best = vec![*symbol_id];
            }
            Some(current) if score == *current => best.push(*symbol_id),
            _ => {}
        }
    }

    match best.as_slice() {
        [] => CallResult::NoMatch { reasons },
        [only] => CallResult::Ok(*only),
        many => CallResult::Ambiguous(many.to_vec()),
    }
}

fn function_signature(ctx: &SemaContext, type_id: TypeId) -> Option<MemberCallableSignature> {
    let TypeKind::Function {
        return_t,
        params,
        min_arity,
        is_variadic,
        is_const_member,
        ..
    } = ctx.types.get(type_id)?
    else {
        return None;
    };
    Some(MemberCallableSignature {
        return_t: *return_t,
        params: params.clone(),
        min_arity: *min_arity,
        is_variadic: *is_variadic,
        is_const_member: *is_const_member,
    })
}
