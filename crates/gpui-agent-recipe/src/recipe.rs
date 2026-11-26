use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use gpui_agent::protocol::{AssertSpec, DeliveryMode, Op};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{RecipeError, RecipeErrorCode, err};
use crate::param::Param;
use crate::registry::Registry;
use crate::schema::{OpSchema, SchemaKind};

/// Recipe document version (not the wire protocol version).
pub const RECIPE_FORMAT_VERSION: u32 = 1;
/// Cap so one recipe cannot flood the mailbox / connection.
pub const MAX_RECIPE_STEPS: usize = 256;

const RECIPE_FIELDS: &[&str] = &["$schema", "v", "name", "app", "params", "steps"];
const STEP_FIELDS: &[&str] = &["id", "needs", "screenshot", "key_delay_ms", "op"];

/// The longest pause [`RecipeStep::key_delay_ms`] may ask for between two keys.
pub const MAX_KEY_DELAY_MS: u64 = 10_000;

/// JSON recipe: a named DAG (or implicit sequence) of protocol ops.
///
/// JSON is the canonical form; a line-based `.wants` file compiles to
/// the same struct. Parse with [`Recipe::from_json`] /
/// [`parse_recipe`] rather than `serde_json::from_str`: only those
/// reject unknown step fields. In code, start from [`Recipe::new`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Recipe {
    /// Optional JSON Schema reference for editors; ignored by the runner.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<String>,
    /// Format version — must equal [`RECIPE_FORMAT_VERSION`].
    #[serde(default = "recipe_v1")]
    pub v: u32,
    /// Recipe name (file stem for `.wants`).
    pub name: String,
    /// Optional app the recipe targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// `$param`s bound by `--set`: bare names or typed entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<Param>,
    /// Steps in file order.
    pub steps: Vec<RecipeStep>,
}

fn recipe_v1() -> u32 {
    RECIPE_FORMAT_VERSION
}

/// One recipe step: an id, optional `needs` dependencies, and the op.
/// In code, start from [`RecipeStep::new`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RecipeStep {
    /// Step id — the wave-schedule node name.
    pub id: String,
    /// Step ids this step must wait for (`[]` = no prerequisites;
    /// file order applies only when *every* step has empty `needs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    /// After this step, write a PNG when `recipe run --screenshot-dir` is set.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub screenshot: bool,
    /// On a `type` step only: send the text one character at a time, one
    /// `type` request per character, waiting this many milliseconds between
    /// two of them, as a person types. Without it the text goes as one
    /// request. The client paces the keys; the host sees ordinary `type`
    /// requests, so any host that takes `type` takes a paced step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_delay_ms: Option<u64>,
    /// The wire op, flattened into the step JSON.
    #[serde(flatten)]
    pub op: Op,
}

impl RecipeStep {
    /// A step that runs `op`, with no `needs` and no screenshot flag.
    pub fn new(id: impl Into<String>, op: Op) -> Self {
        Self {
            id: id.into(),
            needs: Vec::new(),
            screenshot: false,
            key_delay_ms: None,
            op,
        }
    }
}

impl Recipe {
    /// A recipe of the current format version with these steps, no params
    /// and no app.
    pub fn new(name: impl Into<String>, steps: Vec<RecipeStep>) -> Self {
        Self {
            json_schema: None,
            v: RECIPE_FORMAT_VERSION,
            name: name.into(),
            app: None,
            params: Vec::new(),
            steps,
        }
    }

    /// Parse structured recipe JSON, rejecting keys that are not part of
    /// the recipe format or of the step's op.
    ///
    /// # Errors
    /// [`RecipeErrorCode::Parse`] on malformed JSON, a key written twice
    /// in one object, or a wrongly typed field;
    /// [`RecipeErrorCode::UnknownField`] on an unknown key.
    pub fn from_json(text: &str) -> Result<Self, RecipeError> {
        // A `Value` keeps only the last of two equal keys, so they are
        // refused on the text, before one of them is gone.
        reject_duplicate_keys(text)?;
        let value: Value = serde_json::from_str(text)
            .map_err(|e| err(RecipeErrorCode::Parse, format!("recipe json: {e}")))?;
        Self::from_value(&value)
    }

    /// [`Recipe::from_json`] for an already parsed JSON value. A key the
    /// text wrote twice is already down to one here and cannot be refused:
    /// run [`reject_duplicate_keys`] on the text the value came from.
    ///
    /// # Errors
    /// As [`Recipe::from_json`].
    pub fn from_value(value: &Value) -> Result<Self, RecipeError> {
        if let Some(map) = value.as_object() {
            if let Some(key) = map.keys().find(|k| !RECIPE_FIELDS.contains(&k.as_str())) {
                return Err(err(
                    RecipeErrorCode::UnknownField,
                    format!("recipe json: unknown field `{key}`"),
                ));
            }
        }
        let recipe = Self::deserialize(value)
            .map_err(|e| err(RecipeErrorCode::Parse, format!("recipe json: {e}")))?;
        let raw_steps = value.get("steps").and_then(Value::as_array);
        for (step, raw) in recipe.steps.iter().zip(raw_steps.into_iter().flatten()) {
            // `key_delay_ms` is a step field, not an op's: only `type` reads it.
            if raw.get("key_delay_ms").is_some() && !matches!(step.op, Op::Type { .. }) {
                return Err(err(
                    RecipeErrorCode::UnknownField,
                    format!(
                        "recipe json: step `{}` has unknown field `key_delay_ms` for op `{}` (only `type` is paced)",
                        step.id,
                        op_name(raw)
                    ),
                )
                .at_step(&step.id));
            }
            if let Some(key) = unknown_step_field(raw, step) {
                return Err(err(
                    RecipeErrorCode::UnknownField,
                    format!(
                        "recipe json: step `{}` has unknown field `{key}` for op `{}`",
                        step.id,
                        op_name(raw)
                    ),
                )
                .at_step(&step.id));
            }
        }
        Ok(recipe)
    }

    /// The declared param named `name`.
    pub fn param(&self, name: &str) -> Option<&Param> {
        self.params.iter().find(|param| param.name == name)
    }

    /// Declared param names, in file order.
    pub fn param_names(&self) -> Vec<&str> {
        self.params
            .iter()
            .map(|param| param.name.as_str())
            .collect()
    }

    /// JSON Schema (2020-12) object for this recipe's inputs: one
    /// property per param, with `required` listing the params a caller
    /// must supply.
    pub fn input_schema(&self) -> Value {
        let properties: serde_json::Map<String, Value> = self
            .params
            .iter()
            .map(|param| (param.name.clone(), param.json_schema()))
            .collect();
        let required: Vec<&str> = self
            .params
            .iter()
            .filter(|param| param.is_required())
            .map(|param| param.name.as_str())
            .collect();
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": self.name,
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }

    /// True when any step declares `needs` — disables the implicit
    /// linear chain in favor of the explicit DAG.
    pub fn uses_explicit_needs(&self) -> bool {
        self.steps.iter().any(|step| !step.needs.is_empty())
    }
}

/// Refuse JSON text in which one object writes a key twice.
///
/// [`Recipe::from_json`] runs this on its text. A caller that receives a
/// recipe inside a larger JSON message (an MCP request) and parses that
/// message itself runs it on the message's text: once the recipe is a
/// [`Value`], the earlier key is gone.
///
/// # Errors
/// [`RecipeErrorCode::Parse`], naming the repeated key (or the JSON error).
pub fn reject_duplicate_keys(text: &str) -> Result<(), RecipeError> {
    serde_json::from_str::<NoDuplicateKeys>(text)
        .map(|_| ())
        .map_err(|e| err(RecipeErrorCode::Parse, format!("recipe json: {e}")))
}

/// Reads any JSON and keeps nothing: its only job is to fail on an object
/// that writes one key twice.
struct NoDuplicateKeys;

impl<'de> Deserialize<'de> for NoDuplicateKeys {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Walk;

        impl<'de> serde::de::Visitor<'de> for Walk {
            type Value = NoDuplicateKeys;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                while seq.next_element::<NoDuplicateKeys>()?.is_some() {}
                Ok(NoDuplicateKeys)
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut seen = BTreeSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if seen.contains(&key) {
                        return Err(serde::de::Error::custom(format!("duplicate field `{key}`")));
                    }
                    map.next_value::<NoDuplicateKeys>()?;
                    seen.insert(key);
                }
                Ok(NoDuplicateKeys)
            }
        }

        deserializer.deserialize_any(Walk)
    }
}

fn op_name(raw: &Value) -> &str {
    raw.get("op").and_then(Value::as_str).unwrap_or("?")
}

/// First key of a raw step object that the step's op does not read.
///
/// `Op` is the wire type and stays lenient about unknown keys, so the
/// recipe layer probes each key instead: overwrite it with values no
/// typed field accepts. A key the op reads then fails to parse or
/// changes the parsed step; a key it ignores does neither.
fn unknown_step_field(raw: &Value, step: &RecipeStep) -> Option<String> {
    let map = raw.as_object()?;
    let baseline = serde_json::to_value(step).ok()?;
    let probes = [
        Value::String("\u{1}recipe-probe".into()),
        serde_json::json!({ "\u{1}recipe-probe": true }),
    ];
    map.keys()
        .filter(|key| !STEP_FIELDS.contains(&key.as_str()))
        .find(|key| {
            probes.iter().all(|probe| {
                let mut candidate = map.clone();
                candidate.insert((*key).clone(), probe.clone());
                RecipeStep::deserialize(Value::Object(candidate))
                    .ok()
                    .and_then(|step| serde_json::to_value(step).ok())
                    .is_some_and(|value| value == baseline)
            })
        })
        .cloned()
}

/// Parse a file: `.json` as structured recipe, anything else as `.wants`.
/// Path `-` reads stdin.
///
/// # Errors
/// [`RecipeErrorCode::Io`] on read failure, else as
/// [`parse_recipe_source`].
pub fn parse_recipe(path: &Path) -> Result<Recipe, RecipeError> {
    let text = if path.as_os_str() == "-" {
        std::io::read_to_string(std::io::stdin())
            .map_err(|e| err(RecipeErrorCode::Io, format!("stdin: {e}")))?
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| err(RecipeErrorCode::Io, format!("{}: {e}", path.display())))?
    };
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("recipe")
        .to_string();
    let hint = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    parse_recipe_source(&text, &name, hint == "json")
}

/// Parse recipe text; JSON when `force_json` or the text starts with
/// `{`, `.wants` line format otherwise.
///
/// # Errors
/// As [`Recipe::from_json`] / [`parse_wants`].
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "`force_json` is the caller's parse-mode flag — a bool is the interface"
)]
pub fn parse_recipe_source(
    text: &str,
    name: &str,
    force_json: bool,
) -> Result<Recipe, RecipeError> {
    let trimmed = text.trim();
    if force_json || trimmed.starts_with('{') {
        Recipe::from_json(trimmed)
    } else {
        parse_wants(trimmed, name)
    }
}

/// Line-based `.wants`: one protocol op per line, `#` comments.
///
/// # Errors
/// [`RecipeErrorCode::Parse`] on a malformed line;
/// [`RecipeErrorCode::InvalidRecipe`] on an empty recipe.
pub fn parse_wants(text: &str, name: &str) -> Result<Recipe, RecipeError> {
    let mut steps = Vec::with_capacity(16);
    let mut params = BTreeSet::new();
    for (idx, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at_line = |message: String| {
            err(
                RecipeErrorCode::Parse,
                format!("line {}: {message}", idx + 1),
            )
        };
        let tokens = tokenize(line).map_err(at_line)?;
        if tokens.is_empty() {
            continue;
        }
        let id = format!("s{}", steps.len() + 1);
        let step = parse_wants_step(tokens, id).map_err(at_line)?;
        collect_placeholders_in_op(&step.op, &mut params);
        steps.push(step);
    }
    if steps.is_empty() {
        return Err(err(RecipeErrorCode::InvalidRecipe, "recipe has no steps"));
    }
    Ok(Recipe {
        json_schema: None,
        v: RECIPE_FORMAT_VERSION,
        name: name.into(),
        app: None,
        params: params.into_iter().map(Param::named).collect(),
        steps,
    })
}

