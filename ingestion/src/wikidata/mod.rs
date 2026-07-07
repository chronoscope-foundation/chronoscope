//! Wikidata ingestion module.
//!
//! Transforms filtered Wikidata architectural entities into Chronoscope schema.

pub mod commits;
pub mod filter;
pub mod handlers;
pub mod lifecycle;
pub mod parsing;
pub mod stream;
pub mod usage;

use chronoscope_core::{
    Cited, Evidence, ExternalLink, WikidataEntityId, WikidataField, WikidataIdParseError,
    WikidataPropertyId,
};

use crate::SourceIdx;

/// Immutable context for a property handler.
///
/// Contains everything a handler needs to create citations. The
/// Wikidata entity and property ids are parsed once at construction,
/// so `cited()` can stamp evidence with validated ids without
/// re-parsing or risking a malformed id.
#[derive(Clone, Copy)]
pub struct PropertyContext {
    entity_id: WikidataEntityId,
    revision_id: u64,
    property_id: WikidataPropertyId,
}

impl PropertyContext {
    /// Create a new `PropertyContext`, parsing the entity and
    /// property ids at the boundary.
    ///
    /// # Errors
    /// Returns [`WikidataIdParseError`] if `wikidata_id` is not a
    /// well-formed entity id (`Q<digits>`) or `property` is not a
    /// well-formed property id (`P<digits>`).
    pub fn new(
        wikidata_id: &str,
        revision_id: u64,
        property: &str,
    ) -> Result<Self, WikidataIdParseError> {
        Ok(Self::with_property(
            WikidataEntityId::parse(wikidata_id)?,
            revision_id,
            WikidataPropertyId::parse(property)?,
        ))
    }

    /// Build a context from already-parsed ids (no re-parsing or
    /// fallibility). For callers that have validated the ids at an
    /// earlier boundary.
    #[must_use]
    pub fn with_property(
        entity_id: WikidataEntityId,
        revision_id: u64,
        property_id: WikidataPropertyId,
    ) -> Self {
        Self {
            entity_id,
            revision_id,
            property_id,
        }
    }

    /// Create a cited value with Wikidata evidence for this
    /// context's property.
    #[must_use]
    pub fn cited<T>(&self, raw: impl Into<String>, value: T) -> Cited<T, SourceIdx> {
        self.cited_under(self.property_id, raw, value)
    }

    /// Create a cited value attributing evidence to `property_id`,
    /// for a value read from a property other than the context's own.
    #[must_use]
    pub fn cited_under<T>(
        &self,
        property_id: WikidataPropertyId,
        raw: impl Into<String>,
        value: T,
    ) -> Cited<T, SourceIdx> {
        Cited::new(
            value,
            vec![Evidence::Wikidata {
                entity_id: self.entity_id,
                revision_id: self.revision_id,
                field: WikidataField::Statement { property_id },
                observed_value: raw.into(),
            }],
        )
    }

    /// Get the current property ID.
    #[must_use]
    pub fn property(&self) -> WikidataPropertyId {
        self.property_id
    }

    /// Get the Wikidata entity ID.
    #[must_use]
    pub fn wikidata_id(&self) -> WikidataEntityId {
        self.entity_id
    }
}

/// Output from a property handler.
///
/// Handlers are pure functions that return what they want to add.
#[derive(Default)]
pub struct HandlerOutput {
    pub links: Vec<ExternalLink>,
    pub issues: Vec<String>,
}

impl HandlerOutput {
    /// Create an empty output.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an external link.
    pub fn add_link(&mut self, link: ExternalLink) {
        self.links.push(link);
    }

    /// Record an issue.
    pub fn issue(&mut self, msg: impl Into<String>) {
        self.issues.push(msg.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn property_context_creates_cited_evidence() -> TestResult {
        let ctx = PropertyContext::new("Q100", 42, "P571")?;
        let cited = ctx.cited("test_raw", 123);

        assert_eq!(cited.value, 123);
        assert_eq!(cited.evidence.len(), 1);
        if let chronoscope_core::Evidence::Wikidata {
            entity_id,
            revision_id,
            field,
            observed_value,
        } = &cited.evidence[0]
        {
            assert_eq!(*entity_id, WikidataEntityId::new(100));
            assert_eq!(
                *field,
                chronoscope_core::WikidataField::Statement {
                    property_id: WikidataPropertyId::new(571),
                }
            );
            assert_eq!(observed_value, "test_raw");
            assert_eq!(*revision_id, 42);
        } else {
            return Err("expected Wikidata evidence".into());
        }
        Ok(())
    }
}
