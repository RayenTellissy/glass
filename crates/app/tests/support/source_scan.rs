//! A small Rust lexer and the source rules for UI code: user-facing text goes through
//! `t!()` with a key that exists in `assets/i18n/en.json`, and colours come from the
//! theme. Used by `tests/ui_source.rs`.
//!
//! The lexer knows comments, strings (plain, byte, C and raw), chars, lifetimes,
//! numbers, identifiers and punctuation, which is all the rules need. It does not
//! expand macros, so it checks what is written, not what a macro generates.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// A comment containing this marker, followed by a reason, allows literal text on its
/// own line and the next one.
pub const ALLOW_TEXT: &str = "allow-literal-text:";

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    /// The literal's contents between the quotes, escapes left as written.
    Str(String),
    Char,
    Lifetime,
    Number(String),
    Punct(char),
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub line: usize,
}

#[derive(Debug, Default)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    /// Comment text by the line it starts on.
    pub comments: HashMap<usize, Vec<String>>,
}

impl Lexed {
    /// Whether a comment on `line` or the line before carries `marker` and a reason.
    pub fn allowed(&self, line: usize, marker: &str) -> bool {
        [line, line.saturating_sub(1)].iter().any(|l| {
            self.comments.get(l).is_some_and(|comments| {
                comments.iter().any(|c| {
                    c.find(marker)
                        .is_some_and(|i| !c[i + marker.len()..].trim().is_empty())
                })
            })
        })
    }
}

pub fn lex(src: &str) -> Lexed {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Lexed::default();
    let mut i = 0;
    let mut line = 1;
    let at = |i: usize| chars.get(i).copied().unwrap_or('\0');
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && at(i + 1) == '/' {
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            out.comments.entry(line).or_default().push(text);
        } else if c == '/' && at(i + 1) == '*' {
            let (start, start_line) = (i, line);
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && at(i + 1) == '*' {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && at(i + 1) == '/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
            let text: String = chars[start..i.min(chars.len())].iter().collect();
            out.comments.entry(start_line).or_default().push(text);
        } else if let Some((prefix_len, raw)) = string_prefix(&chars, i) {
            let start_line = line;
            i += prefix_len;
            let mut hashes = 0;
            if raw {
                while at(i) == '#' {
                    hashes += 1;
                    i += 1;
                }
            }
            i += 1; // opening quote
            let body_start = i;
            loop {
                if i >= chars.len() {
                    break;
                }
                let ch = chars[i];
                if ch == '\n' {
                    line += 1;
                }
                if !raw && ch == '\\' {
                    if at(i + 1) == '\n' {
                        line += 1;
                    }
                    i += 2;
                    continue;
                }
                if ch == '"' && (0..hashes).all(|h| at(i + 1 + h) == '#') {
                    break;
                }
                i += 1;
            }
            let body: String = chars[body_start..i.min(chars.len())].iter().collect();
            i += 1 + hashes;
            out.tokens.push(Token {
                tok: Tok::Str(body),
                line: start_line,
            });
        } else if c == '\'' || (c == 'b' && at(i + 1) == '\'') {
            if c == 'b' {
                i += 1;
            }
            if at(i + 1) == '\\' {
                // Past the quote, the backslash and the escaped character.
                i += 3;
                while i < chars.len() && chars[i] != '\'' {
                    i += 1;
                }
                i += 1;
                out.tokens.push(Token {
                    tok: Tok::Char,
                    line,
                });
            } else if at(i + 2) == '\'' {
                i += 3;
                out.tokens.push(Token {
                    tok: Tok::Char,
                    line,
                });
            } else {
                i += 1;
                while is_ident_char(at(i)) {
                    i += 1;
                }
                out.tokens.push(Token {
                    tok: Tok::Lifetime,
                    line,
                });
            }
        } else if c.is_ascii_digit() {
            let start = i;
            while is_ident_char(at(i)) || (at(i) == '.' && at(i + 1).is_ascii_digit()) {
                i += 1;
            }
            out.tokens.push(Token {
                tok: Tok::Number(chars[start..i].iter().collect()),
                line,
            });
        } else if is_ident_start(c) {
            if c == 'r' && at(i + 1) == '#' {
                i += 2;
            }
            let start = i;
            while is_ident_char(at(i)) {
                i += 1;
            }
            out.tokens.push(Token {
                tok: Tok::Ident(chars[start..i].iter().collect()),
                line,
            });
        } else {
            out.tokens.push(Token {
                tok: Tok::Punct(c),
                line,
            });
            i += 1;
        }
    }
    out
}

