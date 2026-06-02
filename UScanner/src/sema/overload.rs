use super::symbol::SymbolId;
use super::types::TypeId;
use super::SemaContext;

pub enum CallResult {
    Ok(SymbolId),
    Ambiguous(Vec<SymbolId>),
    NoMatch { reasons: Vec<String> },
}

pub fn resolve_call(_ctx: &SemaContext, callee_set: &[SymbolId], _arg_types: &[TypeId]) -> CallResult {
    match callee_set {
        [only] => CallResult::Ok(*only),
        [] => CallResult::NoMatch {
            reasons: vec!["no callable candidates".to_string()],
        },
        many => CallResult::Ambiguous(many.to_vec()),
    }
}
