//! Pure parsing of a model's text response into a deduped label set, plus
//! optional validation against an allowed set. No I/O — unit-testable.

// Functions are pub and will be used by later tasks; suppress dead_code for now.
#![allow(dead_code)]

/// Parse a model's raw `content` into an ordered, deduped, lowercased label
/// set. Tolerates a JSON array, or a comma/newline separated list. Empty
/// input yields `["unknown"]`.
pub fn parse_labels(content: &str) -> Vec<String> {
    let trimmed = content.trim();
    // Try JSON array of strings first.
    let raw: Vec<String> = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<String>>(trimmed).unwrap_or_default()
    } else {
        trimmed
            .split(['\n', ','])
            .map(|s| s.trim().to_string())
            .collect()
    };
    let mut out: Vec<String> = Vec::new();
    for label in raw {
        let norm = label.trim().to_lowercase();
        if norm.is_empty() || out.contains(&norm) {
            continue;
        }
        out.push(norm);
    }
    if out.is_empty() {
        out.push("unknown".to_string());
    }
    out
}

/// Keep only labels present in `allowed` (case-insensitive). If none remain,
/// return `["unknown"]`. If `allowed` is empty, return `labels` unchanged.
pub fn validate_labels(labels: Vec<String>, allowed: &[String]) -> Vec<String> {
    if allowed.is_empty() {
        return labels;
    }
    let allow_lower: Vec<String> = allowed.iter().map(|a| a.to_lowercase()).collect();
    let kept: Vec<String> = labels
        .into_iter()
        .filter(|l| allow_lower.contains(&l.to_lowercase()))
        .collect();
    if kept.is_empty() {
        vec!["unknown".to_string()]
    } else {
        kept
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_comma_list() {
        assert_eq!(parse_labels("receipt, ticket"), v(&["receipt", "ticket"]));
    }
    #[test]
    fn parses_newlines_and_trims_and_lowercases() {
        assert_eq!(
            parse_labels("Receipt\n  TICKET \n"),
            v(&["receipt", "ticket"])
        );
    }
    #[test]
    fn parses_json_array() {
        assert_eq!(
            parse_labels("[\"receipt\", \"ticket\"]"),
            v(&["receipt", "ticket"])
        );
    }
    #[test]
    fn dedups_preserving_order() {
        assert_eq!(
            parse_labels("receipt, ticket, receipt"),
            v(&["receipt", "ticket"])
        );
    }
    #[test]
    fn empty_becomes_unknown() {
        assert_eq!(parse_labels("   "), v(&["unknown"]));
    }
    #[test]
    fn validate_keeps_in_set_only() {
        let allowed = v(&["invoice", "receipt", "ticket"]);
        assert_eq!(
            validate_labels(v(&["receipt", "spam"]), &allowed),
            v(&["receipt"])
        );
    }
    #[test]
    fn validate_none_in_set_is_unknown() {
        let allowed = v(&["invoice"]);
        assert_eq!(validate_labels(v(&["spam"]), &allowed), v(&["unknown"]));
    }
    #[test]
    fn validate_empty_allowed_is_passthrough() {
        assert_eq!(validate_labels(v(&["x", "y"]), &[]), v(&["x", "y"]));
    }
}
