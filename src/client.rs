use std::time::Duration;

use serde::Deserialize;
use tracing::debug;

use crate::error::{Error, Result};
use crate::state::{EngineState, SharedState};

/// A grammar match returned by LanguageTool, with character-normalized offsets.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GrammarMatch {
    /// Character offset into the original text (0-indexed, char not byte).
    pub offset: usize,
    /// Length in characters.
    pub length: usize,
    pub message: String,
    pub short_message: String,
    pub replacements: Vec<String>,
    pub rule_id: String,
    pub category_id: String,
    pub category_name: String,
    /// e.g. "grammar", "style", "misspelling", "typographical"
    pub issue_type: String,
}

// ---- LT JSON wire types ----

#[derive(Deserialize)]
struct LtResponse {
    matches: Vec<LtMatch>,
}

#[derive(Deserialize)]
struct LtMatch {
    message: String,
    #[serde(rename = "shortMessage")]
    short_message: String,
    replacements: Vec<LtReplacement>,
    offset: usize,
    length: usize,
    rule: LtRule,
}

#[derive(Deserialize)]
struct LtReplacement {
    value: String,
}

#[derive(Deserialize)]
struct LtRule {
    id: String,
    #[serde(rename = "issueType", default)]
    issue_type: String,
    category: LtCategory,
}

#[derive(Deserialize)]
struct LtCategory {
    id: String,
    name: String,
}

// ---- client ----

pub(crate) struct LtClient {
    port: u16,
    inner: reqwest::Client,
}

impl LtClient {
    pub fn new(port: u16) -> Self {
        let inner = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self { port, inner }
    }

    /// Check text, verifying the engine is ready first.
    pub async fn check(
        &self,
        text: &str,
        language: &str,
        disabled_rules: &[String],
        disabled_categories: &[String],
        state: &SharedState,
    ) -> Result<Vec<GrammarMatch>> {
        if state.get() != EngineState::Ready {
            return Err(Error::NotReady { state: state.get().to_string() });
        }

        let url = format!("http://127.0.0.1:{}/v2/check", self.port);
        let mut params = vec![
            ("text", text.to_string()),
            ("language", language.to_string()),
        ];
        if !disabled_rules.is_empty() {
            params.push(("disabledRules", disabled_rules.join(",")));
        }
        if !disabled_categories.is_empty() {
            params.push(("disabledCategories", disabled_categories.join(",")));
        }

        let resp = self.inner.post(&url).form(&params).send().await?;
        let body = resp.error_for_status()?.text().await?;
        debug!("LT /v2/check response: {} bytes", body.len());

        let lt: LtResponse = serde_json::from_str(&body)?;
        let matches = lt
            .matches
            .into_iter()
            .map(|m| convert_match(m, text))
            .collect();
        Ok(matches)
    }
}

/// Convert a raw LT match to our typed struct, normalizing byte offsets → char offsets.
fn convert_match(m: LtMatch, text: &str) -> GrammarMatch {
    // LT's `offset` is a UTF-16 code-unit offset, but in practice for typical prose
    // (no surrogate pairs) it equals the byte offset in a UTF-8 string.
    // We convert from byte offset to char index to be safe with multi-byte characters.
    let char_offset = byte_offset_to_char(text, m.offset);
    // `length` from LT is also in code units; we compute by taking the substring.
    let char_length = {
        let byte_end = m.offset + m.length;
        let char_end = byte_offset_to_char(text, byte_end);
        char_end.saturating_sub(char_offset)
    };

    GrammarMatch {
        offset: char_offset,
        length: char_length,
        message: m.message,
        short_message: m.short_message,
        replacements: m.replacements.into_iter().map(|r| r.value).collect(),
        rule_id: m.rule.id,
        category_id: m.rule.category.id,
        category_name: m.rule.category.name,
        issue_type: m.rule.issue_type,
    }
}

/// Convert a byte offset in `text` to a character index.
/// Clamps to `text.chars().count()` if out of range.
fn byte_offset_to_char(text: &str, byte_offset: usize) -> usize {
    if byte_offset == 0 {
        return 0;
    }
    let clamped = byte_offset.min(text.len());
    // Count chars up to the clamped byte position
    text[..clamped].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_offset_ascii() {
        let text = "hello world";
        assert_eq!(byte_offset_to_char(text, 6), 6);
    }

    #[test]
    fn byte_offset_multibyte_niños() {
        // "niños" — ñ is 2 bytes in UTF-8 (0xC3 0xB1)
        let text = "niños camina mal";
        // "niños " = n(1)+i(1)+ñ(2)+o(1)+s(1)+space(1) = 7 bytes, 6 chars
        // byte offset 7 = start of "camina"
        assert_eq!(byte_offset_to_char(text, 7), 6);
    }

    #[test]
    fn byte_offset_zero() {
        assert_eq!(byte_offset_to_char("hola", 0), 0);
    }

    #[test]
    fn byte_offset_clamped() {
        let text = "abc";
        assert_eq!(byte_offset_to_char(text, 999), 3);
    }
}