/// Substitute bound values into step op payloads (targets, args,
/// assert names). Step `id`/`needs` are graph identity, never `$param`s.
/// `$$` becomes a literal `$`. A param with a `default` takes it when
/// `set` has no value; an optional param with neither is absent, and
/// every invoke arg written as exactly that placeholder is omitted.
///
/// # Errors
/// [`RecipeErrorCode::UnknownParam`] naming the first `set` key the
/// recipe does not declare; [`RecipeErrorCode::InvalidParamValue`] for a
/// value its param's type, `enum` or `pattern` rejects;
/// [`RecipeErrorCode::MissingParam`] naming the first required param
/// missing from `set`.
pub fn apply_params(
    recipe: &Recipe,
    set: &BTreeMap<String, String>,
) -> Result<Recipe, RecipeError> {
    if let Some(name) = set.keys().find(|name| recipe.param(name).is_none()) {
        return Err(err(
            RecipeErrorCode::UnknownParam,
            format!("--set {name}=… is not a param of recipe `{}`", recipe.name),
        ));
    }
    let mut bound = set.clone();
    let mut absent = BTreeSet::new();
    for param in &recipe.params {
        match (set.get(&param.name), &param.default) {
            (Some(value), _) => param
                .check(value)
                .map_err(|message| err(RecipeErrorCode::InvalidParamValue, message))?,
            (None, Some(default)) => {
                bound.insert(param.name.clone(), default.clone());
            }
            (None, None) if param.is_required() => {
                return Err(err(
                    RecipeErrorCode::MissingParam,
                    format!("missing --set {}=…", param.name),
                ));
            }
            (None, None) => {
                absent.insert(param.name.as_str());
            }
        }
    }
    let mut out = recipe.clone();
    for step in &mut out.steps {
        if let Op::Invoke { args, .. } = &mut step.op {
            omit_absent(args, &absent);
        }
        substitute_op_in_place(&mut step.op, &bound)
            .map_err(|message| err(RecipeErrorCode::MissingParam, message).at_step(&step.id))?;
    }
    Ok(out)
}

/// Check format version, params, step ids, `needs` edges (including
/// cycles), and op validity against `registry`.
///
/// # Errors
/// The first violation [`lint_recipe`] finds.
pub fn validate_recipe(recipe: &Recipe, registry: &Registry) -> Result<(), RecipeError> {
    match lint_recipe(recipe, registry).into_iter().next() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Every violation in `recipe`, in a deterministic order; empty when the
/// recipe is valid.
#[expect(
    clippy::too_many_lines,
    reason = "one linear list of format rules; splitting it hides the rule order"
)]
pub fn lint_recipe(recipe: &Recipe, registry: &Registry) -> Vec<RecipeError> {
    use RecipeErrorCode as Code;

    let mut errors = Vec::new();
    if recipe.v != RECIPE_FORMAT_VERSION {
        errors.push(err(
            Code::UnsupportedVersion,
            format!(
                "unsupported recipe version {} (want {RECIPE_FORMAT_VERSION})",
                recipe.v
            ),
        ));
    }
    if recipe.name.trim().is_empty() {
        errors.push(err(Code::InvalidRecipe, "recipe name cannot be empty"));
    }
    if recipe.steps.is_empty() {
        errors.push(err(Code::InvalidRecipe, "recipe has no steps"));
    }
    if recipe.steps.len() > MAX_RECIPE_STEPS {
        errors.push(err(
            Code::InvalidRecipe,
            format!(
                "recipe has {} steps (max {MAX_RECIPE_STEPS})",
                recipe.steps.len()
            ),
        ));
        return errors;
    }

    let mut params = BTreeSet::new();
    for param in &recipe.params {
        let name = &param.name;
        if !is_ident(name) {
            errors.push(err(
                Code::InvalidParam,
                format!("param `{name}` must match [A-Za-z_][A-Za-z0-9_]*"),
            ));
        } else if !params.insert(name.as_str()) {
            errors.push(err(Code::InvalidParam, format!("duplicate param `{name}`")));
        }
        for problem in param.lint() {
            errors.push(err(Code::InvalidParam, problem));
        }
    }
    let omittable: BTreeSet<&str> = recipe
        .params
        .iter()
        .filter(|param| param.omits_when_absent())
        .map(|param| param.name.as_str())
        .collect();

    for step in &recipe.steps {
        if let Some(delay) = step.key_delay_ms
            && !(1..=MAX_KEY_DELAY_MS).contains(&delay)
        {
            errors.push(
                err(
                    Code::InvalidStep,
                    format!(
                        "step `{}`: key_delay_ms must be 1 to {MAX_KEY_DELAY_MS}, got {delay}",
                        step.id
                    ),
                )
                .at_step(&step.id),
            );
        }
    }

    let mut ids = BTreeSet::new();
    for step in &recipe.steps {
        if step.id.trim().is_empty() {
            errors.push(err(Code::InvalidStep, "step id cannot be empty"));
        } else if string_has_placeholder(&step.id) {
            errors.push(
                err(
                    Code::InvalidStep,
                    format!(
                        "step id `{}` cannot contain $params (graph identity is not substitutable)",
                        step.id
                    ),
                )
                .at_step(&step.id),
            );
        } else if !ids.insert(step.id.as_str()) {
            errors.push(
                err(
                    Code::InvalidStep,
                    format!("duplicate step id `{}`", step.id),
                )
                .at_step(&step.id),
            );
        }
    }

    let mut used = BTreeSet::new();
    for step in &recipe.steps {
        let mut seen = BTreeSet::new();
        for need in &step.needs {
            let problem = if string_has_placeholder(need) {
                Some(format!(
                    "step `{}` needs `{need}` cannot contain $params (graph identity is not substitutable)",
                    step.id
                ))
            } else if !ids.contains(need.as_str()) {
                Some(format!("step `{}` needs unknown `{need}`", step.id))
            } else if need == &step.id {
                Some(format!("step `{}` cannot need itself", step.id))
            } else if !seen.insert(need.as_str()) {
                Some(format!("step `{}` lists needs `{need}` twice", step.id))
            } else {
                None
            };
            if let Some(message) = problem {
                errors.push(err(Code::InvalidNeeds, message).at_step(&step.id));
            }
        }

        let mut found = BTreeSet::new();
        collect_placeholders_in_op(&step.op, &mut found);
        for name in found {
            if recipe.param(&name).is_none() {
                errors.push(
                    err(
                        Code::InvalidParam,
                        format!(
                            "placeholder ${name} is not declared in recipe.params (write $${name} for a literal)"
                        ),
                    )
                    .at_step(&step.id),
                );
            }
            used.insert(name);
        }
        if !omittable.is_empty() {
            let mut kept = step.op.clone();
            if let Op::Invoke { args, .. } = &mut kept {
                omit_absent(args, &omittable);
            }
            let mut left = BTreeSet::new();
            collect_placeholders_in_op(&kept, &mut left);
            for name in left.iter().filter(|name| omittable.contains(name.as_str())) {
                errors.push(
                    err(
                        Code::InvalidParam,
                        format!(
                            "optional param `{name}` has no default, so it can only be a whole invoke arg value (\"key\": \"${name}\"); give it a default or make it required"
                        ),
                    )
                    .at_step(&step.id),
                );
            }
        }

        if let Op::Keybinding {
            binding, confirm, ..
        } = &step.op
        {
            if gpui_agent::is_quit_binding(binding) && !confirm {
                errors.push(
                    err(
                        Code::Unconfirmed,
                        format!("dangerous keybinding `{binding}` requires confirm=true"),
                    )
                    .at_step(&step.id),
                );
            }
        }
        if let Op::Invoke { name, args } = &step.op {
            match registry.get(name) {
                Some(schema) if matches!(schema.kind, SchemaKind::Invoke) => {
                    if let Err(message) = check_invoke_args(schema, args, false) {
                        errors.push(err(Code::InvalidArgs, message).at_step(&step.id));
                    }
                }
                Some(_) => errors.push(
                    err(
                        Code::UnknownInvoke,
                        format!("invoke `{name}` is registered but is not an invoke schema"),
                    )
                    .at_step(&step.id),
                ),
                None => errors.push(
                    err(
                        Code::UnknownInvoke,
                        format!("unknown invoke `{name}` (not in schema registry; fail closed)"),
                    )
                    .at_step(&step.id),
                ),
            }
        }
    }

    for name in &params {
        if !used.contains(*name) {
            errors.push(err(
                Code::InvalidParam,
                format!("param `{name}` is declared but no step uses ${name}"),
            ));
        }
    }

    // Judged on the edges that name another step: a bad `needs` entry is
    // reported above, and neither is a cycle nor hides one elsewhere.
    let edges: Vec<RecipeStep> = recipe
        .steps
        .iter()
        .map(|step| {
            let mut step = step.clone();
            let id = step.id.clone();
            step.needs
                .retain(|need| *need != id && ids.contains(need.as_str()));
            step
        })
        .collect();
    if crate::plan::schedule(&edges).is_err() {
        errors.push(err(Code::Cycle, "recipe has a dependency cycle"));
    }
    errors
}

/// Check `args` against an invoke schema: every required arg present, no
/// arg the schema does not declare, and declared types respected.
///
/// Unbound (`bound == false`) strings that still hold a `$placeholder`
/// are not type-checked; [`coerce_invoke_args`] types them at bind time.
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "one flag: whether params are already bound"
)]
pub(crate) fn check_invoke_args(
    schema: &OpSchema,
    args: &Value,
    bound: bool,
) -> Result<(), String> {
    let empty = serde_json::Map::new();
    let map = match args {
        Value::Null => &empty,
        Value::Object(map) => map,
        _ if schema.args.is_none() && schema.required.is_empty() => return Ok(()),
        _ => return Err(format!("invoke `{}` args must be an object", schema.name)),
    };
    let required = schema.required.iter().map(String::as_str).chain(
        schema
            .args
            .iter()
            .flatten()
            .filter(|(_, spec)| spec.required)
            .map(|(name, _)| name.as_str()),
    );
    for name in required {
        if !map.contains_key(name) {
            return Err(format!(
                "invoke `{}` is missing required arg `{name}`",
                schema.name
            ));
        }
    }
    // Args not listed at all: only `required` is known about them.
    let Some(listed) = &schema.args else {
        return Ok(());
    };
    for (name, value) in map {
        let Some(spec) = listed.get(name) else {
            // Named in `required` with no `args` entry: declared, untyped.
            if schema.required.contains(name) {
                continue;
            }
            let declared: BTreeSet<&str> = listed
                .keys()
                .chain(&schema.required)
                .map(String::as_str)
                .collect();
            return Err(format!(
                "invoke `{}` has no arg `{name}` (schema declares: {})",
                schema.name,
                if declared.is_empty() {
                    "none".to_string()
                } else {
                    declared.into_iter().collect::<Vec<_>>().join(", ")
                }
            ));
        };
        if !bound && value.as_str().is_some_and(string_has_placeholder) {
            continue;
        }
        let ok = match spec.ty.as_str() {
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.is_i64() || value.is_u64(),
            "bool" | "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            _ => true,
        };
        if !ok {
            return Err(format!(
                "invoke `{}` arg `{name}` must be {}, got {value}",
                schema.name, spec.ty
            ));
        }
    }
    Ok(())
}

