//! What a run selects — `-k`, `-m`, `--strict-markers` — as a value that travels with a request
//! (TID-90). Platform-neutral: the daemon and the CLI name it on every target, the warm pool that
//! applies it exists on Unix only.

/// What a run selects, applied by each worker forked off a warm image before it serves (TID-90):
/// the shim reads `-k` / `-m` / `--strict-markers` from its environment at start-up, and a
/// persistent image was started without this run's. `None` fields keep what the image has
/// (the project's own `addopts`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Selection {
    /// Absent (or `null`, which the shim reads the same way) keeps the image's own `-k`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    /// Absent keeps the image's own `-m` — the project's `addopts`, for a run that gave none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    pub strict_markers: bool,
}

impl Selection {
    /// Whether this selection narrows anything at all.
    pub fn is_empty(&self) -> bool {
        self.keyword.is_none() && self.marker.is_none() && !self.strict_markers
    }
}

/// pytest's `-k` expression, parsed (TID-102): identifiers, `and`, `or`, `not` and parentheses,
/// the grammar the shim's `_parse_selection_expr` reads — ported token for token, so the daemon
/// and the workers agree on what an expression means and on which expressions mean nothing (the
/// shim reports an unreadable filter once and selects everything; `parse` answers `None` for the
/// same inputs, and the daemon then decides nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeywordExpr {
    Ident(String),
    Not(Box<KeywordExpr>),
    And(Vec<KeywordExpr>),
    Or(Vec<KeywordExpr>),
}

impl KeywordExpr {
    /// The tree for `expr`, or `None` when it is outside the grammar.
    pub fn parse(expr: &str) -> Option<Self> {
        // The shim's `_SELECTION_IDENT`: `[\w.:+\-\[\]\\/]+`, `\w` being Unicode word characters.
        fn ident_char(c: char) -> bool {
            c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '+' | '-' | '[' | ']' | '\\' | '/')
        }
        let mut tokens: Vec<String> = Vec::new();
        let mut rest = expr;
        while let Some(c) = rest.chars().next() {
            if c.is_whitespace() {
                rest = &rest[c.len_utf8()..];
            } else if c == '(' || c == ')' {
                tokens.push(c.to_string());
                rest = &rest[1..];
            } else {
                let end = rest.find(|ch: char| !ident_char(ch)).unwrap_or(rest.len());
                if end == 0 {
                    return None; // an unexpected character
                }
                tokens.push(rest[..end].to_string());
                rest = &rest[end..];
            }
        }
        let mut pos = 0;
        let tree = Self::parse_or(&tokens, &mut pos)?;
        (pos == tokens.len()).then_some(tree)
    }

    fn parse_or(tokens: &[String], pos: &mut usize) -> Option<Self> {
        let mut items = vec![Self::parse_and(tokens, pos)?];
        while tokens.get(*pos).map(String::as_str) == Some("or") {
            *pos += 1;
            items.push(Self::parse_and(tokens, pos)?);
        }
        Some(if items.len() == 1 {
            items.pop().expect("one item")
        } else {
            Self::Or(items)
        })
    }

    fn parse_and(tokens: &[String], pos: &mut usize) -> Option<Self> {
        let mut items = vec![Self::parse_not(tokens, pos)?];
        while tokens.get(*pos).map(String::as_str) == Some("and") {
            *pos += 1;
            items.push(Self::parse_not(tokens, pos)?);
        }
        Some(if items.len() == 1 {
            items.pop().expect("one item")
        } else {
            Self::And(items)
        })
    }

    fn parse_not(tokens: &[String], pos: &mut usize) -> Option<Self> {
        let token = tokens.get(*pos)?.as_str();
        if token == "not" {
            *pos += 1;
            return Some(Self::Not(Box::new(Self::parse_not(tokens, pos)?)));
        }
        if token == "(" {
            *pos += 1;
            let inner = Self::parse_or(tokens, pos)?;
            if tokens.get(*pos).map(String::as_str) != Some(")") {
                return None;
            }
            *pos += 1;
            return Some(inner);
        }
        if matches!(token, ")" | "and" | "or") {
            return None;
        }
        *pos += 1;
        Some(Self::Ident(token.to_string()))
    }

    /// pytest's match rule, on a complete id: an identifier holds when it is a case-insensitive
    /// substring of any of the node's names.
    pub fn matches(&self, names: &[String]) -> bool {
        match self {
            Self::Ident(ident) => names.iter().any(|n| contains_ignore_case(n, ident)),
            Self::Not(inner) => !inner.matches(names),
            Self::And(items) => items.iter().all(|t| t.matches(names)),
            Self::Or(items) => items.iter().any(|t| t.matches(names)),
        }
    }
}

/// `haystack` contains `needle`, case-insensitively — the daemon runs this for every identifier
/// against every name of every record (TID-102), so the ASCII case takes no allocation.
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if haystack.is_ascii() && needle.is_ascii() {
        let (h, n) = (haystack.as_bytes(), needle.as_bytes());
        if n.is_empty() {
            return true;
        }
        return h.len() >= n.len()
            && h.windows(n.len())
                .any(|w| w.iter().zip(n).all(|(a, b)| a.eq_ignore_ascii_case(b)));
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

#[cfg(test)]
mod keyword_expr_tests {
    use super::KeywordExpr;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_shims_grammar_parses_and_matches_the_same_way() {
        let node = names(&[
            "tests",
            "unit",
            "test_alpha.py",
            "TestGroup",
            "test_three",
            "slow",
        ]);
        let case = names(&["test_beta.py", "test_cases[1-a]"]);
        for (expr, node_hit, case_hit) in [
            ("one", false, false),
            ("three", true, false),
            ("TestGroup", true, false),
            ("testgroup", true, false), // case-insensitive
            ("cases and not 2-b", false, true),
            ("alpha or 2-b", true, false),
            ("not three", false, true),
            ("slow", true, false),            // a mark name is a keyword
            ("test_cases[1-a]", false, true), // the whole case id, brackets and all
            ("unit", true, false),            // a directory below the rootdir (TID-100)
            ("(alpha or beta) and not group", false, true),
            ("tests/unit", false, false), // `/` is an identifier character; no name holds it
        ] {
            let tree = KeywordExpr::parse(expr).unwrap_or_else(|| panic!("{expr:?} parses"));
            assert_eq!(tree.matches(&node), node_hit, "{expr:?} on the node");
            assert_eq!(tree.matches(&case), case_hit, "{expr:?} on the case");
        }
    }

    #[test]
    fn what_the_shim_rejects_is_rejected_here() {
        for expr in [
            "",
            "and",
            "one and",
            "not",
            "(one",
            "one)",
            "one or or two",
            "a b",
            "x $ y",
        ] {
            assert!(
                KeywordExpr::parse(expr).is_none(),
                "{expr:?} is outside the grammar"
            );
        }
    }

    #[test]
    fn a_b_is_two_identifiers_and_therefore_not_an_expression() {
        // The shim tokenises `a b` as two identifiers with nothing between them and raises;
        // pytest does the same. A daemon that read it as `a and b` would deselect what the
        // workers, ignoring the filter, would run.
        assert!(KeywordExpr::parse("a b").is_none());
    }
}
