//! Ref glob matching (spec section 5.3).
//!
//! `*` matches any characters except `/`; `**` matches any characters
//! including `/`; everything else matches literally. The match covers the whole
//! ref name.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Literal(char),
    Star,
    DoubleStar,
}

fn tokenize(pattern: &str) -> Vec<Token> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut tokens = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                tokens.push(Token::DoubleStar);
                i += 2;
            }
            '*' => {
                tokens.push(Token::Star);
                i += 1;
            }
            other => {
                tokens.push(Token::Literal(other));
                i += 1;
            }
        }
    }
    tokens
}

/// True when `pattern` matches the whole of `name`.
#[must_use]
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let tokens = tokenize(pattern);
    let name: Vec<char> = name.chars().collect();
    let mut dp = vec![false; name.len() + 1];
    dp[0] = true;
    for token in tokens {
        let mut next = vec![false; name.len() + 1];
        match token {
            Token::Literal(expected) => {
                for j in 0..name.len() {
                    if dp[j] && name[j] == expected {
                        next[j + 1] = true;
                    }
                }
            }
            Token::Star => {
                next[0] = dp[0];
                for j in 1..=name.len() {
                    next[j] = dp[j] || (next[j - 1] && name[j - 1] != '/');
                }
            }
            Token::DoubleStar => {
                next[0] = dp[0];
                for j in 1..=name.len() {
                    next[j] = dp[j] || next[j - 1];
                }
            }
        }
        dp = next;
    }
    dp[name.len()]
}

/// True when any glob in `patterns` matches `name`.
#[must_use]
pub fn any_glob_matches(patterns: &[String], name: &str) -> bool {
    patterns.iter().any(|pattern| glob_match(pattern, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_does_not_cross_slash() {
        assert!(glob_match("refs/heads/*", "refs/heads/main"));
        assert!(!glob_match("refs/heads/*", "refs/heads/feature/x"));
        assert!(glob_match("refs/heads/feature/*", "refs/heads/feature/x"));
    }

    #[test]
    fn double_star_crosses_slash() {
        assert!(glob_match("refs/heads/**", "refs/heads/main"));
        assert!(glob_match("refs/heads/**", "refs/heads/feature/x"));
        assert!(glob_match("**", "anything/at/all"));
        assert!(glob_match("refs/**/x", "refs/a/b/x"));
    }

    #[test]
    fn literals_and_anchoring() {
        assert!(glob_match("refs/tags/v1", "refs/tags/v1"));
        assert!(!glob_match("refs/tags/v1", "refs/tags/v1/extra"));
        assert!(!glob_match("v1", "refs/tags/v1"));
    }

    #[test]
    fn any_glob() {
        let globs = vec![
            "refs/heads/feature/*".to_string(),
            "refs/tags/*".to_string(),
        ];
        assert!(any_glob_matches(&globs, "refs/heads/feature/x"));
        assert!(any_glob_matches(&globs, "refs/tags/v1"));
        assert!(!any_glob_matches(&globs, "refs/heads/main"));
    }
}
