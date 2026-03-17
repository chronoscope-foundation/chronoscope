//! Bundle loader: loads an [`IngestionOutput`] into the database.
//!
//! This module is the bridge between the ingestion pipeline (which produces
//! [`IngestionOutput`] bundles) and the entity database. It handles:
//! - Inserting entities with their external IDs, links, and annotations
//! - Creating `research_urls` entries for image sources
//! - Linking entities to sources via annotations

use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;

use chronoscope_core::ingestion::IngestionBundle;
use chronoscope_core::links::LinkTarget;
use sqlx::SqlitePool;

use crate::error::{DbError, DbResult};
use crate::models::{
    extract_location, extract_target_url, extract_temporal_bounds, link_type_to_db,
    relation_type_to_db,
};
use crate::types::{
    AnnotationDbId, EntityDbId, EntityLinkDbId, ExternalIdType, ResearchUrlId, ResearchUrlStatus,
};
use crate::{now, queries};

/// Summary of what `load_bundle` did.
#[derive(Debug, Clone, Default)]
pub struct LoadResult {
    pub entities_created: usize,
    pub images_created: usize,
    pub annotations_created: usize,
    pub relations_created: usize,
}

/// Load an [`IngestionBundle`] into the database.
///
/// Generic over the bundle's key types — works with any `IngestionBundle<E, S, L>`
/// as long as the keys are orderable, hashable, and debuggable.
///
/// The entire load is wrapped in a single transaction for atomicity.
/// Every entity in the bundle is inserted unconditionally (no dedup).
///
/// # Errors
/// Returns `DbError` on database failures, JSON serialization errors,
/// or if the bundle has invalid cross-references.
pub async fn load_bundle<E, S, L>(
    pool: &SqlitePool,
    bundle: &IngestionBundle<E, S, L>,
) -> DbResult<LoadResult>
where
    E: Ord + Clone + Eq + Hash + Copy + Debug,
    S: Ord + Clone + Eq + Hash + Copy + Debug,
    L: Ord + Clone + Debug,
{
    // Validate cross-references before any DB writes
    bundle.validate_references().map_err(|errors| {
        DbError::InvalidBundle(errors.iter().map(|e| format!("{e}")).collect())
    })?;

    let mut result = LoadResult::default();
    let timestamp = now();

    let mut tx = pool.begin().await?;

    // Phase 1: Load images as pending research_urls (so source_id_map is complete for Phase 2)
    let mut source_id_map: HashMap<S, ResearchUrlId> = HashMap::new();

    for (&source_idx, image) in &bundle.images {
        let url_str = image.url.to_string();

        let existing: Option<(ResearchUrlId,)> = sqlx::query_as(queries::GET_URL_BY_URL.sql)
            .bind(&url_str)
            .fetch_optional(&mut *tx)
            .await?;

        let url_id = if let Some((id,)) = existing {
            id
        } else {
            let new_id = ResearchUrlId::generate();
            queries::CREATE_URL_OR_IGNORE
                .query()
                .bind(&new_id)
                .bind(&url_str)
                .bind(ResearchUrlStatus::Pending)
                .bind(0_i32)
                .bind(None::<String>) // worker_affinity
                .bind(timestamp)
                .execute(&mut *tx)
                .await?;
            result.images_created += 1;
            new_id
        };

        source_id_map.insert(source_idx, url_id);
    }

    // Phase 2: Load entities (insert every entity unconditionally)
    let mut entity_id_map: HashMap<E, EntityDbId> = HashMap::new();

    for (&entity_idx, entity) in &bundle.entities {
        let links: Vec<_> = bundle
            .entity_links
            .get(&entity_idx)
            .map(|link_indices| {
                link_indices
                    .iter()
                    .filter_map(|li| bundle.external_links.get(li))
                    .collect()
            })
            .unwrap_or_default();

        // Insert new entity
        let entity_json = serde_json::to_string(entity)?;
        let (earliest, latest) = extract_temporal_bounds(entity);
        let location = extract_location(entity);

        let new_id = EntityDbId::generate();
        queries::INSERT_ENTITY
            .query()
            .bind(&new_id)
            .bind(&entity_json)
            .bind(&earliest)
            .bind(&latest)
            .bind(location.map(|(lat, _)| lat))
            .bind(location.map(|(_, lon)| lon))
            .bind(timestamp)
            .bind(timestamp)
            .execute(&mut *tx)
            .await?;
        result.entities_created += 1;

        // Extract and insert all external IDs from links
        for ext_id in extract_external_ids(&links) {
            queries::INSERT_EXTERNAL_ID
                .query()
                .bind(ext_id.0.as_str())
                .bind(&ext_id.1)
                .bind(&new_id)
                .execute(&mut *tx)
                .await?;
        }

        // Insert links
        for link in &links {
            let link_id = EntityLinkDbId::generate();
            let target_url = extract_target_url(&link.target);

            queries::INSERT_ENTITY_LINK
                .query()
                .bind(&link_id)
                .bind(&new_id)
                .bind(link_type_to_db(&link.link_type))
                .bind(&target_url)
                .execute(&mut *tx)
                .await?;
        }

        // Insert annotations for this entity (source_id_map is complete from Phase 1)
        for annotation in bundle.annotations.iter().filter(|a| a.entity == entity_idx) {
            let uid = source_id_map.get(&annotation.source).ok_or_else(|| {
                DbError::InvalidBundle(vec![format!(
                    "annotation references unmapped source {:?}",
                    annotation.source
                )])
            })?;

            let annotation_id = AnnotationDbId::generate();
            let kind_json = serde_json::to_string(&annotation.kind)?;

            queries::INSERT_ANNOTATION
                .query()
                .bind(&annotation_id)
                .bind(&new_id)
                .bind(uid)
                .bind(&kind_json)
                .bind(timestamp)
                .execute(&mut *tx)
                .await?;
            result.annotations_created += 1;
        }

        entity_id_map.insert(entity_idx, new_id);
    }

    // Phase 3: Load entity relations (entity_id_map is complete from Phase 2)
    for relation in &bundle.entity_relations {
        let from = entity_id_map.get(&relation.from_entity).ok_or_else(|| {
            DbError::InvalidBundle(vec![format!(
                "relation references unmapped entity {:?}",
                relation.from_entity
            )])
        })?;
        let to = entity_id_map.get(&relation.to_entity).ok_or_else(|| {
            DbError::InvalidBundle(vec![format!(
                "relation references unmapped entity {:?}",
                relation.to_entity
            )])
        })?;

        let relation_type = relation_type_to_db(&relation.relation_type);
        let evidence_json = serde_json::to_string(&relation.evidence)?;

        queries::INSERT_ENTITY_RELATION
            .query()
            .bind(from)
            .bind(to)
            .bind(relation_type)
            .bind(&evidence_json)
            .execute(&mut *tx)
            .await?;
        result.relations_created += 1;
    }

    tx.commit().await?;

    Ok(result)
}

