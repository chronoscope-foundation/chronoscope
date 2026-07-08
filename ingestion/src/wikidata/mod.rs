//! Wikidata ingestion module.
//!
//! Transforms filtered Wikidata architectural entities into fact-store
//! commits: `parsing`, `handlers`, and `lifecycle` extract fact-grammar values
//! (with their [`FactualCitation`]s) from Wikidata JSON, and `commits` shapes
//! them into one `submit::Commit` per item.
//!
//! # Rank policy
//!
//! [`Rank::Deprecated`](chronoscope_integrations::wikidata::Rank) claims are
//! dropped at extraction — Wikidata marks them known-incorrect. Every other
//! claim is asserted, with no `Preferred` selection: parallel claims about the
//! same slot flow into the store as competing citations for the conflict
//! machinery to surface.

pub mod commits;
pub mod filter;
pub mod handlers;
pub mod lifecycle;
pub mod parsing;
pub mod stream;
pub mod usage;

use chronoscope_core::external_ids::{WikidataEntityId, WikidataPropertyId};
use chronoscope_core::grammar::citations::{
    Excerpt, ExcerptError, ExternalSource, FactualCitation, WikidataField,
};
use chronoscope_core::nonempty::NonEmptyVec;
use chronoscope_integrations::wikidata::{Claim, Rank};

/// Claims eligible for assertion: everything except `Rank::Deprecated`.
///
/// The single implementation of the module's rank policy — every extraction
/// path reads claims through this filter.
pub fn asserted_claims(claims: &[Claim]) -> impl Iterator<Item = &Claim> {
    claims.iter().filter(|c| c.rank != Rank::Deprecated)
}

/// Per-item citation context: the parsed item id and revision every Wikidata
/// citation carries, plus the pre-built item-record citation used as the
/// fallback source for facts with no more precise origin.
#[derive(Clone)]
pub struct ItemContext {
    entity_id: WikidataEntityId,
    revision_id: u64,
    item_citation: FactualCitation,
}

impl ItemContext {
    /// Build a context for one item snapshot, constructing the item-record
    /// citation up front.
    pub fn new(entity_id: WikidataEntityId, revision_id: u64) -> Result<Self, ExcerptError> {
        let item_citation = Self::build_citation(
            entity_id,
            revision_id,
            WikidataField::Item,
            entity_id.to_string(),
        )?;
        Ok(Self {
            entity_id,
            revision_id,
            item_citation,
        })
    }

    /// The one place a Wikidata `FactualCitation` is shaped: quote `value` as
    /// the excerpt and record `field`/`revision` beside the item id. Every
    /// citation this context hands out flows through here so the excerpt and
    /// source can't drift apart.
    fn build_citation(
        entity_id: WikidataEntityId,
        revision_id: u64,
        field: WikidataField,
        value: String,
    ) -> Result<FactualCitation, ExcerptError> {
        let excerpt = Excerpt::new(value.clone())?;
        Ok(FactualCitation {
            source: ExternalSource::Wikidata {
                entity_id,
                field,
                revision_id,
                value,
            },
            excerpts: NonEmptyVec::singleton(excerpt),
        })
    }

    /// The item's Wikidata entity id.
    pub fn entity_id(&self) -> WikidataEntityId {
        self.entity_id
    }

    /// The pinned revision id of the item snapshot.
    pub fn revision_id(&self) -> u64 {
        self.revision_id
    }

    /// Citation for a claim read from `field`, quoting the observed `value`
    /// as its excerpt.
    pub fn citation(
        &self,
        field: WikidataField,
        value: impl Into<String>,
    ) -> Result<FactualCitation, ExcerptError> {
        Self::build_citation(self.entity_id, self.revision_id, field, value.into())
    }

    /// Citation for a property statement.
    pub fn statement_citation(
        &self,
        property_id: WikidataPropertyId,
        value: impl Into<String>,
    ) -> Result<FactualCitation, ExcerptError> {
        self.citation(WikidataField::Statement { property_id }, value)
    }

    /// The item-record citation — cites the item's existence as a whole.
    pub fn item_citation(&self) -> FactualCitation {
        self.item_citation.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chronoscope_integrations::wikidata::{DataValue, Snak};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn citation_carries_field_revision_and_observed_value() -> TestResult {
        let ctx = ItemContext::new(WikidataEntityId::new(100), 42)?;
        let citation = ctx.statement_citation(WikidataPropertyId::new(571), "test_raw")?;

        let ExternalSource::Wikidata {
            entity_id,
            field,
            revision_id,
            value,
        } = &citation.source
        else {
            return Err("expected Wikidata source".into());
        };
        assert_eq!(*entity_id, WikidataEntityId::new(100));
        assert_eq!(
            *field,
            WikidataField::Statement {
                property_id: WikidataPropertyId::new(571),
            }
        );
        assert_eq!(*revision_id, 42);
        assert_eq!(value, "test_raw");
        assert_eq!(citation.excerpts.first().as_str(), "test_raw");
        Ok(())
    }

    #[test]
    fn item_citation_quotes_the_item_qid() -> TestResult {
        let ctx = ItemContext::new(WikidataEntityId::new(1234), 7)?;
        let citation = ctx.item_citation();
        let ExternalSource::Wikidata { field, value, .. } = &citation.source else {
            return Err("expected Wikidata source".into());
        };
        assert_eq!(*field, WikidataField::Item);
        assert_eq!(value, "Q1234");
        Ok(())
    }

    #[test]
    fn asserted_claims_drops_deprecated_only() {
        let value = |s: &str| Claim::simple(Snak::Value(DataValue::String(s.to_owned())));
        let mut deprecated = value("dropped");
        deprecated.rank = Rank::Deprecated;
        let mut preferred = value("preferred");
        preferred.rank = Rank::Preferred;
        let claims = vec![value("normal"), deprecated, preferred];

        let kept: Vec<&str> = asserted_claims(&claims)
            .filter_map(|c| c.mainsnak.string_value())
            .collect();
        assert_eq!(kept, vec!["normal", "preferred"]);
    }
}