/// At a string literal start: the prefix length (before any `#`) and whether it is raw.
fn string_prefix(chars: &[char], i: usize) -> Option<(usize, bool)> {
    let at = |j: usize| chars.get(i + j).copied().unwrap_or('\0');
    if i > 0 && is_ident_char(chars[i - 1]) {
        return None;
    }
    for (prefix, raw) in [
        ("br", true),
        ("cr", true),
        ("r", true),
        ("b", false),
        ("c", false),
        ("", false),
    ] {
        let n = prefix.len();
        if !prefix.chars().enumerate().all(|(j, p)| at(j) == p) {
            continue;
        }
        if raw {
            let mut j = n;
            while at(j) == '#' {
                j += 1;
            }
            if at(j) == '"' {
                return Some((n, true));
            }
        } else if at(n) == '"' {
            return Some((n, false));
        }
    }
    None
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Drops `#[cfg(test)]` items (test modules and functions) from the token stream.
pub fn without_test_items(tokens: &[Token]) -> Vec<Token> {
    let is = |i: usize, t: &Tok| tokens.get(i).is_some_and(|x| &x.tok == t);
    let ident = |s: &str| Tok::Ident(s.to_owned());
    let mut out = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        let cfg_test = is(i, &Tok::Punct('#'))
            && is(i + 1, &Tok::Punct('['))
            && is(i + 2, &ident("cfg"))
            && is(i + 3, &Tok::Punct('('))
            && is(i + 4, &ident("test"))
            && is(i + 5, &Tok::Punct(')'))
            && is(i + 6, &Tok::Punct(']'));
        if !cfg_test {
            out.push(tokens[i].clone());
            i += 1;
            continue;
        }
        // Skip to the item's body and past its matching brace (or its `;`).
        i += 7;
        while i < tokens.len() && !is(i, &Tok::Punct('{')) && !is(i, &Tok::Punct(';')) {
            i += 1;
        }
        if is(i, &Tok::Punct('{')) {
            i = matching(tokens, i) + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Index of the bracket closing the one at `open`, or the last token.
pub fn matching(tokens: &[Token], open: usize) -> usize {
    let mut depth = 0i32;
    for (j, token) in tokens.iter().enumerate().skip(open) {
        match token.tok {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => {
                depth -= 1;
                if depth == 0 {
                    return j;
                }
            }
            _ => {}
        }
    }
    tokens.len().saturating_sub(1)
}

/// A rule violation in one file.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Violation {
    pub line: usize,
    pub message: String,
}

/// One `t!("key", name = value)` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyUse {
    pub key: String,
    pub line: usize,
    pub args: BTreeSet<String>,
}

/// egui methods whose arguments are shown to the user.
pub const TEXT_METHODS: &[&str] = &[
    "button",
    "checkbox",
    "code",
    "collapsing",
    "colored_label",
    "heading",
    "hint_text",
    "hyperlink_to",
    "label",
    "link",
    "menu_button",
    "monospace",
    "on_disabled_hover_text",
    "on_hover_text",
    "on_hover_text_at_pointer",
    "prefix",
    "radio",
    "radio_value",
    "selectable_label",
    "selectable_value",
    "selected_text",
    "small",
    "small_button",
    "strong",
    "suffix",
    "text",
    "toggle_value",
    "weak",
];

/// egui constructors (`Type::function`) whose arguments are shown to the user.
pub const TEXT_CONSTRUCTORS: &[(&str, &str)] = &[
    ("Button", "new"),
    ("Button", "image_and_text"),
    ("Checkbox", "new"),
    ("CollapsingHeader", "new"),
    ("ComboBox", "from_label"),
    ("Hyperlink", "from_label_and_url"),
    ("Label", "new"),
    ("Link", "new"),
    ("RadioButton", "new"),
    ("RichText", "new"),
    ("SelectableLabel", "new"),
    ("Window", "new"),
];

/// Whether a string literal reads as words once `{...}` format placeholders are removed.
pub fn is_wordy(literal: &str) -> bool {
    let mut depth = 0;
    let mut chars = literal.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
            }
            '}' if depth == 0 && chars.peek() == Some(&'}') => {
                chars.next();
            }
            '{' => depth += 1,
            '}' => depth -= 1,
            '\\' => {
                // `\n`, `\t`, `\u{..}` are escapes, not words.
                chars.next();
            }
            c if depth == 0 && c.is_alphabetic() => return true,
            _ => {}
        }
    }
    false
}

