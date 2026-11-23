//! Recipe params: a bare name, or a typed entry with constraints.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

const PARAM_FIELDS: &[&str] = &[
    "name",
    "type",
    "required",
    "default",
    "enum",
    "pattern",
    "description",
];

/// The value type of a [`Param`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ParamType {
    /// Any text.
    #[default]
    String,
    /// A JSON number.
    Number,
    /// A whole number.
    Integer,
    /// `true` or `false`.
    Bool,
}

impl ParamType {
    /// The name used in recipe files.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Integer => "integer",
            Self::Bool => "bool",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "string" => Some(Self::String),
            "number" => Some(Self::Number),
            "integer" => Some(Self::Integer),
            "bool" | "boolean" => Some(Self::Bool),
            _ => None,
        }
    }

    fn accepts(self, value: &str) -> bool {
        match self {
            Self::String => true,
            Self::Number => value.parse::<serde_json::Number>().is_ok(),
            Self::Integer => value.parse::<i64>().is_ok() || value.parse::<u64>().is_ok(),
            Self::Bool => value.parse::<bool>().is_ok(),
        }
    }

    /// Whether two accepted values are the same value of this type.
    /// Numbers compare as numbers (`1` is `1.0` is `1e0`), as JSON Schema
    /// compares an `enum`; whole numbers compare exactly, at any size.
    #[expect(
        clippy::float_cmp,
        reason = "enum membership is exact equality of the two numbers, not closeness"
    )]
    fn same(self, left: &str, right: &str) -> bool {
        if left == right {
            return true;
        }
        match self {
            Self::String | Self::Bool => false,
            Self::Number | Self::Integer => match (left.parse::<i128>(), right.parse::<i128>()) {
                (Ok(left), Ok(right)) => left == right,
                _ => matches!(
                    (left.parse::<f64>(), right.parse::<f64>()),
                    (Ok(left), Ok(right)) if left == right
                ),
            },
        }
    }
}

/// One declared recipe param.
///
/// In a recipe file a param is either a bare name (`"title"`: a required
/// string) or an object with `name` and any of `type`, `required`,
/// `default`, `enum`, `pattern`, `description`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Param {
    /// `$name` in steps; matches `[A-Za-z_][A-Za-z0-9_]*`.
    pub name: String,
    /// Value type; values are checked against it at bind time.
    pub ty: ParamType,
    /// Explicit `required`. `None` means required unless `default` is set.
    pub required: Option<bool>,
    /// Value used when the caller gives none.
    pub default: Option<String>,
    /// Allowed values; empty means any.
    pub allowed: Vec<String>,
    /// Regular expression the value must match somewhere (anchor with
    /// `^…$` for a full match).
    pub pattern: Option<String>,
    /// Text for people and form builders.
    pub description: Option<String>,
}

