use regex::Regex;
use std::sync::OnceLock;

fn key_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b[A-Z][A-Z0-9]+-\d+\b").unwrap())
}

/// Extract the first Jira-style ticket key from a string, if any.
pub fn extract_ticket_key(text: &str) -> Option<String> {
    key_re().find(text).map(|m| m.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_key_in_title() {
        assert_eq!(
            extract_ticket_key("JIRA-1234 - Login bug"),
            Some("JIRA-1234".to_string())
        );
        assert_eq!(
            extract_ticket_key("feature/ABC-9 work"),
            Some("ABC-9".to_string())
        );
    }
    #[test]
    fn returns_none_when_no_key() {
        assert_eq!(extract_ticket_key("Slack | general"), None);
    }
    #[test]
    fn picks_first_key_when_multiple() {
        assert_eq!(extract_ticket_key("AB-1 vs CD-2"), Some("AB-1".to_string()));
    }
}
