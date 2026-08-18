//! Rendering the ask schema as compact BAML-style type text for the prompt.
//!
//! The prompt-assembly step places this ahead of the constrained decode, to
//! prime the model with the shape and narrow the gap where llguidance forcing an
//! unexpected token degrades adherence. `Qwen3::ask` takes a caller-assembled
//! prompt, so wiring this into it is that step's job, not done yet. It renders
//! from the **same** schema value the constraint compiles (`schema_for!(T)`), so
//! a field rename or reorder cannot desync what the model is told from what it is
//! held to. JSON Schema is the wrong text for that job: verbose, its boilerplate
//! read as signal, so this is the compact syntax the POC used. It reads whatever
//! draft schemars emits: the workspace 0.8's draft-07 (`definitions`, a
//! single-value `enum` for a variant tag) as well as 1's draft-2020-12 (`$defs`,
//! `const`).
//!
//! Per-node guidance the model reads comes from an `x-vlm` extension key, set by
//! `#[schemars(extend("x-vlm" = "..."))]` on a type, field, or variant, and never
//! from `///` doc comments: developer docs stay developer-facing, and the prompt
//! shows only what an `x-vlm` deliberately put there. A node without one renders
//! its shape with no `@description`. `x-vlm` rides the schema untouched (llguidance
//! passes vendor `x-` keys through), so the render and the constraint share the one
//! value.
//!
//! The constraint is a bare `serde_json::Value` (what mistral.rs compiles), so
//! the walk parses it into a typed `SchemaNode` once and renders from its fields
//! rather than sniffing keys and downcasting at every step. An unrecognized shape
//! renders as `any` rather than aborting; only a value that is not a JSON Schema
//! at all is an error, since our own types always parse and a failure there is a
//! defect.

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

/// The subset of JSON Schema the ask types produce, parsed from the schema
/// `Value` so the render reads typed fields. Unknown keys are ignored: schemars
/// emits more than the prompt needs.
///
/// `properties`, `$defs`, and `definitions` are [`IndexMap`]s so the object order
/// the schema wrote survives deserialize; schemars orders alphabetically, so
/// `_type` leads each variant and the render matches the order the grammar
/// enforces.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SchemaNode {
    #[serde(rename = "$ref")]
    reference: Option<String>,
    #[serde(rename = "$defs")]
    defs: IndexMap<String, SchemaNode>,
    definitions: IndexMap<String, SchemaNode>,
    title: Option<String>,
    #[serde(rename = "x-vlm")]
    vlm: Option<String>,
    #[serde(rename = "type")]
    ty: Option<TypeField>,
    properties: IndexMap<String, SchemaNode>,
    required: Vec<String>,
    #[serde(rename = "oneOf")]
    one_of: Vec<SchemaNode>,
    #[serde(rename = "anyOf")]
    any_of: Vec<SchemaNode>,
    #[serde(rename = "allOf")]
    all_of: Vec<SchemaNode>,
    #[serde(rename = "enum")]
    enum_values: Vec<Value>,
    #[serde(rename = "const")]
    const_value: Option<Value>,
    items: Option<Box<SchemaNode>>,
    #[serde(rename = "prefixItems")]
    prefix_items: Vec<SchemaNode>,
    discriminator: Option<Discriminator>,
}

/// A schema's `type`: one name, or a `["T", "null"]` pair for an optional scalar.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TypeField {
    Single(String),
    Multiple(Vec<String>),
}

/// An explicit `discriminator`, when a schema names its tag field outright rather
/// than leaving it to be inferred as the common single-const property.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Discriminator {
    #[serde(rename = "propertyName")]
    property_name: Option<String>,
}

impl SchemaNode {
    /// The last path segment of this node's `$ref`, e.g. `#/$defs/Entity` ->
    /// `Entity`.
    fn ref_name(&self) -> Option<&str> {
        self.reference
            .as_deref()
            .and_then(|reference| reference.rsplit('/').next())
    }

    /// The scalar `type` name, when this node names exactly one.
    fn type_str(&self) -> Option<&str> {
        match &self.ty {
            Some(TypeField::Single(name)) => Some(name),
            _ => None,
        }
    }

    /// Whether this node is the `{"type": "null"}` branch of an optional.
    fn is_null(&self) -> bool {
        self.type_str() == Some("null")
    }