impl Param {
    /// A required string param.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ty: ParamType::String,
            required: None,
            default: None,
            allowed: Vec::new(),
            pattern: None,
            description: None,
        }
    }

    /// Whether the caller must supply a value.
    pub fn is_required(&self) -> bool {
        self.required.unwrap_or(self.default.is_none())
    }

    /// Optional with no default: absent means its invoke args are omitted.
    pub fn omits_when_absent(&self) -> bool {
        !self.is_required() && self.default.is_none()
    }

    fn is_plain(&self) -> bool {
        *self == Self::named(self.name.clone())
    }

    /// Check `value` against type, `enum` and `pattern`.
    ///
    /// # Errors
    /// A message naming the param and the violated constraint.
    pub fn check(&self, value: &str) -> Result<(), String> {
        if !self.ty.accepts(value) {
            return Err(format!(
                "param `{}`: `{value}` is not a valid {}",
                self.name,
                self.ty.as_str()
            ));
        }
        if !self.allowed.is_empty()
            && !self
                .allowed
                .iter()
                .any(|allowed| self.ty.same(allowed, value))
        {
            return Err(format!(
                "param `{}`: `{value}` is not one of: {}",
                self.name,
                self.allowed.join(", ")
            ));
        }
        if let Some(pattern) = &self.pattern {
            let regex = regex_lite::Regex::new(pattern)
                .map_err(|e| format!("param `{}`: invalid pattern: {e}", self.name))?;
            if !regex.is_match(value) {
                return Err(format!(
                    "param `{}`: `{value}` does not match pattern `{pattern}`",
                    self.name
                ));
            }
        }
        Ok(())
    }

    /// Problems in the declaration itself (not the name).
    pub(crate) fn lint(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let name = &self.name;
        if self.required == Some(true) && self.default.is_some() {
            problems.push(format!(
                "param `{name}` is required but has a default; drop one"
            ));
        }
        if let Some(pattern) = &self.pattern {
            if self.ty != ParamType::String {
                problems.push(format!(
                    "param `{name}`: pattern is for string params, not {}",
                    self.ty.as_str()
                ));
            }
            if let Err(e) = regex_lite::Regex::new(pattern) {
                problems.push(format!("param `{name}`: invalid pattern: {e}"));
                return problems;
            }
        }
        for allowed in &self.allowed {
            if !self.ty.accepts(allowed) {
                problems.push(format!(
                    "param `{name}`: enum value `{allowed}` is not a valid {}",
                    self.ty.as_str()
                ));
            }
        }
        if let Some(default) = &self.default {
            if let Err(message) = self.check(default) {
                problems.push(format!("default of {message}"));
            }
        }
        problems
    }

    /// JSON Schema property for this param.
    pub(crate) fn json_schema(&self) -> Value {
        let mut out = Map::new();
        let ty = match self.ty {
            ParamType::Bool => "boolean",
            other => other.as_str(),
        };
        out.insert("type".into(), json!(ty));
        if let Some(description) = &self.description {
            out.insert("description".into(), json!(description));
        }
        if !self.allowed.is_empty() {
            out.insert(
                "enum".into(),
                Value::Array(self.allowed.iter().map(|v| typed(self.ty, v)).collect()),
            );
        }
        // JSON Schema's `pattern` is an ECMAScript regular expression, so
        // it is written as that engine reads it. A pattern with no such
        // spelling is left out: it is still checked when the value is bound.
        if let Some(pattern) = self.pattern.as_deref().and_then(ecmascript_pattern) {
            out.insert("pattern".into(), json!(pattern));
        }
        if let Some(default) = &self.default {
            out.insert("default".into(), typed(self.ty, default));
        }
        Value::Object(out)
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let map = match value {
            Value::String(name) => return Ok(Self::named(name.clone())),
            Value::Object(map) => map,
            other => return Err(format!("param must be a name or an object, got {other}")),
        };
        let name = map
            .get("name")
            .and_then(Value::as_str)
            .ok_or("param object needs a string `name`")?;
        if let Some(key) = map.keys().find(|k| !PARAM_FIELDS.contains(&k.as_str())) {
            return Err(format!("param `{name}` has unknown field `{key}`"));
        }
        let mut param = Self::named(name);
        if let Some(ty) = map.get("type") {
            param.ty = ty.as_str().and_then(ParamType::parse).ok_or_else(|| {
                format!("param `{name}`: type must be string, number, integer or bool, got {ty}")
            })?;
        }
        if let Some(required) = map.get("required") {
            param.required = Some(
                required
                    .as_bool()
                    .ok_or_else(|| format!("param `{name}`: required must be true or false"))?,
            );
        }
        if let Some(default) = map.get("default") {
            param.default = Some(scalar_text(default).ok_or_else(|| {
                format!("param `{name}`: default must be a string, number or bool")
            })?);
        }
        if let Some(allowed) = map.get("enum") {
            let items = allowed
                .as_array()
                .filter(|items| !items.is_empty())
                .ok_or_else(|| format!("param `{name}`: enum must be a non-empty array"))?;
            param.allowed = items
                .iter()
                .map(scalar_text)
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    format!("param `{name}`: enum values must be strings, numbers or bools")
                })?;
        }
        for (key, slot) in [
            ("pattern", &mut param.pattern),
            ("description", &mut param.description),
        ] {
            if let Some(text) = map.get(key) {
                *slot = Some(
                    text.as_str()
                        .ok_or_else(|| format!("param `{name}`: {key} must be a string"))?
                        .to_string(),
                );
            }
        }
        Ok(param)
    }
}

/// What a character class has read so far, which decides what a `-`
/// after it means.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClassMember {
    /// Nothing yet: a `-` here is a literal hyphen.
    Start,
    /// A single character: a `-` here starts a range.
    Single,
    /// A set (`\d`, `\s`) or the end of a range: a `-` here is a literal
    /// hyphen only as the class's last member.
    Closed,
    /// The literal hyphen that opened the class.
    Hyphen,
}