fn is_t_macro(tokens: &[Token], i: usize) -> bool {
    matches!(&tokens[i].tok, Tok::Ident(name) if name == "t")
        && tokens.get(i + 1).is_some_and(|t| t.tok == Tok::Punct('!'))
        && tokens
            .get(i + 2)
            .is_some_and(|t| matches!(t.tok, Tok::Punct('(' | '[' | '{')))
        && !(i > 0 && tokens[i - 1].tok == Tok::Punct('.'))
}

/// Checks one file's text rules. Returns the `t!` keys it uses and the violations.
pub fn check_text(src: &str) -> (Vec<KeyUse>, Vec<Violation>) {
    let lexed = lex(src);
    let tokens = without_test_items(&lexed.tokens);
    let mut uses = Vec::new();
    // Keyed by token index so a literal inside two text calls is reported once.
    let mut violations = BTreeMap::new();

    for i in 0..tokens.len() {
        if is_t_macro(&tokens, i) {
            let close = matching(&tokens, i + 2);
            match tokens.get(i + 3).map(|t| &t.tok) {
                Some(Tok::Str(key)) => {
                    let mut args = BTreeSet::new();
                    let mut depth = 0;
                    for j in i + 4..close {
                        match &tokens[j].tok {
                            Tok::Punct('(' | '[' | '{') => depth += 1,
                            Tok::Punct(')' | ']' | '}') => depth -= 1,
                            Tok::Ident(name)
                                if depth == 0
                                    && tokens[j - 1].tok == Tok::Punct(',')
                                    && tokens
                                        .get(j + 1)
                                        .is_some_and(|t| t.tok == Tok::Punct('=')) =>
                            {
                                args.insert(name.clone());
                            }
                            _ => {}
                        }
                    }
                    uses.push(KeyUse {
                        key: key.clone(),
                        line: tokens[i].line,
                        args,
                    });
                }
                _ => {
                    violations.insert(
                        i,
                        Violation {
                            line: tokens[i].line,
                            message: "t!() needs a string literal key so the key can be checked"
                                .into(),
                        },
                    );
                }
            }
        }

        let Some((name, open)) = text_call(&tokens, i) else {
            continue;
        };
        let close = matching(&tokens, open);
        let mut j = open + 1;
        while j < close {
            if is_t_macro(&tokens, j) {
                j = matching(&tokens, j + 2) + 1;
                continue;
            }
            match &tokens[j].tok {
                // Closure bodies and blocks are not the text argument; calls inside them
                // are checked on their own.
                Tok::Punct('{') => {
                    j = matching(&tokens, j) + 1;
                    continue;
                }
                Tok::Str(text) if is_wordy(text) && !lexed.allowed(tokens[j].line, ALLOW_TEXT) => {
                    violations.entry(j).or_insert(Violation {
                        line: tokens[j].line,
                        message: format!(
                            "literal text {:?} passed to `{name}`: add a key to assets/i18n/en.json and use t!(), or mark a debug-only string with `// {ALLOW_TEXT} <reason>`",
                            text
                        ),
                    });
                }
                _ => {}
            }
            j += 1;
        }
    }
    (uses, violations.into_values().collect())
}

/// At a text API call: its name and the index of its `(`.
fn text_call(tokens: &[Token], i: usize) -> Option<(String, usize)> {
    let Tok::Ident(name) = &tokens[i].tok else {
        return None;
    };
    let open = i + 1;
    if tokens.get(open)?.tok != Tok::Punct('(') {
        return None;
    }
    if i > 0 && tokens[i - 1].tok == Tok::Punct('.') && TEXT_METHODS.contains(&name.as_str()) {
        return Some((name.clone(), open));
    }
    if i >= 3
        && tokens[i - 1].tok == Tok::Punct(':')
        && tokens[i - 2].tok == Tok::Punct(':')
        && let Tok::Ident(ty) = &tokens[i - 3].tok
        && TEXT_CONSTRUCTORS.contains(&(ty.as_str(), name.as_str()))
    {
        return Some((format!("{ty}::{name}"), open));
    }
    None
}