    /// The constant string this node names, from `const` or a one-value `enum`.
    /// Variant tags are strings, so a non-string constant reads as absent.
    fn single_const(&self) -> Option<String> {
        if let Some(value) = &self.const_value {
            return value.as_str().map(str::to_owned);
        }
        if let [only] = self.enum_values.as_slice() {
            return only.as_str().map(str::to_owned);
        }
        None
    }

    /// Whether this node is a single constant (a tagged union's discriminant, or
    /// one arm of a tag-less enum).
    fn is_single_const(&self) -> bool {
        self.single_const().is_some()
    }

    /// A `type: string` with a multi-value `enum` — schemars' description-less
    /// bare-enum shape (an enum whose variants carry docs takes the `oneOf` form).
    fn is_bare_enum(&self) -> bool {
        self.type_str() == Some("string") && self.enum_values.len() > 1
    }

    /// Whether every `oneOf` variant is a single constant, i.e. a tag-less enum.
    fn variants_all_single_const(&self) -> bool {
        !self.one_of.is_empty() && self.one_of.iter().all(SchemaNode::is_single_const)
    }

    /// This node's one-line, escaped `x-vlm` guidance, if it carries any. The
    /// dedicated channel is what keeps developer `///` docs out of the prompt: the
    /// render reads only what an `x-vlm` annotation put here.
    fn vlm_text(&self) -> Option<String> {
        self.vlm.as_deref().map(|text| {
            text.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace(['\n', '\r', '\t'], " ")
        })
    }
}

/// Why a schema value could not be rendered.
#[derive(Debug, Error)]
pub enum RenderError {
    /// The schema value is not a JSON Schema the renderer can parse. Our own
    /// types always parse, so this is a defect worth surfacing rather than hiding
    /// behind an empty hint.
    #[error("the schema value is not a JSON Schema this renderer understands")]
    Parse(#[source] serde_json::Error),
}

/// Renders a schema value as BAML-style type definitions.
pub fn render_schema(schema: &Value) -> Result<String, RenderError> {
    let root = serde_json::from_value::<SchemaNode>(schema.clone()).map_err(RenderError::Parse)?;
    // schemars 1 writes `$defs`; schemars 0.8 writes `definitions`.
    let defs = if root.defs.is_empty() {
        &root.definitions
    } else {
        &root.defs
    };
    let renderer = Renderer { defs };

    let mut blocks: Vec<String> = Vec::new();
    for (name, spec) in defs {
        if let Some(block) = renderer.render_def(name, spec) {
            blocks.push(block);
        }
    }
    if let Some(root) = renderer.render_root(&root) {
        blocks.push(root);
    }
    Ok(blocks.join("\n\n"))
}

/// The title a root schema renders under when it names none.
const ROOT_NAME: &str = "Output";

struct Renderer<'a> {
    defs: &'a IndexMap<String, SchemaNode>,
}

