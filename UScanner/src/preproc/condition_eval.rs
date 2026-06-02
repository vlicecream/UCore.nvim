use super::MacroTable;

pub fn evaluate_condition(expr: &str, macros: &MacroTable) -> bool {
    let tokens = tokenize(expr, macros);
    let mut parser = Parser { tokens: &tokens, index: 0 };
    parser.parse_expr() != 0
}

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

fn tokenize(expr: &str, macros: &MacroTable) -> Vec<Token> {
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
            } else if ident == "true" || ident == "TRUE" {
                tokens.push(Token::Number(1));
            } else if ident == "false" || ident == "FALSE" {
                tokens.push(Token::Number(0));
            } else {
                let value = macros
                    .value_of(&ident)
                    .and_then(|text| text.parse::<i64>().ok())
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

#[cfg(test)]
mod tests {
    use super::evaluate_condition;
    use crate::preproc::MacroTable;

    #[test]
    fn condition_eval_handles_defined_and_boolean_operators() {
        let mut macros = MacroTable::default();
        macros.define("FOO", "1");
        macros.define("BAR", "0");
        assert!(evaluate_condition("defined(FOO) && !defined(BAZ)", &macros));
        assert!(!evaluate_condition("defined(BAR) && BAR", &macros));
    }

    #[test]
    fn condition_eval_handles_relational_math() {
        let mut macros = MacroTable::default();
        macros.define("VALUE", "7");
        assert!(evaluate_condition("VALUE * 2 >= 14", &macros));
    }
}
