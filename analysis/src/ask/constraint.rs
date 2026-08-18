//! Building the JSON-schema constraint mistral.rs compiles into an llguidance
//! grammar.
//!
//! `schema_for!(T)` gives the schema; `inject_x_guidance` adds the one override
//! that shapes generation. schemars orders object properties alphabetically, so
//! the `_type` tag (`_` sorts below the letters) already leads each variant and
//! the model commits to a variant before emitting its payload, no reordering.

use schemars::JsonSchema;
use serde_json::{Value, json};
use thiserror::Error;

/// The `x-guidance` root key llguidance reads its per-schema overrides from.
const X_GUIDANCE_KEY: &str = "x-guidance";

/// Why a schema type could not be turned into a constraint value.
#[derive(Debug, Error)]
pub enum ConstraintError {
    /// The derived schema did not serialize to a JSON value. Structural and not
    /// reachable for the ask's own types; carried so the boundary stays total.
    #[error("the derived schema could not be serialized to a JSON value")]
    Serialize(#[source] serde_json::Error),
}

/// Builds the constraint value for `T`: its schema with `x-guidance` injected.
pub fn constraint_value<T: JsonSchema>() -> Result<Value, ConstraintError> {
    let mut value =
        serde_json::to_value(schemars::schema_for!(T)).map_err(ConstraintError::Serialize)?;
    inject_x_guidance(&mut value);
    Ok(value)
}

/// Inserts the `x-guidance` override at the root object llguidance reads its
/// per-schema options from. A non-object root (which our schema types never
/// produce) is left untouched.
///
/// `whitespace_flexible: false` forbids whitespace between tokens, so the model
/// emits compact JSON with no indentation, holding the token count down. The
/// separators keep their spaces, though: forcing fully compact JSON (a bare `:`)
/// collapsed Qwen's string generation into garbage in testing, and the standard
/// `": "` / `", "` separators are the combination that produced correct output.
/// We did not root-cause why. Number values (the SAM box prompts) survived the
/// bare separator; string values did not, and pass 2 is all strings.
fn inject_x_guidance(value: &mut Value) {
    if let Value::Object(root) = value {
        root.insert(
            X_GUIDANCE_KEY.to_owned(),
            json!({ "whitespace_flexible": false, "key_separator": ": ", "item_separator": ", " }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::{CompositeOutcome, RelevanceOutcome};
    use super::*;

    /// The internal discriminant key the ask's tagged enums carry.
    const TAG_KEY: &str = "_type";

    /// Counts the `properties` maps that carry the tag, asserting each lists it
    /// first so the model commits to a variant before emitting its payload.
    /// Returning the count lets the caller check the tag was actually found, so a
    /// schemars shape change that moved it out of `properties` fails loudly rather
    /// than passing vacuously.
    fn count_tag_leads(value: &Value) -> usize {
        let mut found = 0;
        match value {
            Value::Object(map) => {
                if let Some(Value::Object(properties)) = map.get("properties")
                    && properties.contains_key(TAG_KEY)
                {
                    let first = properties.keys().next().map(String::as_str);
                    assert_eq!(
                        first,
                        Some(TAG_KEY),
                        "tag `{TAG_KEY}` is not first in {properties:?}"
                    );
                    found += 1;
                }
                for child in map.values() {
                    found += count_tag_leads(child);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| found += count_tag_leads(item)),
            _ => {}
        }
        found
    }

    #[test]
    fn tag_leads_every_variant() -> Result<(), ConstraintError> {
        // Alphabetical ordering puts `_type` first with no reordering; assert the
        // count so a schemars shape change that moved the tag out of `properties`
        // fails loudly instead of passing vacuously.
        assert_eq!(count_tag_leads(&constraint_value::<CompositeOutcome>()?), 2);
        assert_eq!(count_tag_leads(&constraint_value::<RelevanceOutcome>()?), 2);
        Ok(())
    }
}
