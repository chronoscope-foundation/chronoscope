//! Integration tests against a pre-built Wikidata test database.
//!
//! The database is built by the Nix derivation chain (FOD → bundle → load)
//! and made available via the `WIKIDATA_TEST_DB` environment variable, which
//! is set by `nix develop` in all shell tiers.

use std::ops::Deref;

use chrono::Datelike;
use chronoscope_db::{Coordinates, Database, DateRange, Entity, ExternalIdType, ResearchUrlStatus};
use tempfile::NamedTempFile;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Writable copy of the pre-built test database.
///
/// Private fields prevent separating the `Database` handle from the temp file.
/// Implements `Deref<Target = Database>` so callers use it transparently.
struct TestDb {
    db: Database,
    _file: NamedTempFile,
}

impl Deref for TestDb {
    type Target = Database;
    fn deref(&self) -> &Database {
        &self.db
    }
}

async fn wikidata_db() -> Result<TestDb> {
    let store_path = std::env::var("WIKIDATA_TEST_DB")
        .map_err(|_| "WIKIDATA_TEST_DB not set — run tests inside `nix develop`")?;
    let tmp = NamedTempFile::new()?;
    std::fs::copy(format!("{store_path}/wikidata.db"), tmp.path())?;
    let db = Database::new(
        &format!("sqlite:{}?mode=rwc", tmp.path().display()),
        &chronoscope_db::resolve_regions_db()?,
    )
    .await?;
    Ok(TestDb { db, _file: tmp })
}

/// Look up exactly one entity by Wikidata Q-ID.
///
/// The `name` parameter is a human-readable label for diagnostics only.
/// Ingestion no longer derives names from Wikidata labels — only from
/// property-backed name claims like P1448 (official name) — so most test
/// entities have no names at all. Name content is asserted explicitly by
/// the tests that exercise a P1448-bearing entity, not here.
async fn lookup_one(db: &TestDb, qid: &str, name: &str) -> Result<Entity> {
    let mut results = db
        .find_entities_by_external_id(&ExternalIdType::Wikidata, qid)
        .await?;
    assert_eq!(
        results.len(),
        1,
        "expected exactly 1 entity for {qid} ({name})"
    );
    Ok(results.remove(0))
}

fn bounds(entity: &Entity) -> Result<&DateRange> {
    entity
        .temporal_bounds
        .as_ref()
        .ok_or_else(|| format!("entity {} has no temporal bounds", entity.id).into())
}

fn location(entity: &Entity) -> Result<Coordinates> {
    entity
        .location
        .ok_or_else(|| format!("entity {} has no location", entity.id).into())
}

fn assert_coord(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 0.001,
        "coordinate mismatch: got {actual}, expected {expected}"
    );
}

#[tokio::test]
async fn entity_count() -> Result<()> {
    let db = wikidata_db().await?;
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM entities")
        .fetch_one(db.pool_ref())
        .await?;
    // 21 Q-IDs (9 original + 12 Italian for cluster tests), but Chioggia
    // Cathedral splits into 2 (demolished + rebuilt) so 22 entities total.
    assert_eq!(count.0, 22);
    Ok(())
}

#[tokio::test]
async fn eiffel_tower() -> Result<()> {
    let db = wikidata_db().await?;
    let eiffel = lookup_one(&db, "Q243", "Eiffel Tower").await?;

    let b = bounds(&eiffel)?;
    assert_eq!(b.earliest.year(), 1887);
    assert_eq!(b.latest.year(), 1889);

    let loc = location(&eiffel)?;
    assert_coord(loc.lat, 48.858296);
    assert_coord(loc.lon, 2.294479);

    let links = db.find_entity_links(&eiffel.id).await?;
    assert_eq!(links.len(), 152);

    let annotations = db.find_annotations_by_entity(&eiffel.id).await?;
    assert_eq!(annotations.len(), 1);

    Ok(())
}

#[tokio::test]
async fn brooklyn_bridge() -> Result<()> {
    let db = wikidata_db().await?;
    let bridge = lookup_one(&db, "Q125006", "Brooklyn Bridge").await?;

    let b = bounds(&bridge)?;
    assert_eq!(b.earliest.year(), 1869);
    assert_eq!(b.latest.year(), 1883);

    let loc = location(&bridge)?;
    assert_coord(loc.lat, 40.7057);
    assert_coord(loc.lon, -73.9963);

    let links = db.find_entity_links(&bridge.id).await?;
    assert_eq!(links.len(), 76);

    let annotations = db.find_annotations_by_entity(&bridge.id).await?;
    assert_eq!(annotations.len(), 4);

    Ok(())
}