impl Renderer<'_> {
    /// Resolves a `$ref` to its definition, or returns the node unchanged.
    fn deref<'b>(&'b self, spec: &'b SchemaNode) -> &'b SchemaNode {
        spec.ref_name()
            .and_then(|name| self.defs.get(name))
            .unwrap_or(spec)
    }

    /// The BAML type expression for a property node.
    fn resolve_type(&self, spec: &SchemaNode) -> String {
        if let Some(name) = spec.ref_name() {
            return name.to_owned();
        }
        // `allOf: [x]` is how schemars wraps a single subschema; unwrap it.
        if let [only] = spec.all_of.as_slice() {
            return self.resolve_type(only);
        }
        if !spec.one_of.is_empty() {
            if spec.one_of.iter().all(SchemaNode::is_single_const) {
                return "string".to_owned();
            }
            return "any".to_owned();
        }
        // `anyOf: [T, {"type": "null"}]` is a common optional-`T` shape: render
        // the non-null branch and mark it optional.
        if !spec.any_of.is_empty() {
            let non_null: Vec<&SchemaNode> = spec
                .any_of
                .iter()
                .filter(|branch| !branch.is_null())
                .collect();
            if let [only] = non_null.as_slice() {
                let mut ty = self.resolve_type(only);
                if !ty.ends_with('?') {
                    ty.push('?');
                }
                return ty;
            }
            return "any".to_owned();
        }
        // A `["T", "null"]` type is an optional scalar T.
        if let Some(TypeField::Multiple(types)) = &spec.ty {
            let non_null: Vec<&str> = types
                .iter()
                .map(String::as_str)
                .filter(|name| *name != "null")
                .collect();
            if let [single] = non_null.as_slice() {
                return format!("{}?", scalar(single));
            }
            return "any".to_owned();
        }
        if spec.type_str() == Some("array") {
            let item = spec
                .items
                .as_deref()
                .or_else(|| spec.prefix_items.first())
                .map(|items| self.resolve_type(items))
                .unwrap_or_else(|| "any".to_owned());
            return format!("{item}[]");
        }
        scalar(spec.type_str().unwrap_or("any")).to_owned()
    }

    /// Renders one `$defs` entry, or `None` for a shape that carries no prompt
    /// value (a plain scalar alias).
    fn render_def(&self, name: &str, spec: &SchemaNode) -> Option<String> {
        if spec.is_bare_enum() {
            return Some(render_enum(name, spec));
        }
        if !spec.one_of.is_empty() {
            if spec.variants_all_single_const() {
                return Some(render_enum(name, spec));
            }
            if let Some(tag) = self.find_tag_field(spec) {
                return Some(self.render_tagged_union(name, spec, &tag));
            }
        }
        if spec.type_str() == Some("object") || !spec.properties.is_empty() {
            return Some(self.render_class(name, spec));
        }
        None
    }

    /// Renders the root schema when it carries structure directly (some schemas
    /// inline the root rather than referencing a def).
    fn render_root(&self, spec: &SchemaNode) -> Option<String> {
        let name = spec.title.as_deref().unwrap_or(ROOT_NAME);
        if !spec.properties.is_empty() {
            return Some(self.render_class(name, spec));
        }
        if !spec.one_of.is_empty() {
            if let Some(tag) = self.find_tag_field(spec) {
                return Some(self.render_tagged_union(name, spec, &tag));
            }
            if spec.variants_all_single_const() {
                return Some(render_enum(name, spec));
            }
        }
        None
    }

    fn render_class(&self, name: &str, spec: &SchemaNode) -> String {
        let mut lines = Vec::new();
        if let Some(guidance) = spec.vlm_text() {
            lines.push(format!("// {guidance}"));
        }
        lines.push(format!("class {name} {{"));
        self.push_fields(&mut lines, spec, None);
        lines.push("}".to_owned());
        lines.join("\n")
    }

    fn render_tagged_union(&self, name: &str, spec: &SchemaNode, tag: &str) -> String {
        let mut lines = Vec::new();
        if let Some(guidance) = spec.vlm_text() {
            lines.push(format!("// {guidance}"));
        }
        lines.push(format!(
            "// {name}: output ONE of the following (distinguished by \"{tag}\" field):"
        ));
        for raw in &spec.one_of {
            let variant = self.deref(raw);
            let Some(tag_spec) = variant.properties.get(tag) else {
                continue;
            };
            let Some(value) = tag_spec.single_const() else {
                continue;
            };
            lines.push(String::new());
            match variant.vlm_text() {
                Some(desc) => lines.push(format!("// When {tag} = \"{value}\": {desc}")),
                None => lines.push(format!("// When {tag} = \"{value}\":")),
            }
            lines.push(format!("class {name}_{value} {{"));
            self.push_fields(&mut lines, variant, Some((tag, &value)));
            lines.push("}".to_owned());
        }
        lines.join("\n")
    }

    /// Renders the property lines of an object, marking the tag field as the
    /// discriminant literal and appending `?` to fields not in `required`.
    fn push_fields(&self, lines: &mut Vec<String>, spec: &SchemaNode, tag: Option<(&str, &str)>) {
        if spec.properties.is_empty() {
            return;
        }
        let required: std::collections::BTreeSet<&str> =
            spec.required.iter().map(String::as_str).collect();
        for (field, field_spec) in &spec.properties {
            if let Some((tag_field, tag_value)) = tag
                && field == tag_field
            {
                lines.push(format!(
                    "  {field} \"{tag_value}\" @description(\"discriminant\")"
                ));
                continue;
            }
            let mut ty = self.resolve_type(field_spec);
            if !required.contains(field.as_str()) && !ty.ends_with('?') {
                ty.push('?');
            }
            match field_spec.vlm_text() {
                Some(desc) => lines.push(format!("  {field} {ty} @description(\"{desc}\")")),
                None => lines.push(format!("  {field} {ty}")),
            }
        }
    }

    /// The discriminant field of a tagged union: the property common to every
    /// variant whose value is a single constant in each.
    fn find_tag_field(&self, spec: &SchemaNode) -> Option<String> {
        if let Some(name) = spec
            .discriminator
            .as_ref()
            .and_then(|disc| disc.property_name.as_deref())
        {
            return Some(name.to_owned());
        }
        if spec.one_of.is_empty() {
            return None;
        }
        let variants: Vec<&SchemaNode> = spec.one_of.iter().map(|v| self.deref(v)).collect();
        let (first, rest) = variants.split_first()?;
        let mut common: Vec<&String> = first.properties.keys().collect();
        common.retain(|name| {
            rest.iter()
                .all(|variant| variant.properties.contains_key(*name))
        });
        common
            .into_iter()
            .find(|name| {
                variants.iter().all(|variant| {
                    variant
                        .properties
                        .get(*name)
                        .is_some_and(SchemaNode::is_single_const)
                })
            })
            .cloned()
    }
}

