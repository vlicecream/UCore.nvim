use std::collections::HashMap;

use super::tokenizer::first_identifier;

const MAX_EXPANSION_DEPTH: usize = 16;

#[derive(Clone, Debug, Default)]
pub struct MacroTable {
    object_like: HashMap<String, String>,
    function_like: HashMap<String, FunctionMacro>,
}

#[derive(Clone, Debug)]
struct FunctionMacro {
    params: Vec<String>,
    body: String,
}

#[derive(Clone, Debug)]
struct Expansion {
    text: String,
    map: Vec<u32>,
}

#[derive(Clone, Debug)]
struct Piece {
    text: String,
    map: Vec<u32>,
}

impl MacroTable {
    pub fn define(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        self.function_like.remove(&name);
        self.object_like.insert(name, value.into());
    }

    pub fn define_function(
        &mut self,
        name: impl Into<String>,
        params: Vec<String>,
        body: impl Into<String>,
    ) {
        let name = name.into();
        self.object_like.remove(&name);
        self.function_like.insert(
            name,
            FunctionMacro {
                params,
                body: body.into(),
            },
        );
    }

    pub fn undefine(&mut self, name: &str) {
        self.object_like.remove(name);
        self.function_like.remove(name);
    }

    pub fn is_defined(&self, name: &str) -> bool {
        self.object_like.contains_key(name) || self.function_like.contains_key(name)
    }

    pub fn has_function_macro(&self, name: &str) -> bool {
        self.function_like.contains_key(name)
    }

    pub fn value_of(&self, name: &str) -> Option<&str> {
        self.object_like.get(name).map(String::as_str)
    }

    pub fn defines_hash(&self) -> String {
        let mut entries = self
            .object_like
            .iter()
            .map(|(name, value)| format!("o:{name}={value}"))
            .chain(self.function_like.iter().map(|(name, value)| {
                format!("f:{name}({})={}", value.params.join(","), value.body)
            }))
            .collect::<Vec<_>>();
        entries.sort();
        blake3::hash(entries.join("\n").as_bytes())
            .to_hex()
            .to_string()
    }

    pub fn define_from_assignment(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }

