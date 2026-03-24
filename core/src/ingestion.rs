//! Ingestion output types.
//!
//! Types for representing the output of data ingestion processes.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::annotation::Annotation;
use crate::date::UncertainDate;
use crate::entity::{Entity, EntityRelation};
use crate::ids::{EntityIdx, LinkIdx, SourceIdx};
use crate::links::ExternalLink;
use crate::location::UncertainLocation;

/// An image of an entity.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImageSource {
    #[schemars(with = "String")]
    pub url: Url,

    /// When the image was taken (uncertain/range).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<UncertainDate>,

    /// Location extracted from EXIF or other metadata.
    ///
    /// Deliberately simplistic for this stage — just passes through EXIF GPS
    /// coordinates. Will be enriched with geocoding, cross-referencing, and
    /// uncertainty modeling as the ingestion pipeline matures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<UncertainLocation>,
}

/// Metadata about the ingestion process.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct IngestionNotes {
    #[serde(default)]
    pub conflicts_found: Vec<String>,
    #[serde(default)]
    pub additional_research_needed: Vec<String>,
    #[serde(default)]
    pub search_queries_used: Vec<String>,
}

/// Complete output from any chronoscope ingestion process.
///
/// Generic over key types:
/// - `E` — entity key (e.g., `EntityIdx` for production, `&str` for tests)
/// - `S` — source key (e.g., `SourceIdx` for production)
/// - `L` — link key (e.g., `LinkIdx` for production)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(bound(
    deserialize = "E: serde::de::DeserializeOwned + Ord, S: serde::de::DeserializeOwned + Ord, L: serde::de::DeserializeOwned + Ord"
))]
pub struct IngestionBundle<E, S, L> {
    pub entities: BTreeMap<E, Entity>,
    #[serde(default)]
    pub images: BTreeMap<S, ImageSource>,
    #[serde(default)]
    pub external_links: BTreeMap<L, ExternalLink>,

    /// Maps entity key to link keys.
    #[serde(default)]
    pub entity_links: BTreeMap<E, Vec<L>>,

    /// Relationships between entities.
    #[serde(default)]
    pub entity_relations: Vec<EntityRelation<E>>,

    /// Annotations connecting entities to sources.
    #[serde(default)]
    pub annotations: Vec<Annotation<S, E>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<IngestionNotes>,
}

impl<E: Ord, S: Ord, L: Ord> IngestionBundle<E, S, L> {
    /// Create an empty bundle.
    pub fn new() -> Self {
        Self {
            entities: BTreeMap::new(),
            images: BTreeMap::new(),
            external_links: BTreeMap::new(),
            entity_links: BTreeMap::new(),
            entity_relations: Vec::new(),
            annotations: Vec::new(),
            notes: None,
        }
    }

    /// Create a bundle containing a single entity with no relations or sources.
    #[must_use]
    pub fn single(key: E, entity: Entity) -> Self {
        Self {
            entities: BTreeMap::from([(key, entity)]),
            ..Self::new()
        }
    }
}

/// A referential integrity error in an [`IngestionBundle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceError<E, S, L> {
    /// An entity key in `entity_relations` doesn't exist in `entities`.
    DanglingEntityRelation { entity_key: E },
    /// An entity relation has the same entity as both source and target.
    SelfRelation {
        entity_key: E,
        relation_type: crate::entity::EntityRelationType,
    },
    /// An entity key in `entity_links` doesn't exist in `entities`.
    DanglingEntityLink { entity_key: E },
    /// A link key in `entity_links` values doesn't exist in `external_links`.
    DanglingLinkRef { link_key: L },
    /// A source key in `annotations` doesn't exist in `images`.
    DanglingAnnotationSource { source_key: S },
    /// An entity key in `annotations` doesn't exist in `entities`.
    DanglingAnnotationEntity { entity_key: E },
}

impl<E: fmt::Debug, S: fmt::Debug, L: fmt::Debug> fmt::Display for ReferenceError<E, S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DanglingEntityRelation { entity_key } => {
                write!(
                    f,
                    "entity relation references unknown entity {entity_key:?}"
                )
            }
            Self::SelfRelation {
                entity_key,
                relation_type,
            } => {
                write!(
                    f,
                    "entity {entity_key:?} has self-referential {relation_type:?} relation"
                )
            }
            Self::DanglingEntityLink { entity_key } => {
                write!(f, "entity_links references unknown entity {entity_key:?}")
            }
            Self::DanglingLinkRef { link_key } => {
                write!(
                    f,
                    "entity_links references unknown external link {link_key:?}"
                )
            }
            Self::DanglingAnnotationSource { source_key } => {
                write!(f, "annotation references unknown source {source_key:?}")
            }
            Self::DanglingAnnotationEntity { entity_key } => {
                write!(f, "annotation references unknown entity {entity_key:?}")
            }
        }
    }
}

