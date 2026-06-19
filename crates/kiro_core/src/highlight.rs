//! A tiny, dependency-free syntax tokenizer used to colorize fenced code
//! blocks. It is intentionally generic (a union of common keywords) rather than
//! a full grammar — enough to make code readable — and lives in `kiro_core` so
//! it can be unit-tested without GPUI.
//!
//! Tokenization is line-oriented: callers split code on `\n` and tokenize each
//! line, which keeps state simple (line comments and single-line strings).

/// The category of a highlighted token, mapped to a color by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// A language keyword.
    Keyword,
    /// A string or char literal.
    Str,
    /// A line comment.
    Comment,
    /// A numeric literal.
    Number,
    /// An identifier or other word.
    Ident,
    /// Punctuation/operators.
    Punct,
    /// Whitespace or anything else.
    Plain,
}

/// A contiguous run of text sharing one [`TokenKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// The text of this span.
    pub text: String,
    /// Its category.
    pub kind: TokenKind,
}

const KEYWORDS: &[&str] = &[
    // Rust
    "fn",
    "let",
    "mut",
    "pub",
    "struct",
    "enum",
    "impl",
    "use",
    "trait",
    "mod",
    "crate",
    "where",
    "dyn",
    "move",
    "ref",
    "as",
    "self",
    "super",
    "unsafe",
    "async",
    "await",
    "match",
    "loop",
    // Common control flow / decls across many languages
    "if",
    "else",
    "for",
    "while",
    "return",
    "break",
    "continue",
    "in",
    "const",
    "static",
    "type",
    "class",
    "def",
    "import",
    "from",
    "function",
    "var",
    "new",
    "public",
    "private",
    "protected",
    "void",
    "int",
    "float",
    "double",
    "bool",
    "string",
    "char",
    "try",
    "catch",
    "except",
    "finally",
    "throw",
    "raise",
    "with",
    "yield",
    "lambda",
    "and",
    "or",
    "not",
    "is",
    "None",
    "null",
    "nil",
    "true",
    "false",
    "True",
    "False",
    "package",
    "interface",
    "extends",
    "implements",
    "switch",
    "case",
    "default",
    "do",
    "then",
    "end",
    "elif",
    "echo",
    "export",
];

fn keywords_set() -> std::collections::HashSet<&'static str> {
    KEYWORDS.iter().copied().collect()
}

fn hash_comment_lang(lang: Option<&str>) -> bool {
    matches!(
        lang.map(|l| l.to_lowercase()).as_deref(),
        Some(
            "python"
                | "py"
                | "sh"
                | "bash"
                | "zsh"
                | "shell"
                | "ruby"
                | "rb"
                | "yaml"
                | "yml"
                | "toml"
                | "ini"
                | "makefile"
                | "make"
                | "r"
                | "perl"
        )
    )
}

/// Tokenize a single line of code.
pub fn tokenize_line(line: &str, lang: Option<&str>) -> Vec<Span> {
    let kw = keywords_set();
    let hash_comments = hash_comment_lang(lang);
    let chars: Vec<char> = line.chars().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut i = 0;

    let push = |spans: &mut Vec<Span>, text: String, kind: TokenKind| {
        if !text.is_empty() {
            spans.push(Span { text, kind });
        }
    };

    while i < chars.len() {
        let ch = chars[i];

        // Line comments.
        let is_slash_comment = ch == '/' && i + 1 < chars.len() && chars[i + 1] == '/';
        let is_hash_comment = hash_comments && ch == '#';
        if is_slash_comment || is_hash_comment {
            let rest: String = chars[i..].iter().collect();
            push(&mut spans, rest, TokenKind::Comment);
            break;
        }

        // Strings.
        if ch == '"' || ch == '\'' {
            let quote = ch;
            let mut j = i + 1;
            let mut escaped = false;
            while j < chars.len() {
                let cj = chars[j];
                if escaped {
                    escaped = false;
                } else if cj == '\\' {
                    escaped = true;
                } else if cj == quote {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let text: String = chars[i..j.min(chars.len())].iter().collect();
            push(&mut spans, text, TokenKind::Str);
            i = j;
            continue;
        }

        // Whitespace.
        if ch.is_whitespace() {
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            push(&mut spans, chars[i..j].iter().collect(), TokenKind::Plain);
            i = j;
            continue;
        }

        // Numbers.
        if ch.is_ascii_digit() {
            let mut j = i;
            while j < chars.len()
                && (chars[j].is_ascii_alphanumeric() || chars[j] == '.' || chars[j] == '_')
            {
                j += 1;
            }
            push(&mut spans, chars[i..j].iter().collect(), TokenKind::Number);
            i = j;
            continue;
        }

        // Identifiers / keywords.
        if ch.is_alphabetic() || ch == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let kind = if kw.contains(word.as_str()) {
                TokenKind::Keyword
            } else {
                TokenKind::Ident
            };
            push(&mut spans, word, kind);
            i = j;
            continue;
        }

        // Punctuation / operators (single char).
        push(&mut spans, ch.to_string(), TokenKind::Punct);
        i += 1;
    }

    spans
}

/// Tokenize a whole code block into lines of spans.
pub fn highlight(code: &str, lang: Option<&str>) -> Vec<Vec<Span>> {
    code.split('\n')
        .map(|line| tokenize_line(line, lang))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(spans: &[Span]) -> Vec<TokenKind> {
        spans.iter().map(|s| s.kind).collect()
    }

    #[test]
    fn highlights_keyword_ident_punct() {
        let spans = tokenize_line("let x = 5;", Some("rust"));
        // "let" kw, " " plain, "x" ident, " " plain, "=" punct, " " plain, "5" num, ";" punct
        assert_eq!(
            spans[0],
            Span {
                text: "let".into(),
                kind: TokenKind::Keyword
            }
        );
        assert!(spans
            .iter()
            .any(|s| s.kind == TokenKind::Number && s.text == "5"));
        assert!(spans
            .iter()
            .any(|s| s.kind == TokenKind::Ident && s.text == "x"));
        assert!(kinds(&spans).contains(&TokenKind::Punct));
    }

    #[test]
    fn highlights_string_literal() {
        let spans = tokenize_line(r#"print("hello world")"#, Some("python"));
        assert!(spans
            .iter()
            .any(|s| s.kind == TokenKind::Str && s.text == "\"hello world\""));
    }

    #[test]
    fn slash_comment_consumes_rest_of_line() {
        let spans = tokenize_line("let x = 1; // trailing", Some("rust"));
        let last = spans.last().unwrap();
        assert_eq!(last.kind, TokenKind::Comment);
        assert!(last.text.contains("trailing"));
    }

    #[test]
    fn hash_comment_only_for_hash_languages() {
        let py = tokenize_line("x = 1  # note", Some("python"));
        assert_eq!(py.last().unwrap().kind, TokenKind::Comment);
        // In rust, '#' is punctuation, not a comment.
        let rs = tokenize_line("#[derive(Debug)]", Some("rust"));
        assert_eq!(rs[0].kind, TokenKind::Punct);
    }

    #[test]
    fn highlight_splits_lines() {
        let lines = highlight("fn a() {}\nfn b() {}", Some("rust"));
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0][0],
            Span {
                text: "fn".into(),
                kind: TokenKind::Keyword
            }
        );
    }
}