/// Extract all structured external IDs from a set of external links.
///
/// Maps each `LinkTarget` variant to its corresponding `(ExternalIdType, id_string)` pair.
/// Only structured authority links produce external IDs; generic URLs, Wikipedia,
/// `WikimediaCommons`, and Sanborn links do not.
fn extract_external_ids(
    links: &[&chronoscope_core::links::ExternalLink],
) -> Vec<(ExternalIdType, String)> {
    let mut ids = Vec::new();
    for link in links {
        if let Some(pair) = link_target_to_external_id(&link.target) {
            ids.push(pair);
        }
    }
    ids
}

/// Map a single `LinkTarget` to an `(ExternalIdType, id_string)` pair, if applicable.
fn link_target_to_external_id(target: &LinkTarget) -> Option<(ExternalIdType, String)> {
    match target {
        LinkTarget::Wikidata { entity_id } => Some((ExternalIdType::Wikidata, entity_id.0.clone())),
        LinkTarget::OpenStreetMap {
            element_type,
            element_id,
        } => {
            let id_type = match element_type {
                chronoscope_core::ids::OsmElementType::Node => ExternalIdType::OsmNode,
                chronoscope_core::ids::OsmElementType::Way => ExternalIdType::OsmWay,
                chronoscope_core::ids::OsmElementType::Relation => ExternalIdType::OsmRelation,
            };
            Some((id_type, element_id.0.to_string()))
        }
        LinkTarget::Pleiades { place_id } => Some((ExternalIdType::Pleiades, place_id.clone())),
        LinkTarget::GeoNames { id } => Some((ExternalIdType::GeoNames, id.0.to_string())),
        LinkTarget::GettyTgn { id } => Some((ExternalIdType::GettyTgn, id.0.to_string())),
        LinkTarget::Nrhp { reference_number } => {
            Some((ExternalIdType::Nrhp, reference_number.clone()))
        }
        // Wikipedia, WikimediaCommons, Sanborn, and generic URLs don't have
        // structured external IDs in our schema.
        LinkTarget::Wikipedia { .. }
        | LinkTarget::WikimediaCommons { .. }
        | LinkTarget::Sanborn { .. }
        | LinkTarget::Url { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{Datelike, NaiveDate};
    use chronoscope_core::annotation::{Annotation, AnnotationKind};
    use chronoscope_core::entity::{
        Entity, EntityRelation, EntityRelationType, EntityTransition, EntityType,
    };
    use chronoscope_core::ids::{EntityIdx, LinkIdx, SourceIdx, WikidataEntityId};
    use chronoscope_core::ingestion::{ImageSource, IngestionOutput};
    use chronoscope_core::links::{ExternalLink, LinkTarget, LinkType};
    use chronoscope_core::{Cited, DatePrecision, UncertainDate, UncertainLocation};
    use oxilangtag::LanguageTag;

    use super::*;
    use crate::Database;
    use crate::models::EntityDbRow;
    use crate::queue::{Queue, url_queue_config};

    /// Query entity by external ID, returning all columns `EntityDbRow` expects.
    const FIND_ENTITY_FOR_TEST: &str = "
        SELECT e.id, e.entity_type, e.entity_json, e.earliest_date, e.latest_date,
               e.latitude, e.longitude, e.created_at, e.updated_at
        FROM entities e
        JOIN entity_external_ids x ON x.entity_id = e.id
        WHERE x.id_type = ? AND x.external_id = ?
    ";

    fn test_entity(name: &str) -> Entity {
        #[allow(clippy::expect_used)]
        Entity {
            entity_type: EntityType::Building,
            names: vec![Cited::uncited(chronoscope_core::entity::EntityName {
                name: name.to_string(),
                name_type: chronoscope_core::entity::NameType::Official,
                language: LanguageTag::parse("en".to_string()).expect("valid tag"),
                valid_from: None,
                valid_to: None,
            })],
            transitions: vec![],
        }
    }

    fn test_entity_with_date(name: &str, year: i32) -> Entity {
        #[allow(clippy::expect_used)]
        let date = UncertainDate::with_precision(
            NaiveDate::from_ymd_opt(year, 1, 1)
                .expect("valid date")
                .and_hms_opt(0, 0, 0)
                .expect("valid time"),
            DatePrecision::Year,
        )
        .expect("valid precision");

        let mut entity = test_entity(name);
        entity.transitions.push(EntityTransition::Constructed {
            started_at: None,
            completed_at: Some(Cited::uncited(date)),
            location: None,
            trigger_event: None,
        });
        entity
    }

    fn wikidata_link(qid: &str) -> ExternalLink {
        ExternalLink {
            target: LinkTarget::Wikidata {
                entity_id: WikidataEntityId(qid.to_string()),
            },
            link_type: LinkType::SameAs,
        }
    }

    /// Build a bundle with one entity per (name, qid) pair, each with a Wikidata link.
    /// Returns the bundle plus the `EntityIdx` and `LinkIdx` vectors for further customization.
    fn bundle_with_entities(
        entries: &[(&str, &str)],
    ) -> (IngestionOutput, Vec<EntityIdx>, Vec<LinkIdx>) {
        let mut entities = BTreeMap::new();
        let mut external_links = BTreeMap::new();
        let mut entity_links = BTreeMap::new();
        let mut eidxs = Vec::new();
        let mut lidxs = Vec::new();

        for (i, (name, qid)) in entries.iter().enumerate() {
            let ei = EntityIdx::new(i);
            let li = LinkIdx::new(i);
            entities.insert(ei, test_entity(name));
            external_links.insert(li, wikidata_link(qid));
            entity_links.insert(ei, vec![li]);
            eidxs.push(ei);
            lidxs.push(li);
        }

        let bundle = IngestionOutput {
            entities,
            images: BTreeMap::new(),
            external_links,
            entity_links,
            entity_relations: vec![],
            annotations: vec![],
            notes: None,
        };
        (bundle, eidxs, lidxs)
    }

    /// Add an image to a bundle, returning its `SourceIdx`.
    #[allow(clippy::expect_used)]
    fn add_image(bundle: &mut IngestionOutput, url: &str) -> SourceIdx {
        let idx = SourceIdx::new(bundle.images.len());
        bundle.images.insert(
            idx,
            ImageSource {
                url: url::Url::parse(url).expect("test URL is valid"),
                date: None,
                location: None,
            },
        );
        idx
    }

    fn test_bundle() -> IngestionOutput {
        bundle_with_entities(&[("Eiffel Tower", "Q243")]).0
    }

    async fn test_db() -> DbResult<Database> {
        Database::new_without_plan_verification("sqlite::memory:").await
    }

    #[tokio::test]
    async fn load_bundle_creates_entity() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = test_db().await?;
        let bundle = test_bundle();

        let result = load_bundle(db.pool_ref(), &bundle).await?;
        assert_eq!(result.entities_created, 1);

        // Verify entity exists via external ID lookup
        let row: Option<EntityDbRow> = sqlx::query_as(FIND_ENTITY_FOR_TEST)
            .bind("wikidata")
            .bind("Q243")
            .fetch_optional(db.pool_ref())
            .await?;
        let row = row.ok_or("entity not found")?;
        assert_eq!(row.entity_type, crate::types::DbEntityType::Building);
        let entity: Entity = serde_json::from_str(&row.entity_json)?;
        assert_eq!(entity.names.len(), 1);
        assert_eq!(entity.names[0].value.name, "Eiffel Tower");
        Ok(())
    }

    #[tokio::test]
    async fn load_bundle_creates_links() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = test_db().await?;
        let bundle = test_bundle();

        load_bundle(db.pool_ref(), &bundle).await?;

        let links: Vec<(String, String)> =
            sqlx::query_as("SELECT link_type, target_url FROM entity_links")
                .fetch_all(db.pool_ref())
                .await?;
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].0, "same_as");
        assert!(links[0].1.contains("wikidata.org"));
        Ok(())
    }

    #[tokio::test]
    async fn load_bundle_creates_images_as_pending_urls()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = test_db().await?;

        let (mut bundle, eidxs, _) = bundle_with_entities(&[("Eiffel Tower", "Q243")]);
        let s0 = add_image(&mut bundle, "https://example.com/eiffel.jpg");
        bundle.annotations.push(Annotation {
            source: s0,
            entity: eidxs[0],
            kind: AnnotationKind::ExteriorView { region: None },
        });

        let result = load_bundle(db.pool_ref(), &bundle).await?;
        assert_eq!(result.images_created, 1);
        assert_eq!(result.annotations_created, 1);

        // Verify research_url was created as pending
        let urls: Vec<(String, String)> = sqlx::query_as("SELECT url, status FROM research_urls")
            .fetch_all(db.pool_ref())
            .await?;
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].0, "https://example.com/eiffel.jpg");
        assert_eq!(urls[0].1, "pending");

        // Verify annotation was created
        let annotations: Vec<(String,)> = sqlx::query_as("SELECT kind FROM annotations")
            .fetch_all(db.pool_ref())
            .await?;
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations[0].0, "exterior_view");
        Ok(())
    }

    #[tokio::test]
    async fn load_bundle_creates_relations() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    {
        let db = test_db().await?;

        let (mut bundle, eidxs, _) =
            bundle_with_entities(&[("Old Penn Station", "Q761847"), ("MSG", "Q186")]);
        bundle.entity_relations.push(EntityRelation {
            from_entity: eidxs[1],
            to_entity: eidxs[0],
            relation_type: EntityRelationType::Replaces,
            evidence: vec![],
        });

        let result = load_bundle(db.pool_ref(), &bundle).await?;
        assert_eq!(result.entities_created, 2);
        assert_eq!(result.relations_created, 1);

        let relations: Vec<(String,)> =
            sqlx::query_as("SELECT relation_type FROM entity_relations")
                .fetch_all(db.pool_ref())
                .await?;
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].0, "replaces");
        Ok(())
    }

    #[tokio::test]
    async fn load_bundle_temporal_shadow_columns()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = test_db().await?;

        let (mut bundle, eidxs, _) = bundle_with_entities(&[("Eiffel Tower", "Q243")]);
        bundle
            .entities
            .insert(eidxs[0], test_entity_with_date("Eiffel Tower", 1889));

        load_bundle(db.pool_ref(), &bundle).await?;

        let row: Option<EntityDbRow> = sqlx::query_as(FIND_ENTITY_FOR_TEST)
            .bind("wikidata")
            .bind("Q243")
            .fetch_optional(db.pool_ref())
            .await?;
        let row = row.ok_or("entity not found")?;
        // completed_at (year 1889) should populate latest_date
        let latest = row.latest_date.ok_or("no latest")?;
        assert_eq!(latest.and_utc().year(), 1889);
        Ok(())
    }

    #[tokio::test]
    async fn load_bundle_spatial_shadow_columns()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = test_db().await?;

        let (mut bundle, eidxs, _) = bundle_with_entities(&[("Eiffel Tower", "Q243")]);
        let loc = UncertainLocation::coordinates(48.8584, 2.2945, None, None)?;
        bundle
            .entities
            .get_mut(&eidxs[0])
            .ok_or("missing")?
            .transitions
            .push(EntityTransition::Constructed {
                started_at: None,
                completed_at: None,
                location: Some(Cited::uncited(loc)),
                trigger_event: None,
            });

        load_bundle(db.pool_ref(), &bundle).await?;

        let row: Option<EntityDbRow> = sqlx::query_as(FIND_ENTITY_FOR_TEST)
            .bind("wikidata")
            .bind("Q243")
            .fetch_optional(db.pool_ref())
            .await?;
        let row = row.ok_or("entity not found")?;
        let lat = row.latitude.ok_or("no latitude")?;
        let lon = row.longitude.ok_or("no longitude")?;
        assert!((lat - 48.8584).abs() < f64::EPSILON);
        assert!((lon - 2.2945).abs() < f64::EPSILON);
        // earliest_date should be None since no dates were provided
        assert!(row.earliest_date.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn loaded_images_claimable_by_url_queue()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = test_db().await?;

        let (mut bundle, _, _) = bundle_with_entities(&[("Eiffel Tower", "Q243")]);
        add_image(&mut bundle, "https://example.com/photo1.jpg");
        add_image(&mut bundle, "https://example.com/photo2.jpg");

        load_bundle(db.pool_ref(), &bundle).await?;

        // The generic URL queue should be able to claim both images
        let queue: Queue<crate::ResearchUrl> =
            Queue::new(db.pool_ref().clone(), url_queue_config(None));
        let stale = chrono::Utc::now().naive_utc() - chrono::Duration::hours(1);
        let claimed = queue.claim("test-worker", 10, stale).await?;

        assert_eq!(claimed.len(), 2, "queue should claim both loaded images");
        let urls: Vec<&str> = claimed.iter().map(|r| r.url.as_str()).collect();
        assert!(urls.contains(&"https://example.com/photo1.jpg"));
        assert!(urls.contains(&"https://example.com/photo2.jpg"));
        Ok(())
    }

    #[tokio::test]
    async fn load_bundle_rejects_invalid_references()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = test_db().await?;

        let (mut bundle, eidxs, _) = bundle_with_entities(&[("Eiffel Tower", "Q243")]);
        let e_missing = EntityIdx::new(99);
        bundle.entity_relations.push(EntityRelation {
            from_entity: eidxs[0],
            to_entity: e_missing,
            relation_type: EntityRelationType::Replaces,
            evidence: vec![],
        });

        let result = load_bundle(db.pool_ref(), &bundle).await;
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn all_enum_variants_accepted_by_db_checks()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use crate::types::{DbEntityType, EntityLinkDbId, ExternalIdType};
        use chronoscope_core::links::LinkType;

        let db = test_db().await?;
        let timestamp = crate::now();

        // --- EntityType: insert one entity per variant ---
        let entity_types = DbEntityType::all();
        let mut entity_ids = Vec::new();
        for (i, &et) in entity_types.iter().enumerate() {
            let id = EntityDbId::generate();
            let entity = Entity {
                entity_type: match et {
                    DbEntityType::Area => EntityType::Area,
                    DbEntityType::Building => EntityType::Building,
                    DbEntityType::Infrastructure => EntityType::Infrastructure,
                    DbEntityType::Monument => EntityType::Monument,
                    DbEntityType::NaturalFeature => EntityType::NaturalFeature,
                },
                names: vec![],
                transitions: vec![],
            };
            let entity_json = serde_json::to_string(&entity)?;
            queries::INSERT_ENTITY
                .query()
                .bind(&id)
                .bind(&entity_json)
                .bind(None::<String>)
                .bind(None::<String>)
                .bind(None::<f64>)
                .bind(None::<f64>)
                .bind(timestamp)
                .bind(timestamp)
                .execute(db.pool_ref())
                .await?;

            // Verify the generated column round-trips the type
            let row: EntityDbRow = sqlx::query_as(
                "SELECT id, entity_type, entity_json, earliest_date, latest_date, latitude, longitude, created_at, updated_at FROM entities WHERE id = ?",
            )
            .bind(&id)
            .fetch_one(db.pool_ref())
            .await?;
            assert_eq!(row.entity_type, et, "entity_type mismatch for variant {i}");

            entity_ids.push(id);
        }

        // We need at least 2 entities for relations
        let entity_a = &entity_ids[0];
        let entity_b = &entity_ids[1];

        // --- ExternalIdType: insert one row per variant ---
        for (i, id_type) in ExternalIdType::all().iter().enumerate() {
            queries::INSERT_EXTERNAL_ID
                .query()
                .bind(id_type.as_str())
                .bind(format!("test-ext-{i}"))
                .bind(entity_a)
                .execute(db.pool_ref())
                .await?;
        }

        // --- LinkType: insert one link per variant ---
        let link_types = [
            LinkType::SameAs,
            LinkType::Related,
            LinkType::FurtherReading,
        ];
        for (i, lt) in link_types.iter().enumerate() {
            let link_id = EntityLinkDbId::generate();
            queries::INSERT_ENTITY_LINK
                .query()
                .bind(&link_id)
                .bind(entity_a)
                .bind(crate::models::link_type_to_db(lt))
                .bind(format!("https://example.com/link-{i}"))
                .execute(db.pool_ref())
                .await?;
        }

        // --- EntityRelationType: insert one relation per variant ---
        let relation_types = [
            EntityRelationType::Replaces,
            EntityRelationType::Contains,
            EntityRelationType::MergedFrom,
            EntityRelationType::SplitFrom,
        ];
        for rt in &relation_types {
            queries::INSERT_ENTITY_RELATION
                .query()
                .bind(entity_a)
                .bind(entity_b)
                .bind(crate::models::relation_type_to_db(rt))
                .bind("[]")
                .execute(db.pool_ref())
                .await?;
        }

        // --- AnnotationKind: insert one annotation per variant ---
        // First we need a research_url for annotations
        let url_id = ResearchUrlId::generate();
        queries::CREATE_URL
            .query()
            .bind(&url_id)
            .bind("https://example.com/test-image.jpg")
            .bind(ResearchUrlStatus::Pending)
            .bind(0_i32)
            .bind(None::<String>)
            .bind(timestamp)
            .execute(db.pool_ref())
            .await?;

        let annotation_kinds = [
            AnnotationKind::SpatialTrace { geometry: None },
            AnnotationKind::ExteriorView { region: None },
            AnnotationKind::InteriorView { region: None },
            AnnotationKind::TextualNote {
                region: None,
                extracted_text: None,
            },
        ];
        for kind in &annotation_kinds {
            let ann_id = crate::types::AnnotationDbId::generate();
            let kind_json = serde_json::to_string(kind)?;
            queries::INSERT_ANNOTATION
                .query()
                .bind(&ann_id)
                .bind(entity_a)
                .bind(&url_id)
                .bind(&kind_json)
                .bind(timestamp)
                .execute(db.pool_ref())
                .await?;
        }

        Ok(())
    }

    #[tokio::test]
    async fn load_bundle_creates_entity_without_external_id()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = test_db().await?;

        // Bundle with an entity but no external links
        let e0 = EntityIdx::new(0);
        let bundle = IngestionOutput {
            entities: BTreeMap::from([(e0, test_entity("Mystery Building"))]),
            images: BTreeMap::new(),
            external_links: BTreeMap::new(),
            entity_links: BTreeMap::new(),
            entity_relations: vec![],
            annotations: vec![],
            notes: None,
        };

        let result = load_bundle(db.pool_ref(), &bundle).await?;
        assert_eq!(result.entities_created, 1);
        Ok(())
    }
}