#[tokio::test]
async fn hagia_sophia() -> Result<()> {
    let db = wikidata_db().await?;
    let sophia = lookup_one(&db, "Q12506", "Hagia Sophia").await?;

    let b = bounds(&sophia)?;
    assert_eq!(b.earliest.year(), 537);
    assert_eq!(b.latest.year(), 1054);

    let links = db.find_entity_links(&sophia.id).await?;
    assert_eq!(links.len(), 117);

    let annotations = db.find_annotations_by_entity(&sophia.id).await?;
    assert_eq!(annotations.len(), 4);

    Ok(())
}

/// P18 (Wikidata "image" property) values become pending research URLs
/// with `ExteriorView` annotations linking them to their entities.
#[tokio::test]
async fn p18_images_as_pending_research_urls() -> Result<()> {
    let db = wikidata_db().await?;
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM research_urls WHERE status = ?")
        .bind(ResearchUrlStatus::Pending)
        .fetch_one(db.pool_ref())
        .await?;
    assert_eq!(count.0, 52);

    // Split entities (e.g., Chioggia Cathedral) only attach images to the
    // latest entity, so the old cathedral no longer gets a duplicate annotation.
    let annotation_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM annotations")
        .fetch_one(db.pool_ref())
        .await?;
    assert_eq!(annotation_count.0, 52);

    Ok(())
}

/// Chioggia Cathedral was destroyed in 1623 and rebuilt in 1633. The ingestion
/// pipeline's `split_on_rebuild` logic detects the demolish→construct pattern
/// and produces two separate entities sharing the same Wikidata Q-ID.
#[tokio::test]
async fn chioggia_cathedral_splits_on_rebuild() -> Result<()> {
    let db = wikidata_db().await?;
    let results = db
        .find_entities_by_external_id(&ExternalIdType::Wikidata, "Q1111481")
        .await?;
    assert_eq!(results.len(), 2, "should split into old + new cathedral");

    // Sort by earliest date to get old first, new second
    let mut sorted = results;
    sorted.sort_by_key(|e| e.temporal_bounds.as_ref().map(|b| b.earliest));

    let old = &sorted[0];
    let old_bounds = bounds(old)?;
    assert_eq!(old_bounds.earliest.year(), 1623);

    let new = &sorted[1];
    let new_bounds = bounds(new)?;
    assert_eq!(new_bounds.earliest.year(), 1633);

    // Both should have distinct DB IDs
    assert_ne!(old.id, new.id);

    // Both should have the same location (predecessor inherits from successor)
    let old_loc = old.location.ok_or("old cathedral should have location")?;
    let new_loc = new.location.ok_or("new cathedral should have location")?;
    assert_eq!(
        old_loc, new_loc,
        "both cathedrals should be at the same site"
    );

    // The split still produces two entities, but the old-model Replaces
    // relation between them is no longer emitted (synthetic "lifecycle"
    // property markers were retired), so no entity_relations exist.
    let relation_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM entity_relations")
        .fetch_one(db.pool_ref())
        .await?;
    assert_eq!(relation_count.0, 0);

    Ok(())
}

#[tokio::test]
async fn saint_thomas_church() -> Result<()> {
    let db = wikidata_db().await?;
    let church = lookup_one(&db, "Q4356655", "Saint Thomas Church").await?;

    let b = bounds(&church)?;
    assert_eq!(b.earliest.year(), 1913);

    let loc = location(&church)?;
    assert_coord(loc.lat, 40.7608);

    Ok(())
}

#[tokio::test]
async fn vanderbilt_entities() -> Result<()> {
    let db = wikidata_db().await?;

    let cornelius = lookup_one(&db, "Q5171466", "Cornelius Vanderbilt II House").await?;
    let b = bounds(&cornelius)?;
    assert_eq!(b.earliest.year(), 1883);

    let _william = lookup_one(&db, "Q5652831", "William K. Vanderbilt House").await?;

    let triple = lookup_one(&db, "Q108584685", "Vanderbilt Triple Palace").await?;
    let loc = location(&triple)?;
    assert_coord(loc.lat, 40.7596);

    Ok(())
}

/// Names are derived only from property-backed claims (P1448, official
/// name), not from Wikidata labels. Ponte Vecchio carries a French P1448
/// claim, so it is one of the few test entities with a name.
#[tokio::test]
async fn official_name_from_p1448() -> Result<()> {
    let db = wikidata_db().await?;
    let ponte = lookup_one(&db, "Q208633", "Ponte Vecchio").await?;
    let has_official_name = ponte
        .entity
        .names
        .iter()
        .any(|n| n.value.name == "Ponte vecchio -  Point Vieux");
    assert!(
        has_official_name,
        "Q208633 should carry its P1448 official name, got {:?}",
        ponte.entity.names
    );
    Ok(())
}
