//! Bundle loader: loads an [`IngestionBundle`] into the database.
//!
//! This module is the bridge between the ingestion pipeline (which produces
//! bundles) and the entity database. It handles:
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
use crate::models::{extract_location, extract_target_url, extract_temporal_bounds};
use crate::types::{
    AnnotationId, EntityId, EntityLinkId, ExternalIdType, ResearchUrlId, ResearchUrlStatus,
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
    E: Ord + Clone + Eq + Hash + Copy + Debug + serde::Serialize,
    S: Ord + Clone + Eq + Hash + Copy + Debug + serde::Serialize,
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

        // INSERT OR IGNORE handles duplicates; then SELECT to get the id
        // (either our new one or the pre-existing one).
        let new_id = ResearchUrlId::generate();
        let inserted = queries::CREATE_URL_OR_IGNORE
            .query()
            .bind(&new_id)
            .bind(&url_str)
            .bind(ResearchUrlStatus::Pending)
            .bind(0_i32)
            .bind(None::<String>) // worker_affinity
            .bind(timestamp)
            .execute(&mut *tx)
            .await?;

        let url_id = if inserted.rows_affected() > 0 {
            result.images_created += 1;
            new_id
        } else {
            let (id,): (ResearchUrlId,) = sqlx::query_as(queries::GET_URL_BY_URL.sql)
                .bind(&url_str)
                .fetch_one(&mut *tx)
                .await?;
            id
        };

        source_id_map.insert(source_idx, url_id);
    }

    // Pre-group annotations by entity to avoid O(E*A) scan in the entity loop.
    let mut annotations_by_entity: HashMap<E, Vec<_>> = HashMap::new();
    for annotation in &bundle.annotations {
        annotations_by_entity
            .entry(annotation.entity)
            .or_default()
            .push(annotation);
    }

    // TODO: For full-dump ingestion (millions of entities), batch inserts using
    // json_each (like CREATE_URLS_BATCH) with chunked flushes would be much faster
    // than individual INSERT per link/external-id/annotation.

    // Phase 2: Load entities (insert every entity unconditionally)
    let mut entity_id_map: HashMap<E, EntityId> = HashMap::new();

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

        let new_id = EntityId::generate();
        queries::INSERT_ENTITY
            .query()
            .bind(&new_id)
            .bind(&entity_json)
            .bind(earliest)
            .bind(latest)
            .bind(location.map(|(lat, _)| lat))
            .bind(location.map(|(_, lon)| lon))
            .bind(timestamp)
            .bind(timestamp)
            .execute(&mut *tx)
            .await?;
        result.entities_created += 1;

        // Assign administrative regions via point-in-polygon.
        if let Some((lat, lon)) = location {
            sqlx::query(queries::ASSIGN_ENTITY_REGIONS.sql)
                .bind(lon)
                .bind(lat)
                .bind(&new_id)
                .execute(&mut *tx)
                .await?;
        }

        // Extract and insert all external IDs from links
        for ext_id in extract_external_ids(&links) {
            queries::INSERT_EXTERNAL_ID
                .query()
                .bind(ext_id.0)
                .bind(&ext_id.1)
                .bind(&new_id)
                .execute(&mut *tx)
                .await?;
        }

        // Insert links
        for link in &links {
            let link_id = EntityLinkId::generate();
            let target_json = serde_json::to_string(&link.target)?;
            let target_url = extract_target_url(&link.target);

            queries::INSERT_ENTITY_LINK
                .query()
                .bind(&link_id)
                .bind(&new_id)
                .bind(link.link_type)
                .bind(&target_json)
                .bind(&target_url)
                .execute(&mut *tx)
                .await?;
        }

        // Insert annotations for this entity (source_id_map is complete from Phase 1)
        for annotation in annotations_by_entity.get(&entity_idx).into_iter().flatten() {
            let uid = source_id_map.get(&annotation.source).ok_or_else(|| {
                DbError::InvalidBundle(vec![format!(
                    "annotation references unmapped source {:?}",
                    annotation.source
                )])
            })?;

            let annotation_id = AnnotationId::generate();
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

        let relation_type = relation.relation_type;
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
    use chronoscope_core::entity::{Entity, EntityRelation, EntityRelationType, EntityTransition};
    use chronoscope_core::ids::WikidataEntityId;
    use chronoscope_core::ingestion::{ImageSource, TestBundle};
    use chronoscope_core::links::{ExternalLink, LinkTarget, LinkType};
    use chronoscope_core::{Cited, DatePrecision, Location, UncertainDate, UnresolvedLocation};
    use oxilangtag::LanguageTag;

    use super::*;
    use crate::Database;
    use crate::queue::{Queue, url_queue_config};
    use crate::row;

    fn test_entity(name: &str) -> Entity<&'static str, &'static str> {
        #[allow(clippy::expect_used)]
        Entity {
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

    fn test_entity_with_date(name: &str, year: i32) -> Entity<&'static str, &'static str> {
        #[allow(clippy::expect_used)]
        let date = UncertainDate::with_precision(
            NaiveDate::from_ymd_opt(year, 1, 1).expect("valid date"),
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
    /// Returns the bundle plus the entity and link key vectors for further customization.
    fn bundle_with_entities(
        entries: &[(&str, &str)],
    ) -> (TestBundle, Vec<&'static str>, Vec<&'static str>) {
        // Leak string keys for 'static lifetime in tests (small, bounded set).
        static ENTITY_KEYS: &[&str] = &["e0", "e1", "e2", "e3", "e4"];
        static LINK_KEYS: &[&str] = &["l0", "l1", "l2", "l3", "l4"];

        let mut entities = BTreeMap::new();
        let mut external_links = BTreeMap::new();
        let mut entity_links = BTreeMap::new();
        let mut eidxs = Vec::new();
        let mut lidxs = Vec::new();

        for (i, (name, qid)) in entries.iter().enumerate() {
            let ei = ENTITY_KEYS[i];
            let li = LINK_KEYS[i];
            entities.insert(ei, test_entity(name));
            external_links.insert(li, wikidata_link(qid));
            entity_links.insert(ei, vec![li]);
            eidxs.push(ei);
            lidxs.push(li);
        }

        let bundle = TestBundle {
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

    /// Add an image to a bundle, returning its source key.
    #[allow(clippy::expect_used)]
    fn add_image(bundle: &mut TestBundle, url: &str) -> &'static str {
        static SOURCE_KEYS: &[&str] = &["s0", "s1", "s2", "s3", "s4"];
        let idx = SOURCE_KEYS[bundle.images.len()];
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

    fn test_bundle() -> TestBundle {
        bundle_with_entities(&[("Eiffel Tower", "Q243")]).0
    }

    async fn test_db() -> DbResult<Database> {
        Database::new_without_plan_verification("sqlite::memory:", &crate::resolve_regions_db()?)
            .await
    }

    #[tokio::test]
    async fn load_bundle_creates_entity() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = test_db().await?;
        let bundle = test_bundle();

        let result = load_bundle(db.pool_ref(), &bundle).await?;
        assert_eq!(result.entities_created, 1);

        // Verify entity exists via external ID lookup
        let row: Option<row::Entity> = sqlx::query_as(queries::FIND_ENTITIES_BY_EXTERNAL_ID.sql)
            .bind("wikidata")
            .bind("Q243")
            .fetch_optional(db.pool_ref())
            .await?;
        let stored = row.ok_or("entity not found")?.into_domain()?;
        assert_eq!(stored.entity.names.len(), 1);
        assert_eq!(stored.entity.names[0].value.name, "Eiffel Tower");
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

        let row: Option<row::Entity> = sqlx::query_as(queries::FIND_ENTITIES_BY_EXTERNAL_ID.sql)
            .bind("wikidata")
            .bind("Q243")
            .fetch_optional(db.pool_ref())
            .await?;
        let row = row.ok_or("entity not found")?;
        // completed_at (year 1889) should populate latest_date
        let latest = row.latest_date.ok_or("no latest")?;
        assert_eq!(latest.year(), 1889);
        Ok(())
    }

    #[tokio::test]
    async fn load_bundle_spatial_shadow_columns()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = test_db().await?;

        let (mut bundle, eidxs, _) = bundle_with_entities(&[("Eiffel Tower", "Q243")]);
        let loc = UnresolvedLocation::Resolved(Location::point(48.8584, 2.2945)?);
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

        let row: Option<row::Entity> = sqlx::query_as(queries::FIND_ENTITIES_BY_EXTERNAL_ID.sql)
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
        bundle.entity_relations.push(EntityRelation {
            from_entity: eidxs[0],
            to_entity: "e_missing",
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
        use crate::types::{EntityLinkId, ExternalIdType};
        use chronoscope_core::links::LinkType;

        let db = test_db().await?;
        let timestamp = crate::now();

        // --- Insert two entities for relation tests ---
        let mut entity_ids = Vec::new();
        for _ in 0..2 {
            let id = EntityId::generate();
            let entity: Entity<EntityId, crate::types::SourceId> = Entity {
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

            entity_ids.push(id);
        }

        // We need at least 2 entities for relations
        let entity_a = &entity_ids[0];
        let entity_b = &entity_ids[1];

        // --- ExternalIdType: insert one row per variant ---
        for (i, id_type) in ExternalIdType::all().iter().enumerate() {
            queries::INSERT_EXTERNAL_ID
                .query()
                .bind(id_type)
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
            let link_id = EntityLinkId::generate();
            let url = format!("https://example.com/link-{i}");
            let target = LinkTarget::Url {
                url: url::Url::parse(&url)?,
            };
            let target_json = serde_json::to_string(&target)?;
            queries::INSERT_ENTITY_LINK
                .query()
                .bind(&link_id)
                .bind(entity_a)
                .bind(*lt)
                .bind(&target_json)
                .bind(&url)
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
                .bind(*rt)
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
            let ann_id = AnnotationId::generate();
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
        let bundle = TestBundle {
            entities: BTreeMap::from([("e0", test_entity("Mystery Building"))]),
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

    // ==================== Round-trip read query tests ====================

    /// Verifies the full pipeline: load bundle -> insert entity + external IDs ->
    /// query via JOIN -> deserialize JSON -> produce `Entity` with correct fields.
    /// Tests both the single-entity case and the split-entity case where multiple
    /// entities share the same external ID (e.g., lifecycle phases of one Wikidata entity).
    #[tokio::test]
    async fn find_entities_by_external_id_round_trip()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = test_db().await?;

        // Two entities sharing the same Wikidata Q-ID (entity split: the original
        // Saint Thomas Church burned in 1905, a new building was constructed in 1914.
        // Wikidata models both as Q4356655, but we split them into separate entities.)
        let bundle = TestBundle {
            entities: BTreeMap::from([
                ("e0", test_entity("Saint Thomas Church (1870)")),
                ("e1", test_entity("Saint Thomas Church (1914)")),
            ]),
            images: BTreeMap::new(),
            external_links: BTreeMap::from([("l0", wikidata_link("Q4356655"))]),
            entity_links: BTreeMap::from([("e0", vec!["l0"]), ("e1", vec!["l0"])]),
            entity_relations: vec![],
            annotations: vec![],
            notes: None,
        };

        load_bundle(db.pool_ref(), &bundle).await?;

        let results = db
            .find_entities_by_external_id(&crate::types::ExternalIdType::Wikidata, "Q4356655")
            .await?;

        assert_eq!(results.len(), 2);
        let mut names: Vec<&str> = results
            .iter()
            .map(|e| e.entity.names[0].value.name.as_str())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["Saint Thomas Church (1870)", "Saint Thomas Church (1914)"]
        );

        // Both should be distinct entities
        assert_ne!(results[0].id, results[1].id);
        Ok(())
    }

    /// Verifies `LinkType` `sqlx::Type` mapping round-trips correctly through the DB.
    #[tokio::test]
    async fn find_entity_links_round_trip() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    {
        let db = test_db().await?;
        let bundle = test_bundle(); // has one Wikidata same_as link

        load_bundle(db.pool_ref(), &bundle).await?;

        let stored = db
            .find_entities_by_external_id(&crate::types::ExternalIdType::Wikidata, "Q243")
            .await?
            .into_iter()
            .next()
            .ok_or("entity not found")?;

        let links = db.find_entity_links(&stored.id).await?;
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].link_type,
            chronoscope_core::links::LinkType::SameAs
        );
        // target is the parsed LinkTarget, not a raw JSON string
        assert!(matches!(
            links[0].target,
            chronoscope_core::links::LinkTarget::Wikidata { .. }
        ));
        Ok(())
    }

    /// Verifies `AnnotationKind` `sqlx::Type` mapping and both query paths
    /// (by entity, by URL) return consistent results.
    #[tokio::test]
    async fn find_annotations_round_trip() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = test_db().await?;

        let (mut bundle, eidxs, _) = bundle_with_entities(&[("Eiffel Tower", "Q243")]);
        let s0 = add_image(&mut bundle, "https://example.com/eiffel.jpg");
        bundle.annotations.push(Annotation {
            source: s0,
            entity: eidxs[0],
            kind: AnnotationKind::ExteriorView { region: None },
        });

        load_bundle(db.pool_ref(), &bundle).await?;

        let stored = db
            .find_entities_by_external_id(&crate::types::ExternalIdType::Wikidata, "Q243")
            .await?
            .into_iter()
            .next()
            .ok_or("entity not found")?;

        // By entity
        let by_entity = db.find_annotations_by_entity(&stored.id).await?;
        assert_eq!(by_entity.len(), 1);
        assert!(matches!(
            by_entity[0].kind,
            AnnotationKind::ExteriorView { .. }
        ));
        assert_eq!(by_entity[0].entity_id, stored.id);

        // By URL
        let by_url = db.find_annotations_by_url(&by_entity[0].url_id).await?;
        assert_eq!(by_url.len(), 1);
        assert_eq!(by_url[0].id, by_entity[0].id);
        Ok(())
    }

    /// Verifies `DateRange` assembly in `into_domain`: two separate DB columns
    /// (`earliest_date`, `latest_date`) must combine into `Option<DateRange>`.
    #[tokio::test]
    async fn stored_entity_temporal_bounds_assembled_from_shadow_columns()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = test_db().await?;

        let (mut bundle, eidxs, _) = bundle_with_entities(&[("Eiffel Tower", "Q243")]);
        bundle
            .entities
            .insert(eidxs[0], test_entity_with_date("Eiffel Tower", 1889));

        load_bundle(db.pool_ref(), &bundle).await?;

        let stored = db
            .find_entities_by_external_id(&crate::types::ExternalIdType::Wikidata, "Q243")
            .await?
            .into_iter()
            .next()
            .ok_or("entity not found")?;

        let bounds = stored.temporal_bounds.ok_or("no temporal_bounds")?;
        assert_eq!(bounds.latest.year(), 1889);
        assert_eq!(bounds.earliest.year(), 1889);
        // The deserialized Entity should also have the transition
        assert_eq!(stored.entity.transitions.len(), 1);
        Ok(())
    }
}
