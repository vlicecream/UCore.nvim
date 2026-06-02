use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

use super::types::{BuiltinType, TypeArena, TypeId, TypeKind};

static BUILTIN_TYPE_ALIASES: OnceLock<HashMap<String, BuiltinType>> = OnceLock::new();

#[derive(Deserialize, Default)]
struct BuiltinTypesFile {
    #[serde(default)]
    aliases: BuiltinTypeAliases,
}

#[derive(Deserialize, Default)]
struct BuiltinTypeAliases {
    #[serde(default)]
    void: Vec<String>,
    #[serde(default)]
    bool: Vec<String>,
    #[serde(default)]
    char: Vec<String>,
    #[serde(default)]
    int32: Vec<String>,
    #[serde(default)]
    uint32: Vec<String>,
    #[serde(default)]
    float: Vec<String>,
    #[serde(default)]
    double: Vec<String>,
}

pub fn builtin_type_by_name(arena: &mut TypeArena, name: &str) -> Option<TypeId> {
    let kind = builtin_type_aliases().get(name.trim()).copied()?;
    Some(match kind {
        BuiltinType::Void => arena.void_t,
        BuiltinType::Bool => arena.bool_t,
        BuiltinType::Char => arena.intern(TypeKind::Builtin(BuiltinType::Char)),
        BuiltinType::Int32 => arena.int32_t,
        BuiltinType::UInt32 => arena.uint32_t,
        BuiltinType::Float => arena.float_t,
        BuiltinType::Double => arena.double_t,
    })
}

fn builtin_type_aliases() -> &'static HashMap<String, BuiltinType> {
    BUILTIN_TYPE_ALIASES.get_or_init(|| {
        let parsed: BuiltinTypesFile =
            toml::from_str(include_str!("../../data/builtin_types.toml")).unwrap_or_default();
        let mut aliases = HashMap::new();
        extend_aliases(&mut aliases, parsed.aliases.void, BuiltinType::Void);
        extend_aliases(&mut aliases, parsed.aliases.bool, BuiltinType::Bool);
        extend_aliases(&mut aliases, parsed.aliases.char, BuiltinType::Char);
        extend_aliases(&mut aliases, parsed.aliases.int32, BuiltinType::Int32);
        extend_aliases(&mut aliases, parsed.aliases.uint32, BuiltinType::UInt32);
        extend_aliases(&mut aliases, parsed.aliases.float, BuiltinType::Float);
        extend_aliases(&mut aliases, parsed.aliases.double, BuiltinType::Double);
        aliases
    })
}

fn extend_aliases(
    out: &mut HashMap<String, BuiltinType>,
    aliases: Vec<String>,
    builtin: BuiltinType,
) {
    for alias in aliases {
        let trimmed = alias.trim();
        if !trimmed.is_empty() {
            out.insert(trimmed.to_string(), builtin);
        }
    }
}