/// Placeholder names in a string (`{name}`; `{{` and `}}` are literal braces).
pub fn placeholder_names(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            if chars.peek() == Some(&'{') {
                chars.next();
                continue;
            }
            let name: String = chars.by_ref().take_while(|&c| c != '}').collect();
            names.insert(name);
        }
    }
    names
}

/// Checks `t!` uses against the English strings: the key exists and the named arguments
/// match the string's placeholders.
pub fn check_keys(uses: &[KeyUse], strings: &BTreeMap<String, String>) -> Vec<Violation> {
    let mut violations = Vec::new();
    for u in uses {
        match strings.get(&u.key) {
            None => violations.push(Violation {
                line: u.line,
                message: format!("t!({:?}): no such key in assets/i18n/en.json", u.key),
            }),
            Some(text) => {
                let wanted = placeholder_names(text);
                if wanted != u.args {
                    violations.push(Violation {
                        line: u.line,
                        message: format!(
                            "t!({:?}): the string has placeholders {wanted:?} but the call passes {:?}",
                            u.key, u.args
                        ),
                    });
                }
            }
        }
    }
    violations
}

/// Keys in `strings` that no use refers to.
pub fn unused_keys<'a>(strings: &'a BTreeMap<String, String>, uses: &[KeyUse]) -> Vec<&'a str> {
    let used: BTreeSet<&str> = uses.iter().map(|u| u.key.as_str()).collect();
    strings
        .keys()
        .map(String::as_str)
        .filter(|k| !used.contains(k))
        .collect()
}

/// A comment containing this marker, followed by a reason, allows a literal colour on
/// its own line and the next one.
pub const ALLOW_COLOUR: &str = "allow-literal-colour:";

/// Colour types whose constructors and constants make a colour from numbers.
pub const COLOUR_TYPES: &[&str] = &["Color32", "Rgba", "Hsva", "HsvaGamma", "Rgba8"];

/// Constants that are not colours in the design sense: no fill, and egui's
/// "use the default text colour" marker.
pub const COLOUR_CONSTANTS_ALLOWED: &[&str] = &["TRANSPARENT", "PLACEHOLDER"];

/// `#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`.
pub fn is_hex_colour(literal: &str) -> bool {
    literal.strip_prefix('#').is_some_and(|digits| {
        matches!(digits.len(), 3 | 4 | 6 | 8) && digits.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// Checks one file for literal colours: `Color32::from_rgb(..)` and the other `from_*`
/// constructors, `Color32::RED`-style constants, `hex_color!`, and hex colour strings.
pub fn check_colours(src: &str) -> Vec<Violation> {
    let lexed = lex(src);
    let tokens = without_test_items(&lexed.tokens);
    let mut violations = Vec::new();
    let mut flag = |line: usize, what: String| {
        if !lexed.allowed(line, ALLOW_COLOUR) {
            violations.push(Violation {
                line,
                message: format!(
                    "literal colour {what}: use a theme role (`ui::theme::current(ctx).color(Role::..)`), or mark a debug-only colour with `// {ALLOW_COLOUR} <reason>`"
                ),
            });
        }
    };
    for i in 0..tokens.len() {
        match &tokens[i].tok {
            Tok::Ident(ty)
                if COLOUR_TYPES.contains(&ty.as_str())
                    && tokens.get(i + 1).is_some_and(|t| t.tok == Tok::Punct(':'))
                    && tokens.get(i + 2).is_some_and(|t| t.tok == Tok::Punct(':')) =>
            {
                if let Some(Tok::Ident(name)) = tokens.get(i + 3).map(|t| &t.tok) {
                    let constant = name.len() > 1
                        && name
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
                    let literal = name.starts_with("from_")
                        || name == "new"
                        || (constant && !COLOUR_CONSTANTS_ALLOWED.contains(&name.as_str()));
                    if literal {
                        flag(tokens[i].line, format!("`{ty}::{name}`"));
                    }
                }
            }
            Tok::Ident(name)
                if (name == "hex_color" || name == "color_hex")
                    && tokens.get(i + 1).is_some_and(|t| t.tok == Tok::Punct('!')) =>
            {
                flag(tokens[i].line, format!("`{name}!`"));
            }
            Tok::Str(text) if is_hex_colour(text) => {
                flag(tokens[i].line, format!("{text:?}"));
            }
            _ => {}
        }
    }
    violations
}