        let mut parts = trimmed.splitn(2, '=');
        let name = parts.next().unwrap_or_default().trim();
        if name.is_empty() {
            return;
        }
        let value = parts.next().map(str::trim).unwrap_or("1");
        self.define(name.to_string(), value.to_string());
    }

    pub fn define_from_directive(&mut self, directive_body: &str) {
        let trimmed = directive_body.trim();
        if trimmed.is_empty() {
            return;
        }

        let Some(name_token) = first_identifier(trimmed) else {
            return;
        };
        let chars = trimmed.chars().collect::<Vec<_>>();
        let mut index = trimmed[..name_token.end].chars().count();
        let name = name_token.text.to_string();

        if name.is_empty() {
            return;
        }

        if index < chars.len() && chars[index] == '(' {
            index += 1;
            let params_start = index;
            let mut depth = 1usize;
            while index < chars.len() && depth > 0 {
                match chars[index] {
                    '(' => depth += 1,
                    ')' => depth = depth.saturating_sub(1),
                    _ => {}
                }
                index += 1;
            }

            let params_text = chars[params_start..index.saturating_sub(1)]
                .iter()
                .collect::<String>();
            let params = params_text
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            let body = chars[index..].iter().collect::<String>().trim().to_string();
            self.define_function(name, params, body);
            return;
        }

        let value = chars[index..].iter().collect::<String>().trim().to_string();
        self.define(name, if value.is_empty() { "1".to_string() } else { value });
    }

    pub fn expand_line(&self, line: &str) -> String {
        self.expand_line_with_map(line).0
    }

    pub fn expand_line_with_map(&self, line: &str) -> (String, Vec<u32>) {
        let char_count = line.chars().count() as u32;
        let source_map = (0..=char_count).collect::<Vec<_>>();
        let expansion = self.expand_fragment(line, &source_map, 0);
        (expansion.text, expansion.map)
    }

    fn expand_fragment(&self, fragment: &str, source_map: &[u32], depth: usize) -> Expansion {
        if depth >= MAX_EXPANSION_DEPTH {
            return Expansion {
                text: fragment.to_string(),
                map: source_map.to_vec(),
            };
        }

        let chars = fragment.chars().collect::<Vec<_>>();
        let mut text = String::with_capacity(fragment.len());
        let mut map = Vec::<u32>::new();
        let mut index = 0usize;

        while index < chars.len() {
            let ch = chars[index];
            if ch.is_ascii_alphabetic() || ch == '_' {
                let start = index;
                index += 1;
                while index < chars.len() && (chars[index].is_ascii_alphanumeric() || chars[index] == '_')
                {
                    index += 1;
                }
                let ident = chars[start..index].iter().collect::<String>();

                if let Some(function_macro) = self.function_like.get(&ident) {
                    let mut after_ident = index;
                    while after_ident < chars.len() && chars[after_ident].is_whitespace() {
                        after_ident += 1;
                    }
                    if after_ident < chars.len() && chars[after_ident] == '(' {
                        if let Some((args, end_index)) = parse_macro_args(&chars, after_ident) {
                            let expansion = self.expand_function_like(
                                function_macro,
                                &args,
                                source_map,
                                start,
                                end_index,
                                depth,
                            );
                            text.push_str(&expansion.text);
                            extend_char_map(&mut map, &expansion.text, &expansion.map);
                            index = end_index;
                            continue;
                        }
                    }
                }

                if let Some(value) = self.object_like.get(&ident) {
                    text.push_str(value);
                    let start_col = source_map.get(start).copied().unwrap_or(start as u32);
                    for _ in value.chars() {
                        map.push(start_col);
                    }
                    continue;
                }

                let ident_text = chars[start..index].iter().collect::<String>();
                text.push_str(&ident_text);
                for char_index in start..index {
                    map.push(source_map.get(char_index).copied().unwrap_or(char_index as u32));
                }
                continue;
            }

            text.push(ch);
            map.push(source_map.get(index).copied().unwrap_or(index as u32));
            index += 1;
        }

        map.push(
            source_map
                .get(chars.len())
                .copied()
                .unwrap_or(chars.len() as u32),
        );
        Expansion { text, map }
    }

    fn expand_function_like(
        &self,
        macro_def: &FunctionMacro,
        args: &[MacroArg],
        source_map: &[u32],
        macro_start: usize,
        macro_end: usize,
        depth: usize,
    ) -> Expansion {
        let mut arg_expansions = HashMap::<String, Expansion>::new();
        for (param, arg) in macro_def.params.iter().zip(args.iter()) {
            let expansion = self.expand_fragment(&arg.text, &arg.map, depth + 1);
            arg_expansions.insert(param.clone(), expansion);
        }

        let body_chars = macro_def.body.chars().collect::<Vec<_>>();
        let mut substituted_text = String::new();
        let mut substituted_map = Vec::<u32>::new();
        let mut index = 0usize;
        let macro_start_col = source_map
            .get(macro_start)
            .copied()
            .unwrap_or(macro_start as u32);
        let macro_end_col = source_map
            .get(macro_end)
            .copied()
            .unwrap_or(macro_end as u32);

        while index < body_chars.len() {
            let ch = body_chars[index];
            if ch.is_whitespace() {
                substituted_text.push(ch);
                substituted_map.push(macro_start_col);
                index += 1;
                continue;
            }

            if ch == '#' {
                let stringified = self.try_stringify_parameter(
                    &body_chars,
                    &mut index,
                    args,
                    &macro_def.params,
                    macro_start_col,
                );
                if let Some(piece) = stringified {
                    substituted_text.push_str(&piece.text);
                    substituted_map.extend(piece.map);
                    continue;
                }
            }

            let mut piece = self.read_body_piece(
                &body_chars,
                &mut index,
                args,
                &macro_def.params,
                &arg_expansions,
                macro_start_col,
                false,
            );

            while let Some(next_index) = skip_token_paste(&body_chars, index) {
                index = next_index;
                let rhs = self.read_body_piece(
                    &body_chars,
                    &mut index,
                    args,
                    &macro_def.params,
                    &arg_expansions,
                    macro_start_col,
                    true,
                );
                piece.text.push_str(&rhs.text);
                piece.map.extend(rhs.map);
            }

            substituted_text.push_str(&piece.text);
            substituted_map.extend(piece.map);
        }
        substituted_map.push(macro_end_col);

        self.expand_fragment(&substituted_text, &substituted_map, depth + 1)
    }

    fn try_stringify_parameter(
        &self,
        body_chars: &[char],
        index: &mut usize,
        args: &[MacroArg],
        params: &[String],
        macro_start_col: u32,
    ) -> Option<Piece> {
        if body_chars.get(*index).copied() != Some('#')
            || body_chars.get(*index + 1).copied() == Some('#')
        {
            return None;
        }

        let mut probe = *index + 1;
        while probe < body_chars.len() && body_chars[probe].is_whitespace() {
            probe += 1;
        }

        let Some((ident, end_index)) = read_identifier(body_chars, probe) else {
            return None;
        };
        let Some(arg_index) = params.iter().position(|param| param == &ident) else {
            return None;
        };

        *index = end_index;
        let arg = args.get(arg_index)?;
        let text = stringify_macro_argument(&arg.text);
        let start_col = macro_start_col;
        Some(piece_with_single_column(text, start_col))
    }

    fn read_body_piece(
        &self,
        body_chars: &[char],
        index: &mut usize,
        args: &[MacroArg],
        params: &[String],
        arg_expansions: &HashMap<String, Expansion>,
        macro_start_col: u32,
        paste_mode: bool,
    ) -> Piece {
        if let Some((ident, end_index)) = read_identifier(body_chars, *index) {
            *index = end_index;
            if let Some(arg_index) = params.iter().position(|param| param == &ident) {
                let arg = &args[arg_index];
                if paste_mode {
                    return Piece {
                        text: arg.text.clone(),
                        map: trim_piece_map(&arg.text, &arg.map, macro_start_col),
                    };
                }
                if let Some(expansion) = arg_expansions.get(&ident) {
                    return Piece {
                        text: expansion.text.clone(),
                        map: expansion.map[..expansion.map.len().saturating_sub(1)].to_vec(),
                    };
                }
            }

            if paste_mode {
                return piece_with_single_column(ident, macro_start_col);
            }

            if let Some(value) = self.object_like.get(&ident) {
                return piece_with_single_column(value.clone(), macro_start_col);
            }

            return piece_with_single_column(ident, macro_start_col);
        }

        let ch = body_chars.get(*index).copied().unwrap_or_default();
        *index += 1;
        piece_with_single_column(ch.to_string(), macro_start_col)
    }
}