/// Convert args that were written as exactly one placeholder (`"$id"`)
/// to the number / integer / bool type the schema declares, so a bound
/// `--set id=1` reaches the host as `1`, not `"1"`.
pub(crate) fn coerce_invoke_args(
    schema: &OpSchema,
    unbound: &Value,
    bound: &mut Value,
) -> Result<(), String> {
    let (Some(unbound), Some(bound)) = (unbound.as_object(), bound.as_object_mut()) else {
        return Ok(());
    };
    for (name, spec) in schema.args.iter().flatten() {
        let Some(param) = unbound
            .get(name)
            .and_then(Value::as_str)
            .and_then(whole_placeholder)
        else {
            continue;
        };
        let Some(Value::String(text)) = bound.get(name) else {
            continue;
        };
        let parsed = match spec.ty.as_str() {
            "number" => text.parse::<serde_json::Number>().ok().map(Value::Number),
            "integer" => text
                .parse::<i64>()
                .map(Value::from)
                .or_else(|_| text.parse::<u64>().map(Value::from))
                .ok(),
            "bool" | "boolean" => text.parse::<bool>().ok().map(Value::Bool),
            _ => continue,
        };
        let value = parsed.ok_or_else(|| {
            format!(
                "--set {param}={text} is not a valid {} for invoke `{}` arg `{name}`",
                spec.ty, schema.name
            )
        })?;
        bound.insert(name.clone(), value);
    }
    Ok(())
}

/// Drop every object entry whose value is exactly the placeholder of an
/// absent param.
fn omit_absent(value: &mut Value, absent: &BTreeSet<&str>) {
    match value {
        Value::Object(map) => {
            map.retain(|_, entry| {
                !entry
                    .as_str()
                    .and_then(whole_placeholder)
                    .is_some_and(|name| absent.contains(name))
            });
            for entry in map.values_mut() {
                omit_absent(entry, absent);
            }
        }
        Value::Array(items) => {
            for item in items {
                omit_absent(item, absent);
            }
        }
        _ => {}
    }
}

/// The param name when `s` is exactly one `$name` / `${name}`.
fn whole_placeholder(s: &str) -> Option<&str> {
    let (name, end) = parse_placeholder_bytes(s.as_bytes(), 0)?;
    (s.starts_with('$') && end == s.len()).then_some(name)
}

/// Keep this match in sync with [`substitute_op_in_place`]: every stringy
/// `Op` field must be visited or `$param` substitution silently skips it.
fn collect_placeholders_in_op(op: &Op, out: &mut BTreeSet<String>) {
    match op {
        Op::Blob { .. }
        | Op::Press { .. }
        | Op::Scroll { .. }
        | Op::Hover { .. }
        | Op::Drag { .. }
        | Op::Swipe { .. }
        | Op::LongPress { .. }
        | Op::Pinch { .. }
        | Op::Viewport { .. }
        | Op::Paste { .. }
        | Op::System { .. } => {
            for text in op.clone().added_strings_mut() {
                collect_placeholders(text, out);
            }
        }
        Op::Hello
        | Op::Snapshot { .. }
        | Op::Shutdown
        | Op::Wait { .. }
        | Op::Keybindings
        | Op::Events { .. }
        | Op::Revision => {}
        Op::WaitUntil { spec, .. } | Op::Assert { spec } => {
            collect_placeholders(&spec.target, out);
            if let Some(name) = &spec.name {
                collect_placeholders(name, out);
            }
            if let Some(value) = &spec.value {
                collect_placeholders(value, out);
            }
            if let Some(role) = &spec.role {
                collect_placeholders(role, out);
            }
        }
        Op::Click { target, .. } => collect_placeholders(target, out),
        Op::Type { target, text, .. } => {
            collect_placeholders(target, out);
            collect_placeholders(text, out);
        }
        Op::SetValue { target, value, .. } => {
            collect_placeholders(target, out);
            collect_placeholders(value, out);
        }
        Op::Key { target, key, .. } => {
            collect_placeholders(target, out);
            collect_placeholders(key, out);
        }
        Op::Keybinding { binding, chord, .. } => {
            collect_placeholders(binding, out);
            if let Some(chord) = chord {
                collect_placeholders(chord, out);
            }
        }
        Op::Invoke { name, args } => {
            collect_placeholders(name, out);
            collect_placeholders_in_value(args, out);
        }
        Op::Screenshot { path, target, .. } => {
            if let Some(path) = path {
                collect_placeholders(path, out);
            }
            if let Some(target) = target {
                collect_placeholders(target, out);
            }
        }
    }
}

fn collect_placeholders_in_value(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::String(s) => collect_placeholders(s, out),
        Value::Array(xs) => {
            for x in xs {
                collect_placeholders_in_value(x, out);
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                collect_placeholders_in_value(v, out);
            }
        }
        _ => {}
    }
}

fn string_has_placeholder(s: &str) -> bool {
    let mut found = BTreeSet::new();
    collect_placeholders(s, &mut found);
    !found.is_empty()
}

fn collect_placeholders(s: &str, out: &mut BTreeSet<String>) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'$') {
            i += 2;
            continue;
        }
        if bytes[i] == b'$'
            && let Some((name, next)) = parse_placeholder_bytes(bytes, i)
        {
            out.insert(name.to_string());
            i = next;
            continue;
        }
        i += 1;
    }
}

/// `$ident` / `${ident}` with ASCII names. `dollar` is a byte offset of `$`.
fn parse_placeholder_bytes(bytes: &[u8], dollar: usize) -> Option<(&str, usize)> {
    let i = dollar + 1;
    if i >= bytes.len() {
        return None;
    }
    if bytes[i] == b'{' {
        let rel = bytes[i + 1..].iter().position(|&c| c == b'}')?;
        let end = i + 1 + rel;
        let name = std::str::from_utf8(&bytes[i + 1..end]).ok()?;
        if is_ident(name) {
            return Some((name, end + 1));
        }
        return None;
    }
    let mut end = i;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    if end == i {
        return None;
    }
    let name = std::str::from_utf8(&bytes[i..end]).ok()?;
    if is_ident(name) {
        Some((name, end))
    } else {
        None
    }
}

fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Keep this match in sync with [`collect_placeholders_in_op`].
fn substitute_op_in_place(op: &mut Op, set: &BTreeMap<String, String>) -> Result<(), String> {
    match op {
        Op::Blob { .. }
        | Op::Press { .. }
        | Op::Scroll { .. }
        | Op::Hover { .. }
        | Op::Drag { .. }
        | Op::Swipe { .. }
        | Op::LongPress { .. }
        | Op::Pinch { .. }
        | Op::Viewport { .. }
        | Op::Paste { .. }
        | Op::System { .. } => {
            for text in op.added_strings_mut() {
                substitute_string_in_place(text, set)?;
            }
            Ok(())
        }
        Op::Hello
        | Op::Snapshot { .. }
        | Op::Shutdown
        | Op::Wait { .. }
        | Op::Keybindings
        | Op::Events { .. }
        | Op::Revision => Ok(()),
        Op::WaitUntil { spec, .. } | Op::Assert { spec } => {
            substitute_string_in_place(&mut spec.target, set)?;
            if let Some(name) = &mut spec.name {
                substitute_string_in_place(name, set)?;
            }
            if let Some(value) = &mut spec.value {
                substitute_string_in_place(value, set)?;
            }
            if let Some(role) = &mut spec.role {
                substitute_string_in_place(role, set)?;
            }
            Ok(())
        }
        Op::Click { target, .. } => substitute_string_in_place(target, set),
        Op::Type { target, text, .. } => {
            substitute_string_in_place(target, set)?;
            substitute_string_in_place(text, set)
        }
        Op::SetValue { target, value, .. } => {
            substitute_string_in_place(target, set)?;
            substitute_string_in_place(value, set)
        }
        Op::Key { target, key, .. } => {
            substitute_string_in_place(target, set)?;
            substitute_string_in_place(key, set)
        }
        Op::Keybinding { binding, chord, .. } => {
            substitute_string_in_place(binding, set)?;
            if let Some(chord) = chord {
                substitute_string_in_place(chord, set)?;
            }
            Ok(())
        }
        Op::Invoke { name, args } => {
            substitute_string_in_place(name, set)?;
            substitute_value(args, set)
        }
        Op::Screenshot { path, target, .. } => {
            if let Some(path) = path {
                substitute_string_in_place(path, set)?;
            }
            if let Some(target) = target {
                substitute_string_in_place(target, set)?;
            }
            Ok(())
        }
    }
}

fn substitute_value(value: &mut Value, set: &BTreeMap<String, String>) -> Result<(), String> {
    match value {
        Value::String(s) => *s = substitute_string(s, set)?,
        Value::Array(xs) => {
            for x in xs {
                substitute_value(x, set)?;
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                substitute_value(v, set)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn substitute_string_in_place(
    input: &mut String,
    set: &BTreeMap<String, String>,
) -> Result<(), String> {
    if !input.as_bytes().contains(&b'$') {
        return Ok(());
    }
    *input = substitute_string(input, set)?;
    Ok(())
}

fn substitute_string(input: &str, set: &BTreeMap<String, String>) -> Result<String, String> {
    let bytes = input.as_bytes();
    if !bytes.contains(&b'$') {
        return Ok(input.to_string());
    }
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'$') {
            out.push('$');
            i += 2;
            continue;
        }
        if bytes[i] == b'$'
            && let Some((name, next)) = parse_placeholder_bytes(bytes, i)
        {
            let value = set
                .get(name)
                .ok_or_else(|| format!("missing --set {name}=…"))?;
            out.push_str(value);
            i = next;
            continue;
        }
        let ch = input[i..]
            .chars()
            .next()
            .expect("byte index is a char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    Ok(out)
}

fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::with_capacity(8);
    let mut buf = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                let quote = c;
                loop {
                    match chars.next() {
                        Some(ch) if ch == quote => break,
                        Some('\\') => {
                            if let Some(next) = chars.next() {
                                buf.push(next);
                            }
                        }
                        Some(ch) => buf.push(ch),
                        None => return Err("unterminated quote".into()),
                    }
                }
                if chars.peek().is_none_or(|ch| ch.is_whitespace()) {
                    tokens.push(std::mem::take(&mut buf));
                }
            }
            ch if ch.is_whitespace() => {
                if !buf.is_empty() {
                    tokens.push(std::mem::take(&mut buf));
                }
            }
            ch => buf.push(ch),
        }
    }
    if !buf.is_empty() {
        tokens.push(buf);
    }
    Ok(tokens)
}

#[expect(
    clippy::too_many_lines,
    reason = "one match arm per op spelling; splitting by op would scatter the parse table"
)]
fn parse_wants_step(mut tokens: Vec<String>, id: String) -> Result<RecipeStep, String> {
    let mut screenshot = false;
    tokens.retain(|tok| {
        if tok.as_str() == "--screenshot" {
            screenshot = true;
            false
        } else {
            true
        }
    });
    let fingerprint = take_fingerprint(&mut tokens)?;
    let key_delay_ms = take_key_delay(&mut tokens)?;
    let tokens = &tokens;
    if tokens.is_empty() {
        return Err("missing op".into());
    }
    let op_name = tokens[0].replace('-', "_");
    let op = match op_name.as_str() {
        "wait" => Op::Wait { timeout_ms: None },
        "wait_until" => parse_wait_until_op(&tokens[1..])?,
        "hello" => Op::Hello,
        "snapshot" => Op::snapshot(),
        "revision" => Op::Revision,
        "shutdown" => Op::Shutdown,
        "click" => {
            let (delivery, rest) = take_delivery(&tokens[1..])?;
            let target = rest
                .first()
                .ok_or_else(|| "click needs a target".to_string())?
                .clone();
            Op::Click {
                target,
                delivery,
                expect_fingerprint: None,
            }
        }
        "type" => {
            let (delivery, rest) = take_delivery(&tokens[1..])?;
            if rest.len() < 2 {
                return Err("type needs target and text".into());
            }
            Op::Type {
                target: rest[0].clone(),
                text: rest[1].clone(),
                delivery,
                expect_fingerprint: None,
            }
        }
        "set_value" => {
            if tokens.len() < 3 {
                return Err("set-value needs target and value".into());
            }
            Op::SetValue {
                target: tokens[1].clone(),
                value: tokens[2].clone(),
                expect_fingerprint: None,
            }
        }
        "key" => {
            let (delivery, rest) = take_delivery(&tokens[1..])?;
            if rest.len() < 2 {
                return Err("key needs target and key name".into());
            }
            Op::Key {
                target: rest[0].clone(),
                key: rest[1].clone(),
                delivery,
                expect_fingerprint: None,
            }
        }
        "keybindings" | "keybinding.list" => Op::Keybindings,
        "keybinding" => parse_keybinding_wants(&tokens[1..])?,
        "assert" => Op::Assert {
            spec: parse_assert_spec(&tokens[1..])?,
        },
        "screenshot" => parse_screenshot_op(&tokens[1..])?,
        "invoke" => {
            let name = tokens
                .get(1)
                .ok_or_else(|| "invoke needs a name".to_string())?
                .clone();
            let mut map = serde_json::Map::new();
            for token in &tokens[2..] {
                let (key, raw) = token
                    .split_once('=')
                    .ok_or_else(|| format!("expected KEY=VALUE, got {token}"))?;
                let value =
                    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()));
                map.insert(key.to_string(), value);
            }
            Op::Invoke {
                name,
                args: Value::Object(map),
            }
        }
        other => return Err(format!("unknown op `{other}`")),
    };
    let op = match fingerprint {
        Some(fingerprint) => {
            let op = op.with_fingerprint(fingerprint);
            if op.fingerprint_gate().is_none() {
                return Err("--fp applies to click, type, set-value and key".into());
            }
            op
        }
        None => op,
    };
    if key_delay_ms.is_some() && !matches!(op, Op::Type { .. }) {
        return Err("--key-delay-ms applies to type".into());
    }
    Ok(RecipeStep {
        id,
        needs: Vec::new(),
        screenshot,
        key_delay_ms,
        op,
    })
}

