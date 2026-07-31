//! Email address detection.

use super::PatternMatch;
use crate::types::redaction::PiiCategory;
use once_cell::sync::Lazy;
use regex::Regex;

static RE_EMAIL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b").expect("email regex compiles"));

/// Find all email address spans in `text`.
pub fn find_all(text: &str) -> Vec<PatternMatch> {
    RE_EMAIL
        .find_iter(text)
        .map(|m| PatternMatch {
            start: m.start(),
            end: m.end(),
            category: PiiCategory::Email,
            text: m.as_str().to_string(),
        })
        .collect()
}