/// Renders a bare enum (no payload), from either schemars shape (`enum: [..]` or
/// `oneOf: [{const: ..}, ..]`).
fn render_enum(name: &str, spec: &SchemaNode) -> String {
    let mut lines = Vec::new();
    if let Some(guidance) = spec.vlm_text() {
        lines.push(format!("// {guidance}"));
    }
    lines.push(format!("enum {name} {{"));
    if !spec.enum_values.is_empty() {
        for value in &spec.enum_values {
            if let Some(variant) = value.as_str() {
                lines.push(format!("  {variant}"));
            }
        }
    } else {
        for variant in &spec.one_of {
            if let Some(value) = variant.single_const() {
                push_variant_line(&mut lines, &value, variant);
            }
        }
    }
    lines.push("}".to_owned());
    lines.join("\n")
}

/// Maps a JSON Schema scalar type name to its BAML spelling.
fn scalar(json_type: &str) -> &str {
    match json_type {
        "string" => "string",
        "integer" => "int",
        "number" => "float",
        "boolean" => "bool",
        "object" => "object",
        "array" => "array",
        _ => "any",
    }
}

/// Pushes an enum-variant line, with its own `@description` when present.
fn push_variant_line(lines: &mut Vec<String>, value: &str, spec: &SchemaNode) {
    match spec.vlm_text() {
        Some(desc) => lines.push(format!("  {value} @description(\"{desc}\")")),
        None => lines.push(format!("  {value}")),
    }
}

#[cfg(test)]
mod tests {
    use super::super::constraint;
    use super::super::types::{CompositeOutcome, RelevanceOutcome};
    use super::*;

    /// The pass-2 render, exactly. A field rename or reorder moves this string,
    /// which is the desync tripwire the render exists to be. The ask types carry
    /// no `x-vlm`, so no field or variant `@description` appears; only the
    /// hard-coded discriminant label survives. Adding real guidance to a type
    /// moves this golden, which is the point. `media` reuses core's `ImageMedium`
    /// directly, and fields render in schemars 0.8's alphabetical order.
    const RELEVANCE_RENDER: &str = "\
class Entity {
  box Rect
  description string
}

enum ImageMedium {
  picture
  map
  plan
  pictorial_map
}

class Point {
  x float
  y float
}

class Rect {
  lower_right Point
  upper_left Point
}

// RelevanceOutcome: output ONE of the following (distinguished by \"_type\" field):

// When _type = \"analyzed\":
class RelevanceOutcome_analyzed {
  _type \"analyzed\" @description(\"discriminant\")
  entities Entity[]
  media ImageMedium
  summary string
}