impl<E: Ord + Clone, S: Ord + Clone, L: Ord + Clone> IngestionBundle<E, S, L> {
    /// Validate that all cross-references within the bundle are consistent.
    ///
    /// Checks that:
    /// - All entity keys in `entity_relations` exist in `entities`
    /// - No entity relation is self-referential
    /// - All entity keys in `entity_links` exist in `entities`
    /// - All link keys in `entity_links` values exist in `external_links`
    /// - All source/entity keys in `annotations` exist in their maps
    pub fn validate_references(&self) -> Result<(), Vec<ReferenceError<E, S, L>>>
    where
        E: PartialEq,
    {
        let mut errors = Vec::new();

        // Check entity_relations
        for relation in &self.entity_relations {
            if relation.from_entity == relation.to_entity {
                errors.push(ReferenceError::SelfRelation {
                    entity_key: relation.from_entity.clone(),
                    relation_type: relation.relation_type,
                });
            }
            if !self.entities.contains_key(&relation.from_entity) {
                errors.push(ReferenceError::DanglingEntityRelation {
                    entity_key: relation.from_entity.clone(),
                });
            }
            if !self.entities.contains_key(&relation.to_entity) {
                errors.push(ReferenceError::DanglingEntityRelation {
                    entity_key: relation.to_entity.clone(),
                });
            }
        }

        // Check entity_links
        for (entity_key, link_keys) in &self.entity_links {
            if !self.entities.contains_key(entity_key) {
                errors.push(ReferenceError::DanglingEntityLink {
                    entity_key: entity_key.clone(),
                });
            }
            for link_key in link_keys {
                if !self.external_links.contains_key(link_key) {
                    errors.push(ReferenceError::DanglingLinkRef {
                        link_key: link_key.clone(),
                    });
                }
            }
        }

        // Check annotations
        for annotation in &self.annotations {
            if !self.images.contains_key(&annotation.source) {
                errors.push(ReferenceError::DanglingAnnotationSource {
                    source_key: annotation.source.clone(),
                });
            }
            if !self.entities.contains_key(&annotation.entity) {
                errors.push(ReferenceError::DanglingAnnotationEntity {
                    entity_key: annotation.entity.clone(),
                });
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl<E: Ord, S: Ord, L: Ord> Default for IngestionBundle<E, S, L> {
    fn default() -> Self {
        Self::new()
    }
}

/// Ingestion bundle using typed positional indices (production key types).
pub type IngestionOutput = IngestionBundle<EntityIdx, SourceIdx, LinkIdx>;

/// Ingestion bundle using string keys for test fixtures.
#[cfg(test)]
pub type TestBundle = IngestionBundle<&'static str, &'static str, &'static str>;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::annotation::{Annotation, AnnotationKind};
    use crate::entity::{Entity, EntityRelation, EntityRelationType, EntityType};
    use crate::links::{ExternalLink, LinkTarget, LinkType};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn test_entity() -> Entity {
        Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![],
        }
    }

    #[test]
    fn validate_references_ok_for_empty_bundle() {
        let bundle: TestBundle = TestBundle::new();
        assert!(bundle.validate_references().is_ok());
    }

    #[test]
    fn validate_references_ok_for_valid_bundle() -> TestResult {
        let bundle = TestBundle {
            entities: BTreeMap::from([("e1", test_entity())]),
            external_links: BTreeMap::from([(
                "l1",
                ExternalLink {
                    target: LinkTarget::Url {
                        url: url::Url::parse("https://example.com")?,
                    },
                    link_type: LinkType::SameAs,
                },
            )]),
            entity_links: BTreeMap::from([("e1", vec!["l1"])]),
            ..TestBundle::new()
        };
        assert!(bundle.validate_references().is_ok());
        Ok(())
    }

    #[test]
    fn validate_references_catches_dangling_entity_relation() -> TestResult {
        let bundle = TestBundle {
            entities: BTreeMap::from([("e1", test_entity())]),
            entity_relations: vec![EntityRelation {
                from_entity: "e1",
                to_entity: "e_missing",
                relation_type: EntityRelationType::Contains,
                evidence: vec![],
            }],
            ..TestBundle::new()
        };
        let Err(errors) = bundle.validate_references() else {
            return Err("expected validation to fail".into());
        };
        assert!(errors.iter().any(|e| matches!(
            e,
            ReferenceError::DanglingEntityRelation {
                entity_key: "e_missing"
            }
        )));
        Ok(())
    }

    #[test]
    fn validate_references_catches_dangling_link() -> TestResult {
        let bundle = TestBundle {
            entities: BTreeMap::from([("e1", test_entity())]),
            entity_links: BTreeMap::from([("e1", vec!["l_missing"])]),
            ..TestBundle::new()
        };
        let Err(errors) = bundle.validate_references() else {
            return Err("expected validation to fail".into());
        };
        assert!(errors.iter().any(|e| matches!(
            e,
            ReferenceError::DanglingLinkRef {
                link_key: "l_missing"
            }
        )));
        Ok(())
    }

    #[test]
    fn validate_references_catches_dangling_entity_link_key() -> TestResult {
        let bundle = TestBundle {
            entities: BTreeMap::from([("e1", test_entity())]),
            entity_links: BTreeMap::from([("e_missing", vec![])]),
            ..TestBundle::new()
        };
        let Err(errors) = bundle.validate_references() else {
            return Err("expected validation to fail".into());
        };
        assert!(errors.iter().any(|e| matches!(
            e,
            ReferenceError::DanglingEntityLink {
                entity_key: "e_missing"
            }
        )));
        Ok(())
    }

    #[test]
    fn validate_references_catches_dangling_annotation_refs() -> TestResult {
        let bundle = TestBundle {
            entities: BTreeMap::from([("e1", test_entity())]),
            annotations: vec![Annotation {
                source: "s_missing",
                entity: "e_missing",
                kind: AnnotationKind::ExteriorView { region: None },
            }],
            ..TestBundle::new()
        };
        let Err(errors) = bundle.validate_references() else {
            return Err("expected validation to fail".into());
        };
        assert!(errors.iter().any(|e| matches!(
            e,
            ReferenceError::DanglingAnnotationSource {
                source_key: "s_missing"
            }
        )));
        assert!(errors.iter().any(|e| matches!(
            e,
            ReferenceError::DanglingAnnotationEntity {
                entity_key: "e_missing"
            }
        )));
        Ok(())
    }
}
