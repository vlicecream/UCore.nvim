use super::{expand_include_operand, IncludeResolver, MacroTable};

pub fn evaluate_condition(expr: &str, macros: &MacroTable, include_resolver: Option<&IncludeResolver>) -> bool {
    evaluate_condition_value(expr, macros, include_resolver, 0) != 0
}

const MAX_CONDITION_EXPANSION_DEPTH: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Number(i64),
    LParen,
    RParen,
    Not,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    AndAnd,
    OrOr,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
}

struct Parser<'a> {
    tokens: &'a [Token],
    index: usize,
}

impl Parser<'_> {
    fn parse_expr(&mut self) -> i64 {
        self.parse_or()
    }

    fn parse_or(&mut self) -> i64 {
        let mut value = self.parse_and();
        while self.consume(&Token::OrOr) {
            value = i64::from(value != 0 || self.parse_and() != 0);
        }
        value
    }

    fn parse_and(&mut self) -> i64 {
        let mut value = self.parse_equality();
        while self.consume(&Token::AndAnd) {
            value = i64::from(value != 0 && self.parse_equality() != 0);
        }
        value
    }

    fn parse_equality(&mut self) -> i64 {
        let mut value = self.parse_relational();
        loop {
            if self.consume(&Token::EqEq) {
                value = i64::from(value == self.parse_relational());
            } else if self.consume(&Token::NotEq) {
                value = i64::from(value != self.parse_relational());
            } else {
                break;
            }
        }
        value
    }

    fn parse_relational(&mut self) -> i64 {
        let mut value = self.parse_additive();
        loop {
            if self.consume(&Token::Lt) {
                value = i64::from(value < self.parse_additive());
            } else if self.consume(&Token::Le) {
                value = i64::from(value <= self.parse_additive());
            } else if self.consume(&Token::Gt) {
                value = i64::from(value > self.parse_additive());
            } else if self.consume(&Token::Ge) {
                value = i64::from(value >= self.parse_additive());
            } else {
                break;
            }
        }
        value
    }

    fn parse_additive(&mut self) -> i64 {
        let mut value = self.parse_multiplicative();
        loop {
            if self.consume(&Token::Plus) {
                value += self.parse_multiplicative();
            } else if self.consume(&Token::Minus) {
                value -= self.parse_multiplicative();
            } else {
                break;
            }
        }
        value
    }

    fn parse_multiplicative(&mut self) -> i64 {
        let mut value = self.parse_unary();
        loop {
            if self.consume(&Token::Star) {
                value *= self.parse_unary();
            } else if self.consume(&Token::Slash) {
                let rhs = self.parse_unary();
                value = if rhs == 0 { 0 } else { value / rhs };
            } else if self.consume(&Token::Percent) {
                let rhs = self.parse_unary();
                value = if rhs == 0 { 0 } else { value % rhs };
            } else {
                break;
            }
        }
        value
    }

    fn parse_unary(&mut self) -> i64 {
        if self.consume(&Token::Not) {
            return i64::from(self.parse_unary() == 0);
        }
        if self.consume(&Token::Minus) {
            return -self.parse_unary();
        }
        if self.consume(&Token::Plus) {
            return self.parse_unary();
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> i64 {
        if self.consume(&Token::LParen) {
            let value = self.parse_expr();
            let _ = self.consume(&Token::RParen);
            return value;
        }

        match self.peek().cloned() {
            Some(Token::Number(value)) => {
                self.index += 1;
                value
            }
            _ => 0,
        }
    }

    fn consume(&mut self, token: &Token) -> bool {
        if self.peek() == Some(token) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.index)
    }
}

fn evaluate_condition_value(
    expr: &str,
    macros: &MacroTable,
    include_resolver: Option<&IncludeResolver>,
    depth: usize,
) -> i64 {
    let tokens = tokenize(expr, macros, include_resolver, depth);
    let mut parser = Parser { tokens: &tokens, index: 0 };
    parser.parse_expr()
}

