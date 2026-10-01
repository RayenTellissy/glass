//! Maths typed into numeric fields: `112`, `112+10`, `+10` (relative to the current
//! value), `*2`, `-4.5/2`, `(1+2)*3`. Numbers, `+ - * /`, unary minus and parentheses,
//! evaluated left to right with the usual precedence.

/// The value `text` evaluates to; a leading operator applies to `current`.
pub fn evaluate(text: &str, current: f64) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let first = text.chars().next()?;
    let expr = if matches!(first, '+' | '*' | '/') {
        format!("{current}{text}")
    } else {
        text.to_owned()
    };
    let tokens = tokenize(&expr)?;
    let mut parser = Parser { tokens, at: 0 };
    let v = parser.sum()?;
    (parser.at == parser.tokens.len() && v.is_finite()).then_some(v)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Token {
    Num(f64),
    Op(char),
}

fn tokenize(s: &str) -> Option<Vec<Token>> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let n: String = chars[start..i].iter().collect();
            out.push(Token::Num(n.parse().ok()?));
        } else if "+-*/()".contains(c) {
            out.push(Token::Op(c));
            i += 1;
        } else if c == '\u{2212}' {
            out.push(Token::Op('-'));
            i += 1;
        } else {
            return None;
        }
    }
    Some(out)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<Token> {
        self.tokens.get(self.at).copied()
    }

    fn sum(&mut self) -> Option<f64> {
        let mut v = self.product()?;
        while let Some(Token::Op(op @ ('+' | '-'))) = self.peek() {
            self.at += 1;
            let r = self.product()?;
            v = if op == '+' { v + r } else { v - r };
        }
        Some(v)
    }

    fn product(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        while let Some(Token::Op(op @ ('*' | '/'))) = self.peek() {
            self.at += 1;
            let r = self.unary()?;
            v = if op == '*' { v * r } else { v / r };
        }
        Some(v)
    }

    fn unary(&mut self) -> Option<f64> {
        match self.peek()? {
            Token::Op('-') => {
                self.at += 1;
                Some(-self.unary()?)
            }
            Token::Op('(') => {
                self.at += 1;
                let v = self.sum()?;
                (self.peek()? == Token::Op(')')).then(|| self.at += 1)?;
                Some(v)
            }
            Token::Num(n) => {
                self.at += 1;
                Some(n)
            }
            Token::Op(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::evaluate;

    #[test]
    fn plain_relative_and_nested() {
        assert_eq!(evaluate("112", 0.0), Some(112.0));
        assert_eq!(evaluate("112+10", 0.0), Some(122.0));
        assert_eq!(evaluate("+10", 112.0), Some(122.0));
        assert_eq!(evaluate("*2", 50.0), Some(100.0));
        assert_eq!(evaluate("-20", 50.0), Some(-20.0));
        assert_eq!(evaluate("(1+2)*3 - 1", 0.0), Some(8.0));
        assert_eq!(evaluate("2*-3", 0.0), Some(-6.0));
        assert_eq!(evaluate("\u{2212}12", 0.0), Some(-12.0));
    }

    #[test]
    fn rejects_garbage() {
        for s in ["", "abc", "1+", "(1", "1)", "1/0", "1..2"] {
            assert_eq!(evaluate(s, 5.0), None, "{s:?}");
        }
    }
}
