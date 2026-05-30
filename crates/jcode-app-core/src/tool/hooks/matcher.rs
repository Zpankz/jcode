//! Tool-name matcher for hooks.

use serde::Deserialize;

/// How a hook entry matches tool names.
///
/// Accepted config forms (in `matcher` string):
/// - exact: `"bash"` matches only `bash`
/// - alternation: `"bash|jsbash|grep"` matches any listed name
/// - wildcard: `"*"` matches every tool
///
/// Matching is performed against the *resolved* tool name.
#[derive(Debug, Clone, Deserialize)]
#[serde(from = "String")]
pub struct HookMatcher {
    raw: String,
    names: Vec<String>,
    wildcard: bool,
}

impl HookMatcher {
    pub fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed == "*" || trimmed.is_empty() {
            return Self {
                raw: raw.to_string(),
                names: Vec::new(),
                wildcard: true,
            };
        }
        let names: Vec<String> = trimmed
            .split('|')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let wildcard = names.iter().any(|n| n == "*");
        Self {
            raw: raw.to_string(),
            names,
            wildcard,
        }
    }

    pub fn matches(&self, tool_name: &str) -> bool {
        self.wildcard || self.names.iter().any(|n| n == tool_name)
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }
}

impl From<String> for HookMatcher {
    fn from(value: String) -> Self {
        HookMatcher::parse(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        let m = HookMatcher::parse("bash");
        assert!(m.matches("bash"));
        assert!(!m.matches("jsbash"));
    }

    #[test]
    fn alternation_match() {
        let m = HookMatcher::parse("bash|jsbash|grep");
        assert!(m.matches("bash"));
        assert!(m.matches("jsbash"));
        assert!(m.matches("grep"));
        assert!(!m.matches("read"));
    }

    #[test]
    fn wildcard_match() {
        let m = HookMatcher::parse("*");
        assert!(m.matches("anything"));
        assert!(m.matches("bash"));
    }

    #[test]
    fn empty_is_wildcard() {
        let m = HookMatcher::parse("   ");
        assert!(m.matches("x"));
    }

    #[test]
    fn whitespace_trimmed_in_alternation() {
        let m = HookMatcher::parse(" bash | jsbash ");
        assert!(m.matches("bash"));
        assert!(m.matches("jsbash"));
    }
}