fn tokenize(
    expr: &str,
    macros: &MacroTable,
    include_resolver: Option<&IncludeResolver>,
    depth: usize,
) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars = expr.chars().collect::<Vec<_>>();
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
            index += 1;
            continue;
        }

        if ch.is_ascii_digit() {
            let start = index;
            index += 1;
            while index < chars.len() && chars[index].is_ascii_hexdigit() {
                index += 1;
            }
            let text = chars[start..index].iter().collect::<String>();
            let value = if text.starts_with("0x") || text.starts_with("0X") {
                i64::from_str_radix(text.trim_start_matches("0x").trim_start_matches("0X"), 16).unwrap_or(0)
            } else {
                text.parse::<i64>().unwrap_or(0)
            };
            tokens.push(Token::Number(value));
            continue;
        }

        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = index;
            index += 1;
            while index < chars.len() && (chars[index].is_ascii_alphanumeric() || chars[index] == '_') {
                index += 1;
            }
            let ident = chars[start..index].iter().collect::<String>();
            if ident == "defined" {
                let (name, consumed) = parse_defined_operand(&chars[index..]);
                index += consumed;
                tokens.push(Token::Number(i64::from(macros.is_defined(&name))));
            } else if ident == "__has_include" {
                let (include, consumed) = parse_has_include_operand(&chars[index..]);
                index += consumed;
                let include = expand_include_operand(&include, macros);
                let present = include_resolver
                    .map(|resolver| resolver.has_include(&include))
                    .unwrap_or(false);
                tokens.push(Token::Number(i64::from(present)));
            } else if ident == "true" || ident == "TRUE" {
                tokens.push(Token::Number(1));
            } else if ident == "false" || ident == "FALSE" {
                tokens.push(Token::Number(0));
            } else if depth < MAX_CONDITION_EXPANSION_DEPTH && macros.has_function_macro(&ident) {
                let mut probe = index;
                while probe < chars.len() && chars[probe].is_whitespace() {
                    probe += 1;
                }
                if let Some(end_index) = parse_macro_invocation_end(&chars, probe) {
                    let invocation = chars[start..end_index].iter().collect::<String>();
                    let expanded = macros.expand_line(&invocation);
                    let value =
                        evaluate_condition_value(&expanded, macros, include_resolver, depth + 1);
                    tokens.push(Token::Number(value));
                    index = end_index;
                } else {
                    tokens.push(Token::Number(0));
                }
            } else {
                let value = macros
                    .value_of(&ident)
                    .map(|text| {
                        if depth >= MAX_CONDITION_EXPANSION_DEPTH {
                            text.parse::<i64>().unwrap_or(0)
                        } else {
                            evaluate_condition_value(text, macros, include_resolver, depth + 1)
                        }
                    })
                    .unwrap_or_else(|| i64::from(macros.is_defined(&ident)));
                tokens.push(Token::Number(value));
            }
            continue;
        }

        let next = chars.get(index + 1).copied();
        match (ch, next) {
            ('&', Some('&')) => {
                tokens.push(Token::AndAnd);
                index += 2;
            }
            ('|', Some('|')) => {
                tokens.push(Token::OrOr);
                index += 2;
            }
            ('=', Some('=')) => {
                tokens.push(Token::EqEq);
                index += 2;
            }
            ('!', Some('=')) => {
                tokens.push(Token::NotEq);
                index += 2;
            }
            ('<', Some('=')) => {
                tokens.push(Token::Le);
                index += 2;
            }
            ('>', Some('=')) => {
                tokens.push(Token::Ge);
                index += 2;
            }
            ('(', _) => {
                tokens.push(Token::LParen);
                index += 1;
            }
            (')', _) => {
                tokens.push(Token::RParen);
                index += 1;
            }
            ('!', _) => {
                tokens.push(Token::Not);
                index += 1;
            }
            ('+', _) => {
                tokens.push(Token::Plus);
                index += 1;
            }
            ('-', _) => {
                tokens.push(Token::Minus);
                index += 1;
            }
            ('*', _) => {
                tokens.push(Token::Star);
                index += 1;
            }
            ('/', _) => {
                tokens.push(Token::Slash);
                index += 1;
            }
            ('%', _) => {
                tokens.push(Token::Percent);
                index += 1;
            }
            ('<', _) => {
                tokens.push(Token::Lt);
                index += 1;
            }
            ('>', _) => {
                tokens.push(Token::Gt);
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }

    tokens
}

fn parse_defined_operand(chars: &[char]) -> (String, usize) {
    let mut index = 0usize;
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    if index >= chars.len() {
        return (String::new(), index);
    }

    if chars[index] == '(' {
        index += 1;
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        let start = index;
        while index < chars.len() && (chars[index].is_ascii_alphanumeric() || chars[index] == '_') {
            index += 1;
        }
        let name = chars[start..index].iter().collect::<String>();
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index < chars.len() && chars[index] == ')' {
            index += 1;
        }
        return (name, index);
    }

    let start = index;
    while index < chars.len() && (chars[index].is_ascii_alphanumeric() || chars[index] == '_') {
        index += 1;
    }
    (chars[start..index].iter().collect::<String>(), index)
}

fn parse_has_include_operand(chars: &[char]) -> (String, usize) {
    let mut index = 0usize;
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    if chars.get(index).copied() != Some('(') {
        return (String::new(), index);
    }

    index += 1;
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    if index >= chars.len() {
        return (String::new(), index);
    }

    let opener = chars[index];
    let closer = match opener {
        '"' => '"',
        '<' => '>',
        _ => ')',
    };

    let mut value = String::new();
    if opener == '"' || opener == '<' {
        index += 1;
        while index < chars.len() && chars[index] != closer {
            value.push(chars[index]);
            index += 1;
        }
        if index < chars.len() && chars[index] == closer {
            index += 1;
        }
    } else {
        let mut depth = 0usize;
        while index < chars.len() {
            match chars[index] {
                '(' => {
                    depth += 1;
                    value.push(chars[index]);
                }
                ')' if depth == 0 => break,
                ')' => {
                    depth = depth.saturating_sub(1);
                    value.push(chars[index]);
                }
                _ => value.push(chars[index]),
            }
            index += 1;
        }
    }

    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    if chars.get(index).copied() == Some(')') {
        index += 1;
    }

    (value.trim().to_string(), index)
}