/// `pattern` as an ECMAScript engine must be given it to match what this
/// crate's engine (`regex-lite`) matches, or `None` when it has no such
/// spelling. Conservative: syntax not known to be shared has none. The
/// ECMAScript reading meant is the Unicode one (the `u` flag, which JSON
/// Schema validators such as Ajv use): without it `.` and a class count a
/// character outside the Basic Multilingual Plane as two.
///
/// Three things are spelled out, because the two engines read them
/// differently: `.` (here any character but a line feed), and `\s` / `\S`
/// (here the six ASCII whitespace characters; ECMAScript adds the no-break
/// space and the other Unicode spaces).
///
/// No spelling: inline flags (`(?i)`, `(?i:…)`), `(?P<name>…)`, escapes
/// other than the class, control and punctuation ones (`\A`, `\z`,
/// `\x{…}`), `\-` outside a class, `\S` inside one, a `[` inside a class
/// (`[[:alpha:]]`), a class that opens with `]`, and a `-` in a class that
/// is not its first member, its last, or between two single characters
/// (`[--\s]`, `[\d-a]`, `[a-z-9]`).
#[expect(
    clippy::too_many_lines,
    reason = "one scan over the pattern; every arm shares the class state"
)]
fn ecmascript_pattern(pattern: &str) -> Option<String> {
    const SPACES: &str = r"\t\n\x0B\f\r ";
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len());
    // `Some` inside a character class.
    let mut class: Option<ClassMember> = None;
    // A range's `-` was just read; the next single character ends it.
    let mut range_open = false;
    // A single character was read inside a class.
    let single = |class: &mut Option<ClassMember>, range_open: &mut bool| {
        if class.is_some() {
            *class = Some(if *range_open {
                ClassMember::Closed
            } else {
                ClassMember::Single
            });
            *range_open = false;
        }
    };
    let mut at = 0;
    while at < chars.len() {
        let next = chars.get(at + 1).copied();
        match chars[at] {
            '\\' => {
                let escaped = next?;
                match escaped {
                    's' | 'S' | 'd' | 'D' | 'w' | 'W' => {
                        // A set cannot end a range.
                        if range_open || (escaped == 'S' && class.is_some()) {
                            return None;
                        }
                        match (escaped, class.is_some()) {
                            ('s', true) => out.push_str(SPACES),
                            ('s', false) => {
                                out.push('[');
                                out.push_str(SPACES);
                                out.push(']');
                            }
                            ('S', _) => {
                                out.push_str("[^");
                                out.push_str(SPACES);
                                out.push(']');
                            }
                            _ => {
                                out.push('\\');
                                out.push(escaped);
                            }
                        }
                        if class.is_some() {
                            class = Some(ClassMember::Closed);
                        }
                        at += 2;
                    }
                    'x' => {
                        let hex = chars.get(at + 2..at + 4)?;
                        if !hex.iter().all(char::is_ascii_hexdigit) {
                            return None;
                        }
                        out.push_str(r"\x");
                        out.extend(hex);
                        single(&mut class, &mut range_open);
                        at += 4;
                    }
                    _ => {
                        let shared = match escaped {
                            'n' | 'r' | 't' | 'f' | 'v' => true,
                            'b' | 'B' => class.is_none(),
                            '-' => class.is_some(),
                            other => r".^$*+?()[]{}|/\".contains(other),
                        };
                        if !shared {
                            return None;
                        }
                        out.push('\\');
                        out.push(escaped);
                        single(&mut class, &mut range_open);
                        at += 2;
                    }
                }
                continue;
            }
            '[' if class.is_some() => return None,
            '[' => {
                // `[]…]` and `[^]…]` hold a `]` here; ECMAScript reads an
                // empty class.
                let negated = next == Some('^');
                let first = chars.get(at + if negated { 2 } else { 1 }).copied();
                if first == Some(']') {
                    return None;
                }
                class = Some(ClassMember::Start);
                out.push('[');
                if negated {
                    out.push('^');
                }
                at += if negated { 2 } else { 1 };
                continue;
            }
            ']' if class.is_some() => class = None,
            '-' if class.is_some() => match class {
                Some(ClassMember::Start) => class = Some(ClassMember::Hyphen),
                Some(ClassMember::Hyphen) => return None,
                Some(_) if next == Some(']') => {}
                Some(ClassMember::Single) if next != Some('-') => range_open = true,
                _ => return None,
            },
            '.' if class.is_none() => {
                out.push_str(r"[^\n]");
                at += 1;
                continue;
            }
            '(' if class.is_none() && next == Some('?') => {
                let named = chars.get(at + 2) == Some(&'<')
                    && chars
                        .get(at + 3)
                        .is_some_and(|c| c.is_ascii_alphabetic() || *c == '_');
                if chars.get(at + 2) != Some(&':') && !named {
                    return None;
                }
            }
            _ => single(&mut class, &mut range_open),
        }
        out.push(chars[at]);
        at += 1;
    }
    Some(out)
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn typed(ty: ParamType, text: &str) -> Value {
    match ty {
        ParamType::String => None,
        ParamType::Number | ParamType::Integer => {
            text.parse::<serde_json::Number>().ok().map(Value::Number)
        }
        ParamType::Bool => text.parse::<bool>().ok().map(Value::Bool),
    }
    .unwrap_or_else(|| json!(text))
}

/// [`typed`], but only when the typed value reads back as the same text:
/// a recipe is written back as it was read, so `1e2` must not become `100.0`.
fn exact(ty: ParamType, text: &str) -> Value {
    let value = typed(ty, text);
    if scalar_text(&value).as_deref() == Some(text) {
        value
    } else {
        json!(text)
    }
}

impl Serialize for Param {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.is_plain() {
            return serializer.serialize_str(&self.name);
        }
        let mut out = Map::new();
        out.insert("name".into(), json!(self.name));
        if self.ty != ParamType::String {
            out.insert("type".into(), json!(self.ty.as_str()));
        }
        if let Some(required) = self.required {
            out.insert("required".into(), json!(required));
        }
        if let Some(default) = &self.default {
            out.insert("default".into(), exact(self.ty, default));
        }
        if !self.allowed.is_empty() {
            out.insert(
                "enum".into(),
                Value::Array(self.allowed.iter().map(|v| exact(self.ty, v)).collect()),
            );
        }
        if let Some(pattern) = &self.pattern {
            out.insert("pattern".into(), json!(pattern));
        }
        if let Some(description) = &self.description {
            out.insert("description".into(), json!(description));
        }
        Value::Object(out).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Param {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::from_value(&value).map_err(serde::de::Error::custom)
    }
}