// When _type = \"irrelevant\":
class RelevanceOutcome_irrelevant {
  _type \"irrelevant\" @description(\"discriminant\")
  reason string
}";

    #[test]
    fn renders_the_pass_two_shape() -> Result<(), Box<dyn std::error::Error>> {
        let rendered = render_schema(&constraint::constraint_value::<RelevanceOutcome>()?)?;
        assert_eq!(rendered, RELEVANCE_RENDER);
        Ok(())
    }

    #[test]
    fn renders_the_pass_one_shape() -> Result<(), Box<dyn std::error::Error>> {
        let rendered = render_schema(&constraint::constraint_value::<CompositeOutcome>()?)?;
        assert!(rendered.contains("_type \"single\" @description(\"discriminant\")"));
        assert!(rendered.contains("_type \"composite\" @description(\"discriminant\")"));
        // `Rect` is a named class, so a list of panels renders as `Rect[]`.
        assert!(rendered.contains("panels Rect[]"));
        Ok(())
    }

    /// Guidance comes only from `x-vlm`, never a `///` doc: a field carrying a
    /// doc-comment `description` but no `x-vlm` renders with no `@description`, so
    /// developer docs cannot reach the prompt.
    #[test]
    fn doc_comments_do_not_reach_the_prompt() -> Result<(), RenderError> {
        let schema = serde_json::json!({
            "type": "object",
            "title": "Doc",
            "required": ["field"],
            "properties": {
                "field": { "type": "string", "description": "a developer-facing note" }
            }
        });
        let rendered = render_schema(&schema)?;
        assert!(
            rendered.contains("field string") && !rendered.contains("@description"),
            "a doc-only field leaked into the prompt: {rendered}"
        );
        Ok(())
    }

    #[test]
    fn optional_fields_render_as_nullable_types() -> Result<(), RenderError> {
        // schemars emits `Option<T>` as `anyOf: [T, {"type": "null"}]`. Feed the
        // renderer that shape (an optional struct-ref and an optional list) and
        // confirm it recovers the inner type and marks it optional rather than
        // collapsing to `any`. The current ask types carry no optional field, so
        // this guards the path before one arrives.
        let schema = serde_json::json!({
            "type": "object",
            "title": "Holder",
            "properties": {
                "maybe": { "anyOf": [{ "$ref": "#/$defs/Inner" }, { "type": "null" }] },
                "maybe_list": {
                    "anyOf": [{ "type": "array", "items": { "type": "integer" } }, { "type": "null" }]
                }
            },
            "$defs": { "Inner": { "type": "object", "properties": { "a": { "type": "integer" } } } }
        });
        let rendered = render_schema(&schema)?;
        assert!(
            rendered.contains("maybe Inner?"),
            "optional struct-ref lost its type: {rendered}"
        );
        assert!(
            rendered.contains("maybe_list int[]?"),
            "optional list lost its element type: {rendered}"
        );
        Ok(())
    }

    #[test]
    fn vlm_guidance_escapes_backslash_before_quote() -> Result<(), RenderError> {
        // A backslash and a quote in the guidance must both escape, backslash
        // first or the escape it adds for the quote is itself doubled, so the
        // rendered `@description("...")` stays a well-formed string.
        let schema = serde_json::json!({
            "type": "object",
            "title": "Doc",
            "required": ["field"],
            "properties": {
                "field": { "type": "string", "x-vlm": "path a\\b and a \"q\"" }
            }
        });
        let rendered = render_schema(&schema)?;
        assert!(
            rendered.contains(r#"field string @description("path a\\b and a \"q\"")"#),
            "backslash/quote not escaped correctly: {rendered}"
        );
        Ok(())
    }

    #[test]
    fn x_vlm_flows_to_the_prompt_at_every_level() -> Result<(), RenderError> {
        // The channel end to end: an `x-vlm` on the container, on a variant, and
        // on a field each reaches the rendered BAML. Shaped like schemars' tagged
        // enum output (a `oneOf` of const-tagged variants).
        let schema = serde_json::json!({
            "title": "Choice",
            "x-vlm": "Pick one.",
            "oneOf": [
                {
                    "type": "object",
                    "x-vlm": "The yes case.",
                    "required": ["_type", "label"],
                    "properties": {
                        "_type": { "const": "yes" },
                        "label": { "type": "string", "x-vlm": "A short label." }
                    }
                },
                {
                    "type": "object",
                    "required": ["_type"],
                    "properties": { "_type": { "const": "no" } }
                }
            ]
        });
        let rendered = render_schema(&schema)?;
        assert!(
            rendered.contains("// Pick one."),
            "container guidance missing: {rendered}"
        );
        assert!(
            rendered.contains("The yes case."),
            "variant guidance missing: {rendered}"
        );
        assert!(
            rendered.contains(r#"label string @description("A short label.")"#),
            "field guidance missing: {rendered}"
        );
        Ok(())
    }
}
