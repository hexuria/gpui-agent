use std::fmt;

use serde::Serialize;

/// Stable classification of a [`RecipeError`].
///
/// The [`as_str`](Self::as_str) spellings are part of the stable recipe
/// API: they appear as `error_code` in CLI and MCP failure envelopes and
/// are never renamed. New codes may be added in a minor release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RecipeErrorCode {
    /// A recipe or schema file could not be read.
    Io,
    /// Recipe text is not valid JSON / `.wants`, or a field has the wrong type.
    Parse,
    /// The recipe's `v` is not a supported format version.
    UnsupportedVersion,
    /// A recipe-level invariant is broken (empty name, no steps, too many steps).
    InvalidRecipe,
    /// A JSON key that is not part of the recipe format or the step's op.
    UnknownField,
    /// A step id is empty, duplicated, or contains a `$placeholder`.
    InvalidStep,
    /// A `needs` entry is unknown, duplicated, self-referential, or a `$placeholder`.
    InvalidNeeds,
    /// `needs` edges form a cycle.
    Cycle,
    /// `params` has an invalid, duplicate, or unused name, or a step uses
    /// an undeclared `$placeholder`.
    InvalidParam,
    /// A declared param has no bound value.
    MissingParam,
    /// A bound value names a param the recipe does not declare.
    UnknownParam,
    /// A bound value cannot be converted to the type the invoke schema declares.
    InvalidParamValue,
    /// `invoke` names a schema that is not registered as an invoke.
    UnknownInvoke,
    /// `invoke` args do not match the schema (missing, unknown, or wrong type).
    InvalidArgs,
    /// A dangerous op lacks its explicit confirmation (quit keybinding without `confirm`).
    Unconfirmed,
    /// A schema file or schema row is invalid, or two schemas share a name.
    InvalidSchema,
    /// An intent could not be resolved to exactly one schema-backed op.
    Unresolved,
}

impl RecipeErrorCode {
    /// The stable wire spelling (`recipe_cycle`, `recipe_unknown_field`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Io => "recipe_io",
            Self::Parse => "recipe_parse",
            Self::UnsupportedVersion => "recipe_unsupported_version",
            Self::InvalidRecipe => "recipe_invalid",
            Self::UnknownField => "recipe_unknown_field",
            Self::InvalidStep => "recipe_invalid_step",
            Self::InvalidNeeds => "recipe_invalid_needs",
            Self::Cycle => "recipe_cycle",
            Self::InvalidParam => "recipe_invalid_param",
            Self::MissingParam => "recipe_missing_param",
            Self::UnknownParam => "recipe_unknown_param",
            Self::InvalidParamValue => "recipe_invalid_param_value",
            Self::UnknownInvoke => "recipe_unknown_invoke",
            Self::InvalidArgs => "recipe_invalid_args",
            Self::Unconfirmed => "recipe_unconfirmed",
            Self::InvalidSchema => "recipe_invalid_schema",
            Self::Unresolved => "recipe_unresolved",
        }
    }
}

impl fmt::Display for RecipeErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for RecipeErrorCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Why a recipe, schema, or intent was rejected before anything ran.
///
/// `Display` is the human message; match on [`code`](Self::code), not on
/// the text. Serializes as `{"error_code", "error", "step"?}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct RecipeError {
    /// Stable classification.
    #[serde(rename = "error_code")]
    pub code: RecipeErrorCode,
    /// Human-readable message; wording is not part of the stable API.
    #[serde(rename = "error")]
    pub message: String,
    /// Id of the offending step, when the error is about one step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
}

impl RecipeError {
    /// An error with `code` and a human `message`.
    pub fn new(code: RecipeErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            step: None,
        }
    }

    /// Attach the id of the step the error is about.
    #[must_use]
    pub fn at_step(mut self, step: impl Into<String>) -> Self {
        self.step = Some(step.into());
        self
    }

    /// `{"ok":false,"error_code":…,"error":…,"step"?}` — the failure
    /// envelope the CLI prints and MCP tool errors carry.
    ///
    /// # Panics
    ///
    /// Never: [`RecipeError`] serialization cannot fail.
    pub fn envelope(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("RecipeError serializes");
        if let serde_json::Value::Object(map) = &mut value {
            map.insert("ok".into(), serde_json::Value::Bool(false));
        }
        value
    }
}

impl fmt::Display for RecipeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RecipeError {}

pub(crate) fn err(code: RecipeErrorCode, message: impl Into<String>) -> RecipeError {
    RecipeError::new(code, message)
}
