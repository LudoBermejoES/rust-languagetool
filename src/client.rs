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

/// Convert a raw LT match to our typed struct.
///
/// LT's HTTP API returns `offset` and `length` as **Unicode character** counts
/// (confirmed from the `/v2/check` JSON schema and empirical testing with multi-byte
/// text). They are NOT byte offsets. No conversion is needed; we clamp to the
/// actual char count to guard against edge cases.
fn convert_match(m: LtMatch, text: &str) -> GrammarMatch {
    let total_chars = text.chars().count();
    let char_offset = m.offset.min(total_chars);
    let char_length = m.length.min(total_chars.saturating_sub(char_offset));

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

#[cfg(test)]
fn make_lt_match(offset: usize, length: usize, replacement: &str) -> LtMatch {
    LtMatch {
        message: String::new(),
        short_message: String::new(),
        replacements: vec![LtReplacement { value: replacement.to_string() }],
        offset,
        length,
        rule: LtRule {
            id: "TEST".to_string(),
            issue_type: "grammar".to_string(),
            category: LtCategory { id: "TEST".to_string(), name: "Test".to_string() },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_ascii_passthrough() {
        // ASCII-only: char offset == byte offset, both should be identity.
        let text = "She go to school";
        let m = make_lt_match(4, 2, "goes");
        let gm = convert_match(m, text);
        let snippet: String = text.chars().skip(gm.offset).take(gm.length).collect();
        assert_eq!(snippet, "go");
        assert_eq!(gm.offset, 4);
        assert_eq!(gm.length, 2);
    }

    #[test]
    fn offset_multibyte_año() {
        // "Ella tiene veinte año" — LT returns offset=18, length=3 (char units).
        // 'ñ' is 2 bytes; char count of "año" is 3, not 2.
        let text = "Ella tiene veinte año y estudia";
        let m = make_lt_match(18, 3, "años");
        let gm = convert_match(m, text);
        let snippet: String = text.chars().skip(gm.offset).take(gm.length).collect();
        assert_eq!(snippet, "año", "must select the full word including ñ");
        assert_eq!(gm.offset, 18);
        assert_eq!(gm.length, 3);
    }

    #[test]
    fn offset_after_multibyte() {
        // Error word after multi-byte: offsets must still be correct.
        // "Los niños son malo" — "malo" should be "malos" (agreement).
        // "niños" = n(1)+i(1)+ñ(2 bytes/1 char)+o(1)+s(1) → 5 chars, 6 bytes.
        // "Los niños son " = 3+1+5+1+3+1 = 14 chars.
        // If LT gives offset=14, length=4 for "malo":
        let text = "Los niños son malo";
        let m = make_lt_match(14, 4, "malos");
        let gm = convert_match(m, text);
        let snippet: String = text.chars().skip(gm.offset).take(gm.length).collect();
        assert_eq!(snippet, "malo");
    }

    #[test]
    fn offset_clamped_oob() {
        let text = "abc";
        let m = make_lt_match(999, 5, "x");
        let gm = convert_match(m, text);
        assert_eq!(gm.offset, 3); // clamped to text length
        assert_eq!(gm.length, 0);
    }
}