/// Removes `--key-delay-ms <n>` (the pause between two keys of a paced
/// `type`, JSON: `key_delay_ms`) from `tokens`.
fn take_key_delay(tokens: &mut Vec<String>) -> Result<Option<u64>, String> {
    let Some(at) = tokens.iter().position(|tok| tok == "--key-delay-ms") else {
        return Ok(None);
    };
    let raw = tokens
        .get(at + 1)
        .ok_or_else(|| "--key-delay-ms needs milliseconds".to_string())?;
    let delay = raw
        .parse()
        .map_err(|_| format!("--key-delay-ms needs an unsigned integer, got {raw}"))?;
    tokens.drain(at..at + 2);
    if tokens.iter().any(|tok| tok == "--key-delay-ms") {
        return Err("--key-delay-ms given twice".into());
    }
    Ok(Some(delay))
}

/// Removes `--fp <n>` (the target's expected fingerprint, from
/// `snapshot --fingerprints`) from `tokens`.
fn take_fingerprint(tokens: &mut Vec<String>) -> Result<Option<u64>, String> {
    let Some(at) = tokens.iter().position(|tok| tok == "--fp") else {
        return Ok(None);
    };
    let raw = tokens
        .get(at + 1)
        .ok_or_else(|| "--fp needs a fingerprint".to_string())?;
    let fingerprint = raw
        .parse()
        .map_err(|_| format!("--fp needs an unsigned integer, got {raw}"))?;
    tokens.drain(at..at + 2);
    if tokens.iter().any(|tok| tok == "--fp") {
        return Err("--fp given twice".into());
    }
    Ok(Some(fingerprint))
}

fn parse_keybinding_wants(tokens: &[String]) -> Result<Op, String> {
    use gpui_agent::protocol::KeybindingScope;

    let mut binding = None;
    let mut chord = None;
    let mut scope = None;
    let mut confirm = false;
    let mut activate = false;
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        if token == "--id" || token == "--binding" {
            i += 1;
            binding = Some(
                tokens
                    .get(i)
                    .ok_or_else(|| "keybinding needs --id".to_string())?
                    .clone(),
            );
        } else if token == "--scope" {
            i += 1;
            let raw = tokens
                .get(i)
                .ok_or_else(|| "--scope needs focused or global".to_string())?;
            scope = Some(raw.parse::<KeybindingScope>()?);
        } else if token == "--chord" {
            i += 1;
            chord = Some(
                tokens
                    .get(i)
                    .ok_or_else(|| "--chord needs a value".to_string())?
                    .clone(),
            );
        } else if token == "--confirm" {
            if tokens.get(i + 1).map(String::as_str) == Some("true") {
                confirm = true;
                i += 1;
            } else if tokens.get(i + 1).map(String::as_str) == Some("false") {
                confirm = false;
                i += 1;
            } else {
                confirm = true;
            }
        } else if token == "--activate" {
            if tokens.get(i + 1).map(String::as_str) == Some("false") {
                activate = false;
                i += 1;
            } else {
                activate = true;
                if tokens.get(i + 1).map(String::as_str) == Some("true") {
                    i += 1;
                }
            }
        } else if !token.starts_with('-') && binding.is_none() {
            binding = Some(token.clone());
        } else {
            return Err(format!("bad keybinding token `{token}`"));
        }
        i += 1;
    }
    let binding = binding.ok_or_else(|| "keybinding needs --id".to_string())?;
    let scope = scope.ok_or_else(|| "keybinding needs --scope focused|global".to_string())?;
    Ok(Op::Keybinding {
        binding,
        chord,
        scope,
        confirm,
        activate,
    })
}

fn parse_screenshot_op(tokens: &[String]) -> Result<Op, String> {
    use gpui_agent::protocol::ScreenshotMode;
    let mut path = None;
    let mut mode = ScreenshotMode::Viewport;
    let mut target = None;
    let mut max_height_px = None;
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "--out" => {
                i += 1;
                let value = tokens
                    .get(i)
                    .ok_or_else(|| "--out needs a path".to_string())?;
                path = Some(value.clone());
            }
            "--mode" => {
                i += 1;
                let value = tokens
                    .get(i)
                    .ok_or_else(|| "--mode needs viewport or scrolled".to_string())?;
                mode = value.parse()?;
            }
            "--target" => {
                i += 1;
                let value = tokens
                    .get(i)
                    .ok_or_else(|| "--target needs a scroll-view id".to_string())?;
                target = Some(value.clone());
            }
            "--max-height-px" => {
                i += 1;
                let value = tokens
                    .get(i)
                    .ok_or_else(|| "--max-height-px needs a number".to_string())?;
                max_height_px = Some(
                    value
                        .parse()
                        .map_err(|_| format!("bad max_height_px `{value}`"))?,
                );
            }
            other if other.starts_with('-') => {
                return Err(format!("bad screenshot token `{other}`"));
            }
            other if path.is_none() => path = Some(other.to_string()),
            other => return Err(format!("bad screenshot token `{other}`")),
        }
        i += 1;
    }
    if mode.is_scrolled() && target.as_deref().map_or("", str::trim).is_empty() {
        return Err("screenshot mode=scrolled requires --target".into());
    }
    Ok(Op::Screenshot {
        path,
        mode,
        target,
        max_height_px,
        inline: false,
    })
}

fn take_delivery(tokens: &[String]) -> Result<(DeliveryMode, &[String]), String> {
    if tokens.first().map(String::as_str) == Some("--delivery") {
        let raw = tokens
            .get(1)
            .ok_or_else(|| "--delivery needs semantic or virtual".to_string())?;
        let delivery: DeliveryMode = raw.parse()?;
        Ok((delivery, &tokens[2..]))
    } else {
        Ok((DeliveryMode::Semantic, tokens))
    }
}

fn parse_assert_spec(tokens: &[String]) -> Result<AssertSpec, String> {
    let mut spec = AssertSpec::default();
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        if token == "--id" || token == "--target" {
            i += 1;
            spec.target.clone_from(
                tokens
                    .get(i)
                    .ok_or_else(|| "missing assert target".to_string())?,
            );
        } else if token == "--name" {
            i += 1;
            spec.name = Some(
                tokens
                    .get(i)
                    .ok_or_else(|| "missing --name".to_string())?
                    .clone(),
            );
        } else if token == "--value" {
            i += 1;
            spec.value = Some(
                tokens
                    .get(i)
                    .ok_or_else(|| "missing --value".to_string())?
                    .clone(),
            );
        } else if token == "--role" {
            i += 1;
            spec.role = Some(
                tokens
                    .get(i)
                    .ok_or_else(|| "missing --role".to_string())?
                    .clone(),
            );
        } else if token == "--checked" {
            if tokens.get(i + 1).map(String::as_str) == Some("true") {
                spec.checked = Some(true);
                i += 1;
            } else if tokens.get(i + 1).map(String::as_str) == Some("false") {
                spec.checked = Some(false);
                i += 1;
            } else {
                spec.checked = Some(true);
            }
        } else if token == "--exists" {
            if tokens.get(i + 1).map(String::as_str) == Some("false") {
                spec.exists = Some(false);
                i += 1;
            } else {
                spec.exists = Some(true);
                if tokens.get(i + 1).map(String::as_str) == Some("true") {
                    i += 1;
                }
            }
        } else if token == "--absent" {
            spec.exists = Some(false);
        } else if token == "--visible" {
            spec.visible = Some(parse_optional_bool_flag(tokens, &mut i));
        } else if token == "--in-viewport" || token == "--in_viewport" {
            spec.in_viewport = Some(parse_optional_bool_flag(tokens, &mut i));
        } else if let Some((key, raw)) = token.split_once('=') {
            apply_assert_kv(&mut spec, key, raw)?;
        } else if spec.target.is_empty() && !token.starts_with('-') {
            spec.target.clone_from(token);
        } else {
            return Err(format!("bad assert token `{token}`"));
        }
        i += 1;
    }
    if spec.target.is_empty() {
        return Err("assert needs a target".into());
    }
    Ok(spec)
}

fn apply_assert_kv(spec: &mut AssertSpec, key: &str, raw: &str) -> Result<(), String> {
    match key {
        "name" => spec.name = Some(raw.into()),
        "value" => spec.value = Some(raw.into()),
        "role" => spec.role = Some(raw.into()),
        "checked" => {
            spec.checked = Some(parse_bool(raw)?);
        }
        "exists" => spec.exists = Some(parse_bool(raw)?),
        "visible" => spec.visible = Some(parse_bool(raw)?),
        "in_viewport" | "in-viewport" => spec.in_viewport = Some(parse_bool(raw)?),
        "target" | "id" => spec.target = raw.into(),
        other => return Err(format!("unknown assert field `{other}`")),
    }
    Ok(())
}

fn parse_bool(raw: &str) -> Result<bool, String> {
    match raw {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("expected true/false, got {other}")),
    }
}

fn parse_optional_bool_flag(tokens: &[String], i: &mut usize) -> bool {
    if tokens.get(*i + 1).map(String::as_str) == Some("true") {
        *i += 1;
        true
    } else if tokens.get(*i + 1).map(String::as_str) == Some("false") {
        *i += 1;
        false
    } else {
        true
    }
}