fn parse_macro_invocation_end(chars: &[char], open_paren_index: usize) -> Option<usize> {
    if chars.get(open_paren_index).copied() != Some('(') {
        return None;
    }

    let mut index = open_paren_index + 1;
    let mut depth = 1usize;
    let mut in_string = false;
    let mut in_char = false;
    let mut escape = false;

    while index < chars.len() {
        let ch = chars[index];
        if in_string {
            if !escape && ch == '"' {
                in_string = false;
            }
            escape = ch == '\\' && !escape;
            index += 1;
            continue;
        }
        if in_char {
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
            }
            '\'' => {
                in_char = true;
            }
            '(' => {
                depth += 1;
            }
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
        escape = false;
        index += 1;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::evaluate_condition;
    use crate::preproc::{default_include_resolver_for_file, MacroTable};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn condition_eval_handles_defined_and_boolean_operators() {
        let mut macros = MacroTable::default();
        macros.define("FOO", "1");
        macros.define("BAR", "0");
        assert!(evaluate_condition("defined(FOO) && !defined(BAZ)", &macros, None));
        assert!(!evaluate_condition("defined(BAR) && BAR", &macros, None));
    }

    #[test]
    fn condition_eval_handles_relational_math() {
        let mut macros = MacroTable::default();
        macros.define("VALUE", "7");
        assert!(evaluate_condition("VALUE * 2 >= 14", &macros, None));
    }

    #[test]
    fn condition_eval_expands_expression_like_object_macro() {
        let mut macros = MacroTable::default();
        macros.define("VALUE", "(1 + 1)");
        assert!(evaluate_condition("VALUE == 2", &macros, None));
    }

    #[test]
    fn condition_eval_expands_function_like_macro() {
        let mut macros = MacroTable::default();
        macros.define_from_directive("ADD(X, Y) ((X) + (Y))");
        assert!(evaluate_condition("ADD(1, 2) == 3", &macros, None));
    }

    #[test]
    fn condition_eval_treats_uninvoked_function_macro_as_zero() {
        let mut macros = MacroTable::default();
        macros.define_from_directive("FLAG(X) X");
        assert!(!evaluate_condition("FLAG", &macros, None));
    }

    #[test]
    fn condition_eval_handles_has_include() {
        let root = std::env::temp_dir().join(format!(
            "ucore_condition_has_include_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = root.join("Source/MyGame/Private/Test.cpp");
        let header = root.join("Source/MyGame/Public/MyHeader.h");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::create_dir_all(header.parent().unwrap()).unwrap();
        fs::write(root.join("MyGame.uproject"), "{}").unwrap();
        fs::write(&file, "").unwrap();
        fs::write(&header, "// header").unwrap();

        let resolver =
            default_include_resolver_for_file("preprocessor.toml", Some(&file.to_string_lossy()));
        let macros = MacroTable::default();
        assert!(evaluate_condition(
            "__has_include(\"MyHeader.h\")",
            &macros,
            Some(&resolver)
        ));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn condition_eval_handles_has_include_via_macro_operand() {
        let root = std::env::temp_dir().join(format!(
            "ucore_condition_has_include_macro_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = root.join("Source/MyGame/Private/Test.cpp");
        let header = root.join("Source/MyGame/Public/MyHeader.h");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::create_dir_all(header.parent().unwrap()).unwrap();
        fs::write(root.join("MyGame.uproject"), "{}").unwrap();
        fs::write(&file, "").unwrap();
        fs::write(&header, "// header").unwrap();

        let resolver =
            default_include_resolver_for_file("preprocessor.toml", Some(&file.to_string_lossy()));
        let mut macros = MacroTable::default();
        macros.define("HEADER_FILE", "\"MyHeader.h\"");
        assert!(evaluate_condition(
            "__has_include(HEADER_FILE)",
            &macros,
            Some(&resolver)
        ));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn condition_eval_handles_has_include_via_function_macro_operand() {
        let root = std::env::temp_dir().join(format!(
            "ucore_condition_has_include_fn_macro_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = root.join("Source/MyGame/Private/Test.cpp");
        let header = root.join("Source/MyGame/Public/MyHeader.h");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::create_dir_all(header.parent().unwrap()).unwrap();
        fs::write(root.join("MyGame.uproject"), "{}").unwrap();
        fs::write(&file, "").unwrap();
        fs::write(&header, "// header").unwrap();

        let resolver =
            default_include_resolver_for_file("preprocessor.toml", Some(&file.to_string_lossy()));
        let mut macros = MacroTable::default();
        macros.define_from_directive("HEADER_FILE() \"MyHeader.h\"");
        assert!(evaluate_condition(
            "__has_include(HEADER_FILE())",
            &macros,
            Some(&resolver)
        ));

        let _ = fs::remove_dir_all(root);
    }
}
