#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Hash,
    Identifier,
    Number,
    StringLike,
    Punct,
    Whitespace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: TokenKind,
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}

pub fn tokenize(line: &str) -> Vec<Token<'_>> {
    let chars = line.char_indices().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0usize;

    while index < chars.len() {
        let (start, ch) = chars[index];

        if ch.is_ascii_whitespace() {
            index += 1;
            while index < chars.len() && chars[index].1.is_ascii_whitespace() {
                index += 1;
            }
            let end = byte_end(line, &chars, index);
            tokens.push(Token {
                kind: TokenKind::Whitespace,
                text: &line[start..end],
                start,
                end,
            });
            continue;
        }

        if ch == '#' {
            index += 1;
            let end = byte_end(line, &chars, index);
            tokens.push(Token {
                kind: TokenKind::Hash,
                text: &line[start..end],
                start,
                end,
            });
            continue;
        }

        if ch.is_ascii_alphabetic() || ch == '_' {
            index += 1;
            while index < chars.len() {
                let next = chars[index].1;
                if next.is_ascii_alphanumeric() || next == '_' {
                    index += 1;
                } else {
                    break;
                }
            }
            let end = byte_end(line, &chars, index);
            tokens.push(Token {
                kind: TokenKind::Identifier,
                text: &line[start..end],
                start,
                end,
            });
            continue;
        }

        if ch.is_ascii_digit() {
            index += 1;
            while index < chars.len() {
                let next = chars[index].1;
                if next.is_ascii_alphanumeric() || next == '_' {
                    index += 1;
                } else {
                    break;
                }
            }
            let end = byte_end(line, &chars, index);
            tokens.push(Token {
                kind: TokenKind::Number,
                text: &line[start..end],
                start,
                end,
            });
            continue;
        }

        if ch == '"' || ch == '\'' || ch == '<' {
            let closer = match ch {
                '"' => '"',
                '\'' => '\'',
                '<' => '>',
                _ => ch,
            };
            index += 1;
            while index < chars.len() {
                let next = chars[index].1;
                index += 1;
                if next == '\\' && closer != '>' && index < chars.len() {
                    index += 1;
                    continue;
                }
                if next == closer {
                    break;
                }
            }
            let end = byte_end(line, &chars, index);
            tokens.push(Token {
                kind: TokenKind::StringLike,
                text: &line[start..end],
                start,
                end,
            });
            continue;
        }

        index += 1;
        let end = byte_end(line, &chars, index);
        tokens.push(Token {
            kind: TokenKind::Punct,
            text: &line[start..end],
            start,
            end,
        });
    }

    tokens
}

fn byte_end(line: &str, chars: &[(usize, char)], index: usize) -> usize {
    chars
        .get(index)
        .map(|(byte, _)| *byte)
        .unwrap_or(line.len())
}

pub fn parse_directive(trimmed_line: &str) -> Option<Directive<'_>> {
    let tokens = tokenize(trimmed_line);
    let mut non_ws = tokens.iter().filter(|token| token.kind != TokenKind::Whitespace);
    let hash = non_ws.next()?;
    if hash.kind != TokenKind::Hash {
        return None;
    }
    let name = non_ws.next()?;
    if name.kind != TokenKind::Identifier {
        return None;
    }
    let body_start = name.end;
    Some(Directive {
        name: name.text,
        body: trimmed_line[body_start..].trim_start(),
    })
}

pub fn first_identifier(text: &str) -> Option<Token<'_>> {
    tokenize(text)
        .into_iter()
        .find(|token| token.kind == TokenKind::Identifier)
}

pub struct Directive<'a> {
    pub name: &'a str,
    pub body: &'a str,
}

#[cfg(test)]
mod tests {
    use super::{first_identifier, parse_directive, tokenize, TokenKind};

    #[test]
    fn tokenizer_splits_basic_preprocessor_line() {
        let tokens = tokenize("#define VALUE 7");
        assert_eq!(tokens[0].kind, TokenKind::Hash);
        assert_eq!(tokens[1].kind, TokenKind::Identifier);
        assert_eq!(tokens[1].text, "define");
        assert!(tokens.iter().any(|token| token.text == "VALUE"));
    }

    #[test]
    fn tokenizer_parses_directive_name_and_body() {
        let directive = parse_directive("#ifdef MY_FLAG").unwrap();
        assert_eq!(directive.name, "ifdef");
        assert_eq!(directive.body, "MY_FLAG");
    }

    #[test]
    fn tokenizer_finds_first_identifier() {
        let token = first_identifier("  VALUE(X, Y)").unwrap();
        assert_eq!(token.text, "VALUE");
    }
}