fn parse_wait_until_op(tokens: &[String]) -> Result<Op, String> {
    let mut timeout_ms = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i] == "--timeout-ms" || tokens[i] == "--timeout_ms" {
            i += 1;
            let raw = tokens
                .get(i)
                .ok_or_else(|| "wait_until --timeout-ms needs a number".to_string())?;
            timeout_ms = Some(
                raw.parse::<u64>()
                    .map_err(|_| format!("bad timeout_ms `{raw}`"))?,
            );
        } else if let Some(raw) = tokens[i].strip_prefix("timeout_ms=") {
            timeout_ms = Some(
                raw.parse::<u64>()
                    .map_err(|_| format!("bad timeout_ms `{raw}`"))?,
            );
        } else {
            rest.push(tokens[i].clone());
        }
        i += 1;
    }
    let timeout_ms = timeout_ms.ok_or_else(|| "wait_until needs --timeout-ms".to_string())?;
    Ok(Op::WaitUntil {
        timeout_ms,
        spec: parse_assert_spec(&rest)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::todo_registry;
    use serde_json::json;

    #[test]
    fn json_recipe_roundtrip() {
        let json = r#"{
            "name": "demo",
            "params": ["title"],
            "steps": [
                {"id": "set", "op": "set_value", "target": "todo-input", "value": "$title"},
                {"id": "add", "op": "click", "target": "todo-add", "needs": ["set"], "screenshot": true}
            ]
        }"#;
        let recipe = Recipe::from_json(json).unwrap();
        validate_recipe(&recipe, &todo_registry()).unwrap();
        assert_eq!(recipe.steps.len(), 2);
        assert_eq!(recipe.steps[1].needs, vec!["set"]);
        assert!(!recipe.steps[0].screenshot);
        assert!(recipe.steps[1].screenshot);
    }

    #[test]
    fn wants_attached_quotes_become_one_token() {
        let recipe = parse_wants(r#"invoke todo.add title="Buy milk""#, "q").unwrap();
        match &recipe.steps[0].op {
            Op::Invoke { args, .. } => assert_eq!(args["title"], "Buy milk"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wants_parse_and_params() {
        let wants = r"
# create a todo
set-value todo-input $title
click todo-add
assert todo-item-1 name=$title checked=false
";
        let recipe = parse_wants(wants, "crud").unwrap();
        assert_eq!(recipe.param_names(), vec!["title"]);
        validate_recipe(&recipe, &todo_registry()).unwrap();
        let mut set = BTreeMap::new();
        set.insert("title".into(), "Buy milk".into());
        let bound = apply_params(&recipe, &set).unwrap();
        match &bound.steps[0].op {
            Op::SetValue { value, .. } => assert_eq!(value, "Buy milk"),
            other => panic!("{other:?}"),
        }
        match &bound.steps[2].op {
            Op::Assert { spec } => assert_eq!(spec.name.as_deref(), Some("Buy milk")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_invoke_fails_closed() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "bad",
            "steps": [{"id": "x", "op": "invoke", "name": "shell.run", "args": {"cmd": "rm"}}]
        }"#,
        )
        .unwrap();
        let err = validate_recipe(&recipe, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown invoke"), "{err}");
    }

    #[test]
    fn undeclared_placeholder_is_rejected() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "bad",
            "steps": [{"id": "x", "op": "set_value", "target": "todo-input", "value": "$title"}]
        }"#,
        )
        .unwrap();
        let err = validate_recipe(&recipe, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("not declared"), "{err}");
    }

    #[test]
    fn empty_source_is_rejected() {
        let err = parse_recipe_source("", "empty", false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no steps"), "{err}");
        let err = parse_recipe_source("   \n# only comments\n\n", "c", false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no steps"), "{err}");
    }

    #[test]
    fn empty_json_steps_are_rejected() {
        let recipe = Recipe::from_json(r#"{"name":"x","steps":[]}"#).unwrap();
        let err = validate_recipe(&recipe, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("no steps"), "{err}");
    }

    #[test]
    fn bad_json_is_rejected() {
        let err = parse_recipe_source("{", "bad", true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("recipe json"), "{err}");
        let err = parse_recipe_source("not-json", "bad", true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("recipe json"), "{err}");
    }

    #[test]
    fn empty_file_via_parse_recipe() {
        let path = std::env::temp_dir().join("gpui-agent-empty.wants");
        std::fs::write(&path, "").unwrap();
        let err = parse_recipe(&path).unwrap_err().to_string();
        assert!(err.contains("no steps"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_wants_op_is_rejected() {
        let err = parse_wants("frobnicate todo-add", "x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown op"), "{err}");
    }

    #[test]
    fn missing_required_params_at_apply() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "p",
            "params": ["title"],
            "steps": [{"id": "x", "op": "set_value", "target": "todo-input", "value": "$title"}]
        }"#,
        )
        .unwrap();
        validate_recipe(&recipe, &todo_registry()).unwrap();
        let err = apply_params(&recipe, &BTreeMap::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing --set title"), "{err}");
    }

    #[test]
    fn duplicate_step_ids_are_rejected() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "dup",
            "steps": [
                {"id": "a", "op": "hello"},
                {"id": "a", "op": "snapshot"}
            ]
        }"#,
        )
        .unwrap();
        let err = validate_recipe(&recipe, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate"), "{err}");
    }

    #[test]
    fn empty_step_id_and_empty_name_are_rejected() {
        let recipe = Recipe::from_json(r#"{"name":"","steps":[{"id":"a","op":"hello"}]}"#).unwrap();
        let err = validate_recipe(&recipe, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("name"), "{err}");

        let recipe = Recipe::from_json(r#"{"name":"x","steps":[{"id":"","op":"hello"}]}"#).unwrap();
        let err = validate_recipe(&recipe, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("step id"), "{err}");
    }

    #[test]
    fn unknown_and_self_needs_are_rejected() {
        let missing = Recipe::from_json(
            r#"{"name":"x","steps":[{"id":"a","op":"hello","needs":["ghost"]}]}"#,
        )
        .unwrap();
        let err = validate_recipe(&missing, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown"), "{err}");

        let sel =
            Recipe::from_json(r#"{"name":"x","steps":[{"id":"a","op":"hello","needs":["a"]}]}"#)
                .unwrap();
        let err = validate_recipe(&sel, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("itself"), "{err}");
    }

    #[test]
    fn unsupported_recipe_version() {
        let recipe =
            Recipe::from_json(r#"{"v":99,"name":"x","steps":[{"id":"a","op":"hello"}]}"#).unwrap();
        let err = validate_recipe(&recipe, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsupported recipe version"), "{err}");
    }

    #[test]
    fn too_many_steps_are_rejected() {
        assert_eq!(MAX_RECIPE_STEPS, 256);
        let steps: Vec<serde_json::Value> = (0..=MAX_RECIPE_STEPS)
            .map(|i| serde_json::json!({"id": format!("s{i}"), "op": "hello"}))
            .collect();
        let recipe =
            Recipe::from_json(&serde_json::json!({"name":"big","steps": steps}).to_string())
                .unwrap();
        let err = validate_recipe(&recipe, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("max"), "{err}");
    }

    #[test]
    fn invoke_protocol_name_is_not_an_invoke_schema() {
        let recipe = Recipe::from_json(
            r#"{"name":"x","steps":[{"id":"a","op":"invoke","name":"click","args":{}}]}"#,
        )
        .unwrap();
        let err = validate_recipe(&recipe, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("not an invoke schema"), "{err}");
    }

    #[test]
    fn wants_fp_guards_the_step_with_a_fingerprint() {
        let recipe = parse_wants(
            "click --fp 42 todo-add\ntype --delivery virtual todo-input hi --fp 7\nset-value --fp 9 todo-input x\nkey todo-input Enter --fp 18446744073709551615",
            "fp",
        )
        .unwrap();
        let gates: Vec<_> = recipe
            .steps
            .iter()
            .map(|step| step.op.fingerprint_gate().unwrap())
            .collect();
        assert_eq!(
            gates,
            [
                ("todo-add", 42),
                ("todo-input", 7),
                ("todo-input", 9),
                ("todo-input", u64::MAX)
            ]
        );
        assert_eq!(recipe.steps[1].op.delivery(), DeliveryMode::Virtual);
    }

    #[test]
    fn key_delay_paces_a_type_step() {
        let recipe = Recipe::from_json(
            r#"{"name": "r", "steps": [
                {"id": "a", "op": "type", "target": "todo-input", "text": "1234.56", "key_delay_ms": 80}
            ]}"#,
        )
        .unwrap();
        assert_eq!(recipe.steps[0].key_delay_ms, Some(80));
        validate_recipe(&recipe, &todo_registry()).unwrap();
        let round = serde_json::to_value(&recipe.steps[0]).unwrap();
        assert_eq!(round["key_delay_ms"], 80);
    }

    #[test]
    fn key_delay_is_refused_on_any_op_but_type() {
        for step in [
            r#"{"id": "a", "op": "click", "target": "todo-add", "key_delay_ms": 80}"#,
            r#"{"id": "a", "op": "set_value", "target": "todo-input", "value": "x", "key_delay_ms": 80}"#,
            r#"{"id": "a", "op": "click", "target": "todo-add", "key_delay_ms": null}"#,
        ] {
            let err =
                Recipe::from_json(&format!(r#"{{"name": "r", "steps": [{step}]}}"#)).unwrap_err();
            assert_eq!(err.code, RecipeErrorCode::UnknownField, "{step}: {err}");
            assert!(err.to_string().contains("key_delay_ms"), "{step}: {err}");
        }
    }

    #[test]
    fn key_delay_must_be_one_to_ten_seconds() {
        for delay in [0, MAX_KEY_DELAY_MS + 1] {
            let recipe = Recipe::from_json(&format!(
                r#"{{"name": "r", "steps": [{{"id": "a", "op": "type", "target": "todo-input", "text": "x", "key_delay_ms": {delay}}}]}}"#
            ))
            .unwrap();
            let err = validate_recipe(&recipe, &todo_registry()).unwrap_err();
            assert_eq!(err.code, RecipeErrorCode::InvalidStep, "{delay}: {err}");
        }
        for delay in [1, MAX_KEY_DELAY_MS] {
            let recipe = Recipe::from_json(&format!(
                r#"{{"name": "r", "steps": [{{"id": "a", "op": "type", "target": "todo-input", "text": "x", "key_delay_ms": {delay}}}]}}"#
            ))
            .unwrap();
            validate_recipe(&recipe, &todo_registry()).unwrap();
        }
    }

    #[test]
    fn wants_key_delay_flag() {
        let recipe = parse_wants(
            "type todo-input 1234.56 --key-delay-ms 80\ntype --key-delay-ms 5 todo-input x\ntype todo-input y",
            "paced",
        )
        .unwrap();
        let delays: Vec<_> = recipe.steps.iter().map(|step| step.key_delay_ms).collect();
        assert_eq!(delays, [Some(80), Some(5), None]);
        for (line, needle) in [
            ("click todo-add --key-delay-ms 80", "applies to type"),
            ("type todo-input x --key-delay-ms", "needs milliseconds"),
            ("type todo-input x --key-delay-ms -1", "unsigned integer"),
            (
                "type todo-input x --key-delay-ms 1 --key-delay-ms 2",
                "twice",
            ),
        ] {
            let err = parse_wants(line, "x").unwrap_err().to_string();
            assert!(err.contains(needle), "{line}: {err}");
        }
    }

    #[test]
    fn wants_fp_is_refused_where_it_cannot_apply() {
        for (line, needle) in [
            ("assert todo-input --fp 3", "applies to"),
            ("click todo-add --fp", "needs a fingerprint"),
            ("click todo-add --fp -1", "unsigned integer"),
            ("click todo-add --fp 1 --fp 2", "twice"),
        ] {
            let err = parse_wants(line, "x").unwrap_err().to_string();
            assert!(err.contains(needle), "{line}: {err}");
        }
    }

    #[test]
    fn json_steps_accept_expect_fingerprint() {
        let recipe = Recipe::from_json(
            r#"{"name":"x","steps":[{"id":"a","op":"click","target":"todo-add","expect_fingerprint":18446744073709551615}]}"#,
        )
        .unwrap();
        assert_eq!(
            recipe.steps[0].op.fingerprint_gate(),
            Some(("todo-add", u64::MAX))
        );
        validate_recipe(&recipe, &todo_registry()).unwrap();
    }

    #[test]
    fn wants_skips_comments_and_blanks() {
        let recipe = parse_wants("\n# heading\n\nhello\n  # mid\n\nsnapshot\n", "c").unwrap();
        assert_eq!(recipe.steps.len(), 2);
        assert!(matches!(recipe.steps[0].op, Op::Hello));
        assert!(matches!(recipe.steps[1].op, Op::Snapshot { .. }));
    }

    #[test]
    fn wants_unterminated_quote_is_invalid() {
        let err = parse_wants(r#"set-value todo-input "no-end"#, "q")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unterminated quote"), "{err}");
    }

    #[test]
    fn wants_invalid_lines() {
        assert!(
            parse_wants("click", "x")
                .unwrap_err()
                .to_string()
                .contains("target")
        );
        assert!(
            parse_wants("assert", "x")
                .unwrap_err()
                .to_string()
                .contains("target")
        );
        assert!(
            parse_wants("invoke", "x")
                .unwrap_err()
                .to_string()
                .contains("name")
        );
        assert!(
            parse_wants("set-value todo-input", "x")
                .unwrap_err()
                .to_string()
                .contains("set-value")
        );
        assert!(
            parse_wants("type todo-input", "x")
                .unwrap_err()
                .to_string()
                .contains("type")
        );
        assert!(parse_wants("assert todo-item-1 noflag", "x").is_err());
    }

    #[test]
    fn wants_delivery_and_assert_flags() {
        let recipe = parse_wants(
            "click --delivery virtual todo-add\nassert --id todo-item-1 --absent\nassert todo-x checked=true exists=false",
            "d",
        )
        .unwrap();
        match &recipe.steps[0].op {
            Op::Click {
                target,
                delivery,
                expect_fingerprint: None,
            } => {
                assert_eq!(target, "todo-add");
                assert_eq!(*delivery, DeliveryMode::Virtual);
            }
            other => panic!("{other:?}"),
        }
        match &recipe.steps[1].op {
            Op::Assert { spec } => {
                assert_eq!(spec.target, "todo-item-1");
                assert_eq!(spec.exists, Some(false));
            }
            other => panic!("{other:?}"),
        }
        match &recipe.steps[2].op {
            Op::Assert { spec } => {
                assert_eq!(spec.checked, Some(true));
                assert_eq!(spec.exists, Some(false));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn json_extension_forces_json_even_without_brace() {
        let path = std::env::temp_dir().join("gpui-agent-not-object.json");
        std::fs::write(&path, "[]").unwrap();
        let err = parse_recipe(&path).unwrap_err().to_string();
        assert!(err.contains("recipe json"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_json_op_is_rejected() {
        let err = parse_recipe_source(
            r#"{"name":"x","steps":[{"id":"a","op":"explode"}]}"#,
            "x.json",
            true,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("recipe json"), "{err}");
    }

    #[test]
    fn brace_and_bare_params_substitute() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "p",
            "params": ["title"],
            "steps": [
                {"id": "a", "op": "set_value", "target": "todo-input", "value": "${title}"},
                {"id": "b", "op": "set_value", "target": "todo-input", "value": "Hi $title"}
            ]
        }"#,
        )
        .unwrap();
        validate_recipe(&recipe, &todo_registry()).unwrap();
        let mut set = BTreeMap::new();
        set.insert("title".into(), "Buy milk".into());
        let bound = apply_params(&recipe, &set).unwrap();
        match &bound.steps[0].op {
            Op::SetValue { value, .. } => assert_eq!(value, "Buy milk"),
            other => panic!("{other:?}"),
        }
        match &bound.steps[1].op {
            Op::SetValue { value, .. } => assert_eq!(value, "Hi Buy milk"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn params_named_id_do_not_rewrite_graph_identity() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "p",
            "params": ["id"],
            "steps": [
                {"id": "wait", "op": "wait"},
                {"id": "add", "op": "invoke", "name": "todo.add", "args": {"title": "$id"}, "needs": ["wait"]}
            ]
        }"#,
        )
        .unwrap();
        validate_recipe(&recipe, &todo_registry()).unwrap();
        let mut set = BTreeMap::new();
        set.insert("id".into(), "Buy milk".into());
        let bound = apply_params(&recipe, &set).unwrap();
        assert_eq!(bound.steps[0].id, "wait");
        assert_eq!(bound.steps[1].id, "add");
        assert_eq!(bound.steps[1].needs, vec!["wait"]);
        match &bound.steps[1].op {
            Op::Invoke { args, .. } => assert_eq!(args["title"], "Buy milk"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn placeholder_in_step_id_or_needs_fails_validate() {
        let in_id = Recipe::from_json(
            r#"{
            "name": "p",
            "params": ["id"],
            "steps": [{"id": "$id", "op": "hello"}]
        }"#,
        )
        .unwrap();
        let err = validate_recipe(&in_id, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("step id"), "{err}");
        assert!(
            err.contains("$params") || err.contains("graph identity"),
            "{err}"
        );

        let in_needs = Recipe::from_json(
            r#"{
            "name": "p",
            "params": ["id"],
            "steps": [
                {"id": "a", "op": "hello"},
                {"id": "b", "op": "hello", "needs": ["$id"]}
            ]
        }"#,
        )
        .unwrap();
        let err = validate_recipe(&in_needs, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("needs"), "{err}");
        assert!(
            err.contains("$params") || err.contains("graph identity"),
            "{err}"
        );
    }

    #[test]
    fn placeholder_in_needs_is_not_confused_with_a_real_step_id() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "p",
            "params": ["title"],
            "steps": [
                {"id": "wait", "op": "wait"},
                {"id": "add", "op": "hello", "needs": ["$title"]}
            ]
        }"#,
        )
        .unwrap();
        let err = validate_recipe(&recipe, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("needs"), "{err}");
        assert!(
            err.contains("$params") || err.contains("graph identity"),
            "placeholder needs must fail validate even if a later --set would bind: {err}"
        );
    }

    #[test]
    fn wants_standalone_quoted_value() {
        let recipe = parse_wants(r#"set-value todo-input "Buy milk""#, "q").unwrap();
        match &recipe.steps[0].op {
            Op::SetValue { target, value, .. } => {
                assert_eq!(target, "todo-input");
                assert_eq!(value, "Buy milk");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wants_escaped_quotes_inside_quotes() {
        let recipe = parse_wants(r#"set-value todo-input "Say \"hi\"""#, "q").unwrap();
        match &recipe.steps[0].op {
            Op::SetValue { value, .. } => assert_eq!(value, r#"Say "hi""#),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wants_key_and_wait_and_bad_delivery() {
        let recipe = parse_wants("wait\nkey todo-input Enter", "k").unwrap();
        assert!(matches!(recipe.steps[0].op, Op::Wait { .. }));
        match &recipe.steps[1].op {
            Op::Key { target, key, .. } => {
                assert_eq!(target, "todo-input");
                assert_eq!(key, "Enter");
            }
            other => panic!("{other:?}"),
        }
        let err = parse_wants("click --delivery", "x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("--delivery"), "{err}");
        let err = parse_wants("assert todo-item-1 foo=bar", "x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown assert field"), "{err}");
    }

    #[test]
    fn wants_wait_until_visible_and_in_viewport() {
        let recipe = parse_wants(
            "wait_until --timeout-ms 1000 --id todo-nav --visible false\nassert todo-nav visible=false in_viewport=false",
            "vis",
        )
        .unwrap();
        match &recipe.steps[0].op {
            Op::WaitUntil { timeout_ms, spec } => {
                assert_eq!(*timeout_ms, 1000);
                assert_eq!(spec.target, "todo-nav");
                assert_eq!(spec.visible, Some(false));
            }
            other => panic!("{other:?}"),
        }
        match &recipe.steps[1].op {
            Op::Assert { spec } => {
                assert_eq!(spec.visible, Some(false));
                assert_eq!(spec.in_viewport, Some(false));
            }
            other => panic!("{other:?}"),
        }
        let err = parse_wants("wait_until --id todo-nav --visible", "x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("timeout"), "{err}");
    }

    #[test]
    fn wants_screenshot_flag_and_op() {
        let recipe = parse_wants(
            "wait --screenshot\nscreenshot --out artifacts/steps/one.png\nclick --screenshot todo-add",
            "shots",
        )
        .unwrap();
        assert!(recipe.steps[0].screenshot);
        assert!(matches!(recipe.steps[0].op, Op::Wait { .. }));
        match &recipe.steps[1].op {
            Op::Screenshot { path, mode, .. } => {
                assert_eq!(path.as_deref(), Some("artifacts/steps/one.png"));
                assert!(mode.is_viewport());
            }
            other => panic!("{other:?}"),
        }
        assert!(!recipe.steps[1].screenshot);
        match &recipe.steps[2].op {
            Op::Click { target, .. } => assert_eq!(target, "todo-add"),
            other => panic!("{other:?}"),
        }
        assert!(recipe.steps[2].screenshot);
    }

    #[test]
    fn wants_screenshot_scrolled_mode_and_target() {
        let recipe = parse_wants(
            "screenshot --out tall.png --mode scrolled --target todo-list-scroll --max-height-px 4096",
            "tall",
        )
        .unwrap();
        match &recipe.steps[0].op {
            Op::Screenshot {
                path,
                mode,
                target,
                max_height_px,
                inline: false,
            } => {
                assert_eq!(path.as_deref(), Some("tall.png"));
                assert!(mode.is_scrolled());
                assert_eq!(target.as_deref(), Some("todo-list-scroll"));
                assert_eq!(*max_height_px, Some(4096));
            }
            other => panic!("{other:?}"),
        }
        let err = parse_wants("screenshot --out tall.png --mode scrolled", "x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("--target"), "{err}");
    }

    #[test]
    fn unicode_around_placeholders_substitutes() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "p",
            "params": ["title"],
            "steps": [
                {"id": "a", "op": "set_value", "target": "todo-input", "value": "café $title 🎉"}
            ]
        }"#,
        )
        .unwrap();
        validate_recipe(&recipe, &todo_registry()).unwrap();
        let mut set = BTreeMap::new();
        set.insert("title".into(), "Buy milk".into());
        let bound = apply_params(&recipe, &set).unwrap();
        match &bound.steps[0].op {
            Op::SetValue { value, .. } => assert_eq!(value, "café Buy milk 🎉"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn invoke_args_placeholders_substitute_without_json_roundtrip() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "p",
            "params": ["title"],
            "steps": [{"id": "a", "op": "invoke", "name": "todo.add", "args": {"title": "$title"}}]
        }"#,
        )
        .unwrap();
        validate_recipe(&recipe, &todo_registry()).unwrap();
        let mut set = BTreeMap::new();
        set.insert("title".into(), "Nested".into());
        let bound = apply_params(&recipe, &set).unwrap();
        match &bound.steps[0].op {
            Op::Invoke { args, .. } => assert_eq!(args["title"], "Nested"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wants_keybinding_and_list() {
        use gpui_agent::protocol::KeybindingScope;

        let recipe = parse_wants(
            "keybindings\nkeybinding --id todo.go_settings --scope global\nkeybinding --id app.quit --scope global --confirm --chord cmd-q",
            "kb",
        )
        .unwrap();
        assert!(matches!(recipe.steps[0].op, Op::Keybindings));
        match &recipe.steps[1].op {
            Op::Keybinding {
                binding,
                scope,
                confirm,
                activate,
                ..
            } => {
                assert_eq!(binding, "todo.go_settings");
                assert_eq!(*scope, KeybindingScope::Global);
                assert!(!confirm);
                assert!(!activate);
            }
            other => panic!("{other:?}"),
        }
        match &recipe.steps[2].op {
            Op::Keybinding {
                binding,
                chord,
                confirm,
                ..
            } => {
                assert_eq!(binding, "app.quit");
                assert_eq!(chord.as_deref(), Some("cmd-q"));
                assert!(confirm);
            }
            other => panic!("{other:?}"),
        }
        validate_recipe(&recipe, &todo_registry()).unwrap();
    }

    #[test]
    fn recipe_quit_keybinding_without_confirm_fails_validate() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "bad",
            "steps": [{"id": "q", "op": "keybinding", "binding": "app.quit", "scope": "global"}]
        }"#,
        )
        .unwrap();
        let err = validate_recipe(&recipe, &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("confirm=true"), "{err}");
    }

    fn code(json: &str) -> RecipeErrorCode {
        match Recipe::from_json(json) {
            Ok(recipe) => validate_recipe(&recipe, &todo_registry()).unwrap_err().code,
            Err(error) => error.code,
        }
    }

    #[test]
    fn unknown_fields_are_rejected() {
        use RecipeErrorCode as Code;
        let top = r#"{"name":"t","bogus":1,"steps":[{"id":"a","op":"snapshot"}]}"#;
        assert_eq!(code(top), Code::UnknownField);
        let step = r#"{"name":"t","steps":[{"id":"a","op":"snapshot"},
            {"id":"b","need":["a"],"op":"snapshot"}]}"#;
        let error = Recipe::from_json(step).unwrap_err();
        assert_eq!(error.code, Code::UnknownField);
        assert_eq!(error.step.as_deref(), Some("b"));
        let op = r#"{"name":"t","steps":[{"id":"a","op":"click","target":"x","tagret":"y"}]}"#;
        assert_eq!(code(op), Code::UnknownField);
    }

    #[test]
    fn every_serialized_op_field_is_known() {
        // Round-trip: fields the op itself writes must never be flagged.
        let recipe = parse_wants(
            "wait id=todo-input timeout=100\nset-value todo-input Hi\nclick todo-add\n\
             assert todo-item-1 name=Hi\ninvoke todo.add title=x\nsnapshot",
            "rt",
        )
        .unwrap();
        let json = serde_json::to_string(&recipe).unwrap();
        Recipe::from_json(&json).unwrap();
        let schema =
            r#"{"$schema":"./recipe.schema.json","name":"t","steps":[{"id":"a","op":"snapshot"}]}"#;
        Recipe::from_json(schema).unwrap();
    }

    #[test]
    fn validate_rejects_cycles_and_duplicate_needs() {
        use RecipeErrorCode as Code;
        let cycle = r#"{"name":"t","steps":[{"id":"a","needs":["b"],"op":"snapshot"},
            {"id":"b","needs":["a"],"op":"snapshot"}]}"#;
        assert_eq!(code(cycle), Code::Cycle);
        let dup = r#"{"name":"t","steps":[{"id":"a","op":"snapshot"},
            {"id":"b","needs":["a","a"],"op":"snapshot"}]}"#;
        assert_eq!(code(dup), Code::InvalidNeeds);
    }

    #[test]
    fn params_must_be_identifiers_unique_and_used() {
        use RecipeErrorCode as Code;
        for params in [r#"["a b"]"#, r#"["t","t"]"#, r#"["t","unused"]"#] {
            let json = format!(
                r#"{{"name":"t","params":{params},"steps":[{{"id":"a","op":"invoke","name":"todo.add","args":{{"title":"$t"}}}}]}}"#
            );
            assert_eq!(code(&json), Code::InvalidParam, "{params}");
        }
    }

    #[test]
    fn invoke_args_are_checked_against_the_schema() {
        use RecipeErrorCode as Code;
        for args in [r"{}", r#"{"title":1}"#, r#"{"title":"x","extra":1}"#] {
            let json = format!(
                r#"{{"name":"t","steps":[{{"id":"a","op":"invoke","name":"todo.add","args":{args}}}]}}"#
            );
            assert_eq!(code(&json), Code::InvalidArgs, "{args}");
        }
    }

    #[test]
    fn double_dollar_is_a_literal_dollar() {
        let json = r#"{"name":"t","params":["t"],"steps":[{"id":"a","op":"invoke",
            "name":"todo.add","args":{"title":"$t costs $$5, not $$t"}}]}"#;
        let recipe = Recipe::from_json(json).unwrap();
        validate_recipe(&recipe, &todo_registry()).unwrap();
        let set = BTreeMap::from([("t".to_string(), "Milk".to_string())]);
        let bound = apply_params(&recipe, &set).unwrap();
        let Op::Invoke { args, .. } = &bound.steps[0].op else {
            panic!("invoke");
        };
        assert_eq!(args["title"], "Milk costs $5, not $t");
    }

    #[test]
    fn set_must_match_declared_params() {
        use RecipeErrorCode as Code;
        let recipe = parse_wants("invoke todo.add title=$t", "p").unwrap();
        let missing = apply_params(&recipe, &BTreeMap::new()).unwrap_err();
        assert_eq!(missing.code, Code::MissingParam);
        let set = BTreeMap::from([
            ("t".to_string(), "x".to_string()),
            ("typo".to_string(), "y".to_string()),
        ]);
        assert_eq!(
            apply_params(&recipe, &set).unwrap_err().code,
            Code::UnknownParam
        );
    }

    #[test]
    fn lint_reports_every_violation() {
        let json = r#"{"v":9,"name":"t","steps":[{"id":"a","needs":["zz"],"op":"invoke",
            "name":"nope"}]}"#;
        let recipe = Recipe::from_json(json).unwrap();
        let codes: Vec<_> = lint_recipe(&recipe, &todo_registry())
            .into_iter()
            .map(|e| e.code.as_str())
            .collect();
        assert_eq!(
            codes,
            [
                "recipe_unsupported_version",
                "recipe_invalid_needs",
                "recipe_unknown_invoke"
            ]
        );
    }

    /// `schemas/recipe.v1.schema.json` must list exactly the keys each op
    /// reads, or editors and `recipe validate` disagree.
    #[test]
    fn json_schema_properties_match_the_parser() {
        // Read at run time: the file lives outside the published crate.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/recipe.v1.schema.json"
        );
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let schema: Value = serde_json::from_str(&text).unwrap();
        let variants = schema["$defs"]["step"]["oneOf"].as_array().unwrap();
        assert!(variants.len() >= 27, "{}", variants.len());
        for variant in variants {
            let op = variant["properties"]["op"]["const"].as_str().unwrap();
            let mut raw = serde_json::Map::new();
            for key in variant["required"].as_array().unwrap() {
                let key = key.as_str().unwrap();
                let sample = match (op, key) {
                    (_, "op") => json!(op),
                    ("keybinding", "scope") => json!("global"),
                    ("swipe", "direction") => json!("up"),
                    ("system", "action") => json!("back"),
                    (_, "timeout_ms" | "index" | "scale") => json!(1),
                    _ => json!("x"),
                };
                raw.insert(key.into(), sample);
            }
            let raw = Value::Object(raw);
            let step = RecipeStep::deserialize(&raw)
                .unwrap_or_else(|e| panic!("schema minimum for `{op}` does not parse: {e}"));
            assert_eq!(unknown_step_field(&raw, &step), None, "{op}");
            for key in variant["properties"].as_object().unwrap().keys() {
                let mut with = raw.as_object().unwrap().clone();
                with.entry(key.clone()).or_insert(Value::Null);
                assert_eq!(
                    unknown_step_field(&Value::Object(with), &step),
                    None,
                    "schema lists `{key}` for `{op}` but the parser ignores it"
                );
            }
        }
    }
    fn typed_recipe() -> Recipe {
        Recipe::from_value(&serde_json::json!({
            "name": "typed",
            "params": [
                "title",
                {"name": "count", "type": "integer", "default": 2},
                {"name": "kind", "enum": ["a", "b"], "description": "Which kind"},
                {"name": "amount", "pattern": "^[0-9]+\\.00$", "required": false},
            ],
            "steps": [{
                "id": "s1", "op": "invoke", "name": "todo.add",
                "args": {"title": "$title", "count": "$count", "kind": "$kind", "amount": "$amount"},
            }],
        }))
        .unwrap()
    }

    fn bind(recipe: &Recipe, pairs: &[(&str, &str)]) -> Result<Recipe, RecipeError> {
        let set = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        apply_params(recipe, &set)
    }

    fn invoke_args(recipe: &Recipe) -> Value {
        match &recipe.steps[0].op {
            Op::Invoke { args, .. } => args.clone(),
            other => panic!("not an invoke: {other:?}"),
        }
    }

    #[test]
    fn typed_params_default_omit_and_reject() {
        let recipe = typed_recipe();
        let bound = bind(&recipe, &[("title", "t"), ("kind", "a")]).unwrap();
        assert_eq!(
            invoke_args(&bound),
            serde_json::json!({"title": "t", "count": "2", "kind": "a"}),
            "default applied, absent optional arg omitted"
        );
        let full = bind(
            &recipe,
            &[("title", "t"), ("kind", "b"), ("amount", "10.00")],
        )
        .unwrap();
        assert_eq!(invoke_args(&full)["amount"], "10.00");

        for (pairs, code) in [
            (vec![("title", "t")], RecipeErrorCode::MissingParam),
            (
                vec![("title", "t"), ("kind", "c")],
                RecipeErrorCode::InvalidParamValue,
            ),
            (
                vec![("title", "t"), ("kind", "a"), ("count", "x")],
                RecipeErrorCode::InvalidParamValue,
            ),
            (
                vec![("title", "t"), ("kind", "a"), ("amount", "10.50")],
                RecipeErrorCode::InvalidParamValue,
            ),
        ] {
            assert_eq!(bind(&recipe, &pairs).unwrap_err().code, code, "{pairs:?}");
        }
    }

    #[test]
    fn typed_params_round_trip_and_input_schema() {
        let recipe = typed_recipe();
        let again = Recipe::from_value(&serde_json::to_value(&recipe).unwrap()).unwrap();
        assert_eq!(again.params, recipe.params);
        assert_eq!(
            serde_json::to_value(&recipe.params[0]).unwrap(),
            serde_json::json!("title"),
            "a plain param stays a bare name"
        );
        let odd: Param = serde_json::from_value(serde_json::json!(
            {"name": "n", "type": "number", "default": "1e2", "enum": ["1e2", 5]}
        ))
        .unwrap();
        let odd_again: Param = serde_json::from_value(serde_json::to_value(&odd).unwrap()).unwrap();
        assert_eq!(odd_again, odd, "numeric text survives a round trip");
        let schema = recipe.input_schema();
        assert_eq!(schema["required"], serde_json::json!(["title", "kind"]));
        assert_eq!(
            schema["properties"]["count"],
            serde_json::json!({"type": "integer", "default": 2})
        );
        assert_eq!(
            schema["properties"]["kind"]["enum"],
            serde_json::json!(["a", "b"])
        );
    }

    #[test]
    fn typed_param_declarations_are_linted() {
        let lint = |params: Value, args: Value| {
            let recipe = Recipe::from_value(&serde_json::json!({
                "name": "bad", "params": params,
                "steps": [{"id": "s1", "op": "invoke", "name": "todo.add", "args": args}],
            }))
            .unwrap();
            lint_recipe(&recipe, &crate::registry::todo_registry())
                .into_iter()
                .map(|e| (e.code, e.message))
                .collect::<Vec<_>>()
        };
        for (params, args, want) in [
            (
                serde_json::json!([{"name": "title", "type": "integer", "default": "x"}]),
                serde_json::json!({"title": "$title"}),
                "default of",
            ),
            (
                serde_json::json!([{"name": "title", "required": true, "default": "x"}]),
                serde_json::json!({"title": "$title"}),
                "required but has a default",
            ),
            (
                serde_json::json!([{"name": "title", "pattern": "("}]),
                serde_json::json!({"title": "$title"}),
                "invalid pattern",
            ),
            (
                serde_json::json!([{"name": "title", "type": "integer", "pattern": "^[1-9]$"}]),
                serde_json::json!({"title": "$title"}),
                "pattern is for string params",
            ),
            (
                serde_json::json!([{"name": "title", "required": false}]),
                serde_json::json!({"title": "Buy $title"}),
                "can only be a whole invoke arg value",
            ),
        ] {
            let found = lint(params, args);
            assert!(
                found
                    .iter()
                    .any(|(code, message)| *code == RecipeErrorCode::InvalidParam
                        && message.contains(want)),
                "want `{want}` in {found:?}"
            );
        }
        for bad in [
            serde_json::json!([{"name": "t", "typo": 1}]),
            serde_json::json!([{"name": "t", "type": "date"}]),
            serde_json::json!([7]),
        ] {
            let error = Recipe::from_value(&serde_json::json!({
                "name": "bad", "params": bad,
                "steps": [{"id": "s1", "op": "hello"}],
            }))
            .unwrap_err();
            assert_eq!(error.code, RecipeErrorCode::Parse);
        }
    }

    /// A key written twice is refused: the earlier value would be dropped
    /// without a word, and a reader of the file sees both.
    #[test]
    fn duplicate_json_keys_are_rejected() {
        for (text, key) in [
            (
                r#"{"name":"d","steps":[{"id":"a","op":"hello"}],"steps":[{"id":"b","op":"hello"}]}"#,
                "steps",
            ),
            (
                r#"{"name":"d","steps":[{"id":"a","op":"hello","op":"shutdown"}]}"#,
                "op",
            ),
            (
                r#"{"name":"d","steps":[{"id":"a","op":"invoke","name":"todo.add","args":{"title":"a","title":"b"}}]}"#,
                "title",
            ),
        ] {
            for parsed in [
                Recipe::from_json(text),
                parse_recipe_source(text, "inline", false),
            ] {
                let error = parsed.expect_err(text);
                assert_eq!(error.code, RecipeErrorCode::Parse, "{error}");
                assert!(
                    error
                        .to_string()
                        .contains(&format!("duplicate field `{key}`")),
                    "{error}"
                );
            }
        }
    }

    /// `required` may name an arg that has no `args` entry (`target`,
    /// `text`, …): it is declared, only untyped.
    #[test]
    fn an_arg_named_only_in_required_is_declared() {
        let schema: OpSchema = serde_json::from_value(json!({
            "name": "x.press",
            "kind": "invoke",
            "description": "press",
            "required": ["target"],
            "args": {"label": {"type": "string"}},
        }))
        .unwrap();
        schema.validate().unwrap();
        for bound in [false, true] {
            check_invoke_args(
                &schema,
                &json!({"target": "button", "label": "Save"}),
                bound,
            )
            .unwrap();
            let missing = check_invoke_args(&schema, &json!({"label": "Save"}), bound).unwrap_err();
            assert!(
                missing.contains("missing required arg `target`"),
                "{missing}"
            );
            let unknown =
                check_invoke_args(&schema, &json!({"target": "b", "other": 1}), bound).unwrap_err();
            assert!(unknown.contains("no arg `other`"), "{unknown}");
            assert!(unknown.contains("label, target"), "{unknown}");
        }
    }

    /// One bad `needs` entry does not hide a cycle elsewhere, and is not
    /// itself reported as one.
    #[test]
    fn a_bad_need_does_not_hide_a_cycle() {
        use RecipeErrorCode as Code;
        let codes = |text: &str| -> Vec<Code> {
            lint_recipe(&Recipe::from_json(text).unwrap(), &todo_registry())
                .iter()
                .map(|error| error.code)
                .collect()
        };
        let both = r#"{"name":"t","steps":[
            {"id":"a","needs":["b"],"op":"snapshot"},
            {"id":"b","needs":["a"],"op":"snapshot"},
            {"id":"c","needs":["nope"],"op":"snapshot"}]}"#;
        assert_eq!(codes(both), [Code::InvalidNeeds, Code::Cycle]);
        let only_bad = r#"{"name":"t","steps":[
            {"id":"a","needs":["nope","a"],"op":"snapshot"},
            {"id":"b","needs":["a","a"],"op":"snapshot"}]}"#;
        assert!(
            !codes(only_bad).contains(&Code::Cycle),
            "{:?}",
            codes(only_bad)
        );
    }

    /// Every step the crate writes passes `schemas/recipe.v1.schema.json`:
    /// a key written as `null` is one whose schema type admits `null`.
    #[test]
    fn json_schema_accepts_what_the_serializer_writes() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/recipe.v1.schema.json"
        );
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let schema: Value = serde_json::from_str(&text).unwrap();
        for variant in schema["$defs"]["step"]["oneOf"].as_array().unwrap() {
            let op = variant["properties"]["op"]["const"].as_str().unwrap();
            let mut raw = serde_json::Map::new();
            for key in variant["required"].as_array().unwrap() {
                let key = key.as_str().unwrap();
                let sample = match (op, key) {
                    (_, "op") => json!(op),
                    ("keybinding", "scope") => json!("global"),
                    ("swipe", "direction") => json!("up"),
                    ("system", "action") => json!("back"),
                    (_, "timeout_ms" | "index" | "scale") => json!(1),
                    _ => json!("x"),
                };
                raw.insert(key.into(), sample);
            }
            let step = RecipeStep::deserialize(&Value::Object(raw)).unwrap();
            let written = serde_json::to_value(&step).unwrap();
            for (key, value) in written.as_object().unwrap() {
                let property = &variant["properties"][key];
                assert!(
                    !property.is_null(),
                    "`{op}` writes `{key}`, not in the schema"
                );
                let admits_null = match &property["type"] {
                    Value::Null => true,
                    Value::String(ty) => ty == "null",
                    Value::Array(types) => types.iter().any(|ty| ty == "null"),
                    _ => false,
                };
                assert!(
                    !value.is_null() || admits_null,
                    "`{op}` writes `{key}: null`, schema type is {}",
                    property["type"]
                );
            }
        }
    }

    /// A number param's `enum` is compared as numbers, as the input schema
    /// a caller validates against compares it; a string `enum` stays text.
    #[test]
    fn a_numeric_enum_binds_what_its_input_schema_accepts() {
        let recipe = Recipe::from_value(&json!({
            "name": "n",
            "params": [
                {"name": "n", "type": "number", "enum": [1.0, 5]},
                {"name": "m", "type": "number", "enum": ["1e2"], "default": "1e2"},
                {"name": "i", "type": "integer", "enum": [9_007_199_254_740_993_i64]},
                {"name": "s", "enum": ["1.0"]},
            ],
            "steps": [{
                "id": "s1", "op": "invoke", "name": "todo.add",
                "args": {"n": "$n", "m": "$m", "i": "$i", "s": "$s"},
            }],
        }))
        .unwrap();
        let check = |name: &str, value: &str| recipe.param(name).unwrap().check(value);
        for value in ["1", "1.0", "1e0", "5", "5.0"] {
            check("n", value).unwrap();
        }
        check("n", "2").unwrap_err();
        for value in ["100", "1e2", "100.0"] {
            check("m", value).unwrap();
        }
        check("i", "9007199254740993").unwrap();
        check("i", "9007199254740992").unwrap_err();
        check("s", "1.0").unwrap();
        check("s", "1").unwrap_err();
        let properties = &recipe.input_schema()["properties"];
        assert_eq!(properties["n"]["enum"], json!([1.0, 5]));
        assert_eq!(properties["m"]["enum"], json!([100.0]));
        assert_eq!(properties["m"]["default"], json!(100.0));
    }

    /// The input schema's `pattern` is read by ECMAScript validators, so it
    /// is written the way they read it: `.`, `\s` and `\S` as the sets this
    /// crate's engine means. A pattern with no such spelling is left out
    /// and still checked when the value is bound.
    #[test]
    fn input_schema_exports_patterns_as_javascript_reads_them() {
        let exported = |pattern: &str| -> Option<String> {
            let recipe = Recipe::from_value(&json!({
                "name": "p",
                "params": [{"name": "p", "pattern": pattern}],
                "steps": [{
                    "id": "s1", "op": "invoke", "name": "todo.add", "args": {"title": "$p"},
                }],
            }))
            .unwrap();
            validate_recipe(&recipe, &todo_registry()).expect(pattern);
            recipe.input_schema()["properties"]["p"]["pattern"]
                .as_str()
                .map(str::to_string)
        };
        for (pattern, written) in [
            (r"^[0-9]+\.00$", r"^[0-9]+\.00$"),
            (r"^(?:yes|no)$", r"^(?:yes|no)$"),
            (r"^(?<year>\d{4})-\d\d$", r"^(?<year>\d{4})-\d\d$"),
            (r"^[\w\-.]+@\w+$", r"^[\w\-.]+@\w+$"),
            (r"\x41\t", r"\x41\t"),
            (r"^\S+$", r"^[^\t\n\x0B\f\r ]+$"),
            (r"^a\s*b$", r"^a[\t\n\x0B\f\r ]*b$"),
            (r"^[a-z\s]+$", r"^[a-z\t\n\x0B\f\r ]+$"),
            (r"^.+\.$", r"^[^\n]+\.$"),
            (r"^[.]$", r"^[.]$"),
            (r"^[a-z0-9_-]+$", r"^[a-z0-9_-]+$"),
            (r"^[-a-z\s]+$", r"^[-a-z\t\n\x0B\f\r ]+$"),
            (r"^[^-a\x2Dz]$", r"^[^-a\x2Dz]$"),
        ] {
            assert_eq!(exported(pattern).as_deref(), Some(written), "{pattern}");
            // Same pattern to this crate's engine, spelled either way.
            for value in [
                "",
                "a",
                "a b",
                "a\tb",
                "a\u{b}b",
                "a\u{c}b",
                "a\u{a0}b",
                "a\u{2028}b",
                "a\rb",
                "a\nb",
                "ab.",
                "2024-01",
                "x@y",
                " ",
                "\u{a0}",
                "\u{3000}",
                "\r",
                "\n",
            ] {
                let matches = |text: &str| regex_lite::Regex::new(text).unwrap().is_match(value);
                assert_eq!(matches(pattern), matches(written), "{pattern} on {value:?}");
            }
        }
        for pattern in [
            "(?i)^yes$",
            "^(?i:yes)$",
            r"(?P<year>\d{4})",
            r"\Ayes\z",
            "[[:alpha:]]+",
            r"\x{41}",
            r"a\-b",
            r"^[^\S]+$",
            // A hyphen in a class is a range only between two single
            // characters; ECMAScript refuses or reads the rest differently.
            r"^[--\s]+$",
            r"^[--\d]+$",
            r"^[a-z-9]+$",
            r"^[--]+$",
        ] {
            assert_eq!(exported(pattern), None, "{pattern}");
        }
        // What the engines disagree on, and why the sets are spelled out:
        // a no-break space is not `\s` here, and `.` takes a carriage return.
        let spaces = Param::named("p");
        let check = |pattern: &str, value: &str| {
            let mut param = spaces.clone();
            param.pattern = Some(pattern.to_string());
            param.check(value).is_ok()
        };
        assert!(check(r"^\S+$", "a\u{a0}b"));
        assert!(check("^.+$", "a\rb"));
        assert!(check("(?i)^yes$", "YES"));
        assert!(!check("(?i)^yes$", "no"));
    }

    /// `args: {}` says the invoke takes none; leaving `args` out says
    /// nothing about them.
    #[test]
    fn a_schema_that_lists_no_args_takes_none() {
        let schema = |args: Value| -> OpSchema {
            let mut row = json!({"name": "x.list", "kind": "invoke", "description": "list"});
            if !args.is_null() {
                row["args"] = args;
            }
            let schema: OpSchema = serde_json::from_value(row).unwrap();
            schema.validate().unwrap();
            schema
        };
        let closed = schema(json!({}));
        check_invoke_args(&closed, &Value::Null, true).unwrap();
        check_invoke_args(&closed, &json!({}), true).unwrap();
        let extra = check_invoke_args(&closed, &json!({"limit": 1}), true).unwrap_err();
        assert!(extra.contains("no arg `limit`"), "{extra}");
        check_invoke_args(&closed, &json!("text"), true).unwrap_err();
        // Written back as it was read, so the file keeps saying so.
        assert_eq!(serde_json::to_value(&closed).unwrap()["args"], json!({}));

        // `null` is neither: a schema that writes `args` writes a map.
        let null = serde_json::from_value::<OpSchema>(json!({
            "name": "x.list", "kind": "invoke", "description": "list", "args": null,
        }));
        assert!(null.is_err(), "{null:?}");

        let open = schema(Value::Null);
        check_invoke_args(&open, &json!({"limit": 1}), true).unwrap();
        check_invoke_args(&open, &json!("text"), true).unwrap();
        assert!(serde_json::to_value(&open).unwrap().get("args").is_none());

        // The sample app's list takes no args.
        let recipe = Recipe::from_json(
            r#"{"name":"t","steps":[{"id":"a","op":"invoke","name":"todo.list","args":{"x":1}}]}"#,
        )
        .unwrap();
        let error = validate_recipe(&recipe, &todo_registry()).unwrap_err();
        assert_eq!(error.code, RecipeErrorCode::InvalidArgs, "{error}");
    }
}
