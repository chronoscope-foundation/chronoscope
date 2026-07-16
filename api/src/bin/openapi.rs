//! Generate OpenAPI specification for the Chronoscope API.
//!
//! # Post-processing
//!
//! This binary post-processes the generated spec to fix schemars' enum representation.
//! We'd prefer to eliminate this, but dropshot pins us to schemars 0.8 which lacks the
//! necessary controls. See:
//! - <https://github.com/oxidecomputer/dropshot/issues/1375> (schemars 1.0 migration)
//! - <https://github.com/oxidecomputer/dropshot/pull/1449> (documented blockers)
//!
//! The underlying issue is that schemars 0.8.11+ respects doc comments on enum variants
//! (GREsau/schemars#152), which causes it to generate `oneOf` schemas instead of simple
//! string enums. Without OpenAPI discriminator metadata, code generators like Swift OpenAPI
//! Generator fall back to positional `.case1`, `.case2` variant names, making the generated
//! API unpleasant to use.
//!
//! We fix two cases:
//! - **Flat enums**: `oneOf` of string variants → `{type: string, enum: [...]}`
//! - **Tagged enums**: `oneOf` of objects → adds `discriminator` and extracts named variants

use std::env;

use dropshot::ApiDescription;
use indexmap::IndexMap;
use openapiv3::{
    Discriminator, OpenAPI, ReferenceOr, Schema, SchemaKind, Server, StringType, Type,
};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut api = ApiDescription::new();
    chronoscope_api::register_api(&mut api)?;

    let mut spec: OpenAPI = serde_json::from_value(
        api.openapi("Chronoscope API", semver::Version::new(0, 1, 0))
            .json()?,
    )?;

    // The front door mounts the API under `/api` and strips the prefix; routes
    // are defined at root. Declaring the server base keeps the spec's public
    // URLs honest without baking `/api` into every route.
    spec.servers = vec![Server {
        url: "/api".to_string(),
        ..Default::default()
    }];

    if let Some(components) = &mut spec.components {
        components.schemas = std::mem::take(&mut components.schemas)
            .into_iter()
            .flat_map(|(name, schema)| transform(name, schema))
            .collect();
    }

    let output = serde_json::to_string_pretty(&spec)?;
    match env::args().nth(1) {
        Some(path) => {
            std::fs::write(&path, &output)?;
            eprintln!("OpenAPI spec written to {path}");
        }
        None => println!("{output}"),
    }
    Ok(())
}

/// Transform a schema, potentially producing multiple output schemas.
/// - Flat enums: collapse oneOf to single string enum
/// - Tagged unions: extract variants to named schemas and add discriminator
fn transform(name: String, schema_ref: ReferenceOr<Schema>) -> Vec<(String, ReferenceOr<Schema>)> {
    let ReferenceOr::Item(mut schema) = schema_ref else {
        return vec![(name, schema_ref)];
    };
    let SchemaKind::OneOf { one_of } = &schema.schema_kind else {
        return vec![(name, ReferenceOr::Item(schema))];
    };

    // Flat enum: all variants are strings → collapse to single enum
    if let Some(values) = as_flat_enum(one_of) {
        schema.schema_kind = SchemaKind::Type(Type::String(StringType {
            enumeration: values.into_iter().map(Some).collect(),
            ..Default::default()
        }));
        return vec![(name, ReferenceOr::Item(schema))];
    }

    // Tagged union: all variants are objects with shared discriminator property
    if let Some((discriminator, variants)) = as_tagged_union(one_of) {
        let mut variant_schemas = Vec::new();
        let mut one_of = Vec::new();
        let mut mapping = IndexMap::new();

        for (value, variant) in variants {
            let suffix = to_pascal_case(&value);
            let variant_name = format!("{name}_{suffix}");
            mapping.insert(value, format!("#/components/schemas/{variant_name}"));
            one_of.push(ReferenceOr::Reference {
                reference: format!("#/components/schemas/{variant_name}"),
            });
            variant_schemas.push((variant_name, ReferenceOr::Item(variant)));
        }

        schema.schema_kind = SchemaKind::OneOf { one_of };
        schema.schema_data.discriminator = Some(Discriminator {
            property_name: discriminator,
            mapping,
            extensions: IndexMap::new(),
        });

        variant_schemas.push((name, ReferenceOr::Item(schema)));
        return variant_schemas;
    }

    vec![(name, ReferenceOr::Item(schema))]
}

/// If all oneOf variants are string types, collect their enum values.
fn as_flat_enum(variants: &[ReferenceOr<Schema>]) -> Option<Vec<String>> {
    variants
        .iter()
        .try_fold(vec![], |mut acc, v| {
            let ReferenceOr::Item(s) = v else { return None };
            let SchemaKind::Type(Type::String(st)) = &s.schema_kind else {
                return None;
            };
            acc.extend(st.enumeration.iter().filter_map(Clone::clone));
            Some(acc)
        })
        .filter(|v| !v.is_empty())
}

/// If all oneOf variants are objects with a shared single-value-enum property,
/// return the discriminator property name and extracted (value, schema) pairs.
fn as_tagged_union(variants: &[ReferenceOr<Schema>]) -> Option<(String, Vec<(String, Schema)>)> {
    let discriminator = find_discriminator(variants)?;

    let extracted: Option<Vec<_>> = variants
        .iter()
        .map(|v| {
            let ReferenceOr::Item(schema) = v else {
                return None;
            };
            let SchemaKind::Type(Type::Object(obj)) = &schema.schema_kind else {
                return None;
            };
            let ReferenceOr::Item(prop) = obj.properties.get(&discriminator)? else {
                return None;
            };
            let SchemaKind::Type(Type::String(st)) = &prop.schema_kind else {
                return None;
            };
            let value = st.enumeration.first()?.as_ref()?.clone();
            Some((value, schema.clone()))
        })
        .collect();

    Some((discriminator, extracted?))
}

/// Find a property that exists as a single-value string enum in all object variants.
fn find_discriminator(variants: &[ReferenceOr<Schema>]) -> Option<String> {
    let ReferenceOr::Item(first) = variants.first()? else {
        return None;
    };
    let SchemaKind::Type(Type::Object(first_obj)) = &first.schema_kind else {
        return None;
    };

    first_obj
        .properties
        .keys()
        .find(|prop| {
            variants.iter().all(|v| {
                let ReferenceOr::Item(s) = v else { return false };
                let SchemaKind::Type(Type::Object(o)) = &s.schema_kind else {
                    return false;
                };
                matches!(
                    o.properties.get(*prop),
                    Some(ReferenceOr::Item(p))
                        if matches!(&p.schema_kind, SchemaKind::Type(Type::String(st)) if st.enumeration.len() == 1)
                )
            })
        })
        .cloned()
}

fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .filter_map(|p| {
            let mut c = p.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
        })
        .collect()
}