#[derive(Clone, Debug)]
struct MacroArg {
    text: String,
    map: Vec<u32>,
}

fn parse_macro_args(chars: &[char], open_paren_index: usize) -> Option<(Vec<MacroArg>, usize)> {
    if chars.get(open_paren_index).copied() != Some('(') {
        return None;
    }

    let mut args = Vec::<MacroArg>::new();
    let mut current_text = String::new();
    let mut current_map = Vec::<u32>::new();
    let mut depth = 0usize;
    let mut index = open_paren_index;
    let mut in_string = false;
    let mut in_char = false;
    let mut escape = false;

    while index < chars.len() {
        let ch = chars[index];
        if index == open_paren_index {
            depth = 1;
            index += 1;
            continue;
        }

        if in_string {
            current_text.push(ch);
            current_map.push(index as u32);
            if !escape && ch == '"' {
                in_string = false;
            }
            escape = ch == '\\' && !escape;
            index += 1;
            continue;
        }
        if in_char {
            current_text.push(ch);
            current_map.push(index as u32);
            if !escape && ch == '\'' {
                in_char = false;
            }
            escape = ch == '\\' && !escape;
            index += 1;
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                current_text.push(ch);
                current_map.push(index as u32);
                index += 1;
            }
            '\'' => {
                in_char = true;
                current_text.push(ch);
                current_map.push(index as u32);
                index += 1;
            }
            '(' => {
                depth += 1;
                current_text.push(ch);
                current_map.push(index as u32);
                index += 1;
            }
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let trimmed = current_text.trim().to_string();
                    let trimmed_map = trim_arg_map(&current_text, &current_map, trimmed.chars().count());
                    if !trimmed.is_empty() || !args.is_empty() {
                        args.push(MacroArg {
                            text: trimmed,
                            map: trimmed_map,
                        });
                    }
                    return Some((args, index + 1));
                }
                current_text.push(ch);
                current_map.push(index as u32);
                index += 1;
            }
            ',' if depth == 1 => {
                let trimmed = current_text.trim().to_string();
                let trimmed_map = trim_arg_map(&current_text, &current_map, trimmed.chars().count());
                args.push(MacroArg {
                    text: trimmed,
                    map: trimmed_map,
                });
                current_text.clear();
                current_map.clear();
                index += 1;
            }
            _ => {
                current_text.push(ch);
                current_map.push(index as u32);
                index += 1;
            }
        }
    }

    None
}

