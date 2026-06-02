use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct MacroTable {
    object_like: HashMap<String, String>,
}

impl MacroTable {
    pub fn define(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.object_like.insert(name.into(), value.into());
    }

    pub fn undefine(&mut self, name: &str) {
        self.object_like.remove(name);
    }

    pub fn is_defined(&self, name: &str) -> bool {
        self.object_like.contains_key(name)
    }

    pub fn value_of(&self, name: &str) -> Option<&str> {
        self.object_like.get(name).map(String::as_str)
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

        let mut chars = trimmed.chars().peekable();
        let mut name = String::new();
        while let Some(ch) = chars.peek().copied() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                name.push(ch);
                chars.next();
            } else {
                break;
            }
        }

        if name.is_empty() || chars.peek() == Some(&'(') {
            return;
        }

        let value = chars.collect::<String>().trim().to_string();
        self.define(name, if value.is_empty() { "1".to_string() } else { value });
    }

    pub fn expand_line(&self, line: &str) -> String {
        let mut out = String::with_capacity(line.len());
        let mut token = String::new();

        let flush_token = |token: &mut String, out: &mut String, table: &MacroTable| {
            if token.is_empty() {
                return;
            }
            if let Some(value) = table.value_of(token) {
                out.push_str(value);
            } else {
                out.push_str(token);
            }
            token.clear();
        };

        for ch in line.chars() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                token.push(ch);
            } else {
                flush_token(&mut token, &mut out, self);
                out.push(ch);
            }
        }
        flush_token(&mut token, &mut out, self);

        out
    }
}
