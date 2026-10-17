//! TextResolver: TYPE / SET_VALUE payload generation is *not* Instinct.
//!
//! Port of `aui-policy/src/text.rs` (`DeterministicTextResolver` +
//! `TextContext`/`TextResolution`/`TextError`). Instinct picks *which
//! field*; this supplies *what to type* — the quoted literal from the
//! clause, exact and unaltered, abstaining when none or more than one
//! candidate applies. (`ground_select` is not ported: the action space
//! has no select kind.)

use std::fmt;

use crate::goal::UiGoal;

/// Context for resolving typed text. Fingerprint so stale resolves are
/// dropped before dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextContext {
    pub goal: UiGoal,
    pub field_label: String,
    pub field_role: String,
    /// Payloads successfully typed earlier in this run; not
    /// fingerprinted.
    pub typed: Vec<String>,
    /// Observation / action-space fingerprint bits the resolve was
    /// based on.
    pub context_fingerprint: u64,
}

impl TextContext {
    pub fn fingerprint(&self) -> u64 {
        let mut hash = 0xcbf29ce484222325u64;
        for b in self.goal.as_str().as_bytes() {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        for b in self.field_label.as_bytes() {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= self.context_fingerprint;
        hash = hash.wrapping_mul(0x100000001b3);
        hash
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextResolution {
    pub text: String,
    pub context_fingerprint: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextError {
    Ambiguous,
    Missing,
    Invalid(String),
    /// The resolver declined to produce a value (e.g. every literal was
    /// already typed). The caller abstains; nothing is typed.
    Abstain(String),
}

impl fmt::Display for TextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ambiguous => f.write_str("text resolution ambiguous"),
            Self::Missing => f.write_str("no text value found in goal/context"),
            Self::Invalid(msg) => write!(f, "invalid text: {msg}"),
            Self::Abstain(msg) => write!(f, "text resolver abstained: {msg}"),
        }
    }
}

impl std::error::Error for TextError {}

pub trait TextResolver {
    fn resolve(&mut self, context: &TextContext) -> Result<TextResolution, TextError>;
}

/// Deterministically extracts a field's text payload from the goal.
/// No model.
#[derive(Clone, Debug, Default)]
pub struct DeterministicTextResolver;

impl TextResolver for DeterministicTextResolver {
    fn resolve(&mut self, context: &TextContext) -> Result<TextResolution, TextError> {
        let goal = context.goal.as_str();
        let literals = quoted_literals(goal);
        if !literals.is_empty() {
            let untyped: Vec<_> = literals
                .into_iter()
                .filter(|(_, literal)| !context.typed.contains(literal))
                .collect();
            if untyped.is_empty() {
                return Err(TextError::Abstain(
                    "no quoted literal left for this field".into(),
                ));
            }

            let label_words = ascii_words(&context.field_label);
            let selected = untyped
                .iter()
                .find(|(opening_quote, _)| {
                    let before = &goal[..*opening_quote];
                    let start = before
                        .char_indices()
                        .rev()
                        .nth(31)
                        .map_or(0, |(index, _)| index);
                    ascii_words(&before[start..])
                        .iter()
                        .rev()
                        .take(3)
                        .any(|word| label_words.contains(word))
                })
                .unwrap_or(&untyped[0]);
            return Ok(TextResolution {
                text: selected.1.clone(),
                context_fingerprint: context.fingerprint(),
            });
        }

        if let Some(quoted) = first_quoted(goal) {
            if quoted.is_empty() {
                return Err(TextError::Missing);
            }
            return Ok(TextResolution {
                text: quoted,
                context_fingerprint: context.fingerprint(),
            });
        }
        // Pattern: type <value> into <field>
        if let Some(v) = type_into_pattern(goal, &context.field_label) {
            return Ok(TextResolution {
                text: v,
                context_fingerprint: context.fingerprint(),
            });
        }
        // Pattern: fill <field> with <value>
        if let Some(v) = fill_with_pattern(goal, &context.field_label) {
            return Ok(TextResolution {
                text: v,
                context_fingerprint: context.fingerprint(),
            });
        }
        Err(TextError::Missing)
    }
}

fn quoted_literals(goal: &str) -> Vec<(usize, String)> {
    let mut literals = Vec::new();
    let mut cursor = 0;
    while cursor < goal.len() {
        let Some((offset, opening)) = goal[cursor..]
            .char_indices()
            .find(|(_, character)| matches!(*character, '"' | '“'))
        else {
            break;
        };
        let opening_quote = cursor + offset;
        let (closing, content_start) = match opening {
            '"' => ('"', opening_quote + 1),
            '“' => ('”', opening_quote + opening.len_utf8()),
            _ => unreachable!(),
        };
        let Some((offset, _)) = goal[content_start..]
            .char_indices()
            .find(|(_, character)| *character == closing)
        else {
            cursor = content_start;
            continue;
        };
        let content_end = content_start + offset;
        if content_end == content_start {
            cursor = content_end + closing.len_utf8();
            continue;
        }
        literals.push((opening_quote, goal[content_start..content_end].to_owned()));
        cursor = content_end + closing.len_utf8();
    }
    literals
}

fn ascii_words(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn first_quoted(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            if i < bytes.len() {
                return Some(s[start..i].to_owned());
            }
            return None;
        }
        i += 1;
    }
    None
}

fn type_into_pattern(goal: &str, field: &str) -> Option<String> {
    let lower = goal.to_ascii_lowercase();
    let field_l = field.to_ascii_lowercase();
    // "type VALUE into FIELD"
    let type_idx = lower.find("type ")?;
    let into_idx = lower.find(" into ")?;
    // "type into X" has no value between the verb and " into ".
    if into_idx < type_idx + 5 {
        return None;
    }
    let value = goal[type_idx + 5..into_idx].trim();
    let after = &lower[into_idx + 6..];
    if !after.contains(&field_l) && !field_l.is_empty() {
        // Still allow if field label empty / generic.
        if !field.is_empty() {
            return None;
        }
    }
    if value.is_empty() {
        return None;
    }
    Some(value.trim_matches(|c| c == '"' || c == '\'').to_owned())
}

fn fill_with_pattern(goal: &str, field: &str) -> Option<String> {
    let lower = goal.to_ascii_lowercase();
    let field_l = field.to_ascii_lowercase();
    if !field_l.is_empty() && !lower.contains(&field_l) {
        return None;
    }
    let with_idx = lower.find(" with ")?;
    let value = goal[with_idx + 6..].trim();
    if value.is_empty() {
        return None;
    }
    Some(value.trim_matches(|c| c == '"' || c == '\'').to_owned())
}