fn trim_arg_map(raw_text: &str, raw_map: &[u32], trimmed_len: usize) -> Vec<u32> {
    let start = raw_text
        .chars()
        .position(|ch| !ch.is_whitespace())
        .unwrap_or(0);
    let mut map = raw_map
        .iter()
        .skip(start)
        .take(trimmed_len)
        .copied()
        .collect::<Vec<_>>();
    let end = if trimmed_len == 0 {
        raw_map.last().copied().unwrap_or(0)
    } else {
        let start_index = start + trimmed_len - 1;
        raw_map
            .get(start_index + 1)
            .copied()
            .or_else(|| raw_map.get(start_index).copied().map(|value| value + 1))
            .unwrap_or(0)
    };
    map.push(end);
    map
}

fn extend_char_map(target: &mut Vec<u32>, text: &str, map: &[u32]) {
    let char_count = text.chars().count();
    for index in 0..char_count {
        target.push(map.get(index).copied().unwrap_or(0));
    }
}

fn read_identifier(chars: &[char], index: usize) -> Option<(String, usize)> {
    let ch = chars.get(index).copied()?;
    if !ch.is_ascii_alphabetic() && ch != '_' {
        return None;
    }

    let mut end = index + 1;
    while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
        end += 1;
    }
    Some((chars[index..end].iter().collect::<String>(), end))
}

fn skip_token_paste(chars: &[char], index: usize) -> Option<usize> {
    let mut probe = index;
    while probe < chars.len() && chars[probe].is_whitespace() {
        probe += 1;
    }
    if chars.get(probe).copied() == Some('#') && chars.get(probe + 1).copied() == Some('#') {
        probe += 2;
        while probe < chars.len() && chars[probe].is_whitespace() {
            probe += 1;
        }
        return Some(probe);
    }
    None
}

fn stringify_macro_argument(arg: &str) -> String {
    let escaped = arg.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn piece_with_single_column(text: impl Into<String>, column: u32) -> Piece {
    let text = text.into();
    let map = text.chars().map(|_| column).collect::<Vec<_>>();
    Piece { text, map }
}

fn trim_piece_map(text: &str, map: &[u32], fallback: u32) -> Vec<u32> {
    let char_count = text.chars().count();
    if char_count == 0 {
        return Vec::new();
    }

    (0..char_count)
        .map(|index| map.get(index).copied().unwrap_or(fallback))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::MacroTable;

    #[test]
    fn function_like_macro_expands_arguments() {
        let mut macros = MacroTable::default();
        macros.define_from_directive("ADD(X, Y) ((X) + (Y))");

        assert_eq!(macros.expand_line("int32 Answer = ADD(1, 2);"), "int32 Answer = ((1) + (2));");
    }

    #[test]
    fn function_like_macro_expands_nested_object_like_arguments() {
        let mut macros = MacroTable::default();
        macros.define("VALUE", "7");
        macros.define_from_directive("ID(X) X");

        assert_eq!(macros.expand_line("int32 Answer = ID(VALUE);"), "int32 Answer = 7;");
    }

    #[test]
    fn function_like_macro_preserves_original_invocation_column_map() {
        let mut macros = MacroTable::default();
        macros.define_from_directive("BADRET() LongIdentifier");

        let (expanded, map) = macros.expand_line_with_map("return BADRET();");
        assert_eq!(expanded, "return LongIdentifier;");
        assert_eq!(map.get(7), Some(&7));
        assert_eq!(map.get(8), Some(&7));
        assert_eq!(map.get(21), Some(&15));
        assert_eq!(map.last(), Some(&16));
    }

    #[test]
    fn function_like_macro_supports_stringification() {
        let mut macros = MacroTable::default();
        macros.define_from_directive("STR(X) #X");

        assert_eq!(macros.expand_line("const char* Name = STR(Foo Bar);"), "const char* Name = \"Foo Bar\";");
    }

    #[test]
    fn function_like_macro_stringification_preserves_macro_invocation_columns() {
        let mut macros = MacroTable::default();
        macros.define_from_directive("STR(X) #X");

        let (expanded, map) = macros.expand_line_with_map("return STR(Foo);");
        assert_eq!(expanded, "return \"Foo\";");
        assert!(map.iter().skip(7).take(5).all(|col| *col == 7));
        assert_eq!(map.last(), Some(&16));
    }

    #[test]
    fn function_like_macro_supports_token_pasting() {
        let mut macros = MacroTable::default();
        macros.define_from_directive("JOIN(A, B) A##B");

        assert_eq!(macros.expand_line("int32 Value = JOIN(Foo, Bar);"), "int32 Value = FooBar;");
    }
}
