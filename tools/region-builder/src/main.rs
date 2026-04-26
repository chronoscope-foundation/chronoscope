//! Builds a SpatiaLite regions database from an OSM PBF file.
//!
//! Uses cosmogony as a library to extract administrative zones directly
//! from the PBF, then writes them into a SpatiaLite SQLite database with
//! R-tree spatial indexes.
//!
//! Usage:
//!   region-builder --input italy.osm.pbf --output regions.sqlite

use std::path::PathBuf;

use cosmogony::{Zone, ZoneType};
use cosmogony_builder::build_cosmogony;
use geojson::Value as GeoJsonValue;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::{ConnectOptions, Connection, Executor, Row};

fn parse_args() -> (PathBuf, PathBuf) {
    let mut args = std::env::args().skip(1);
    let mut input = None;
    let mut output = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => input = args.next().map(PathBuf::from),
            "--output" => output = args.next().map(PathBuf::from),
            other => {
                eprintln!("Unknown argument: {other}");
                std::process::exit(1);
            }
        }
    }

    match (input, output) {
        (Some(i), Some(o)) => (i, o),
        _ => {
            eprintln!("Usage: region-builder --input <pbf> --output <sqlite>");
            std::process::exit(1);
        }
    }
}

/// Extract the OSM relation ID from cosmogony's osm_id string.
/// Format is "relation:12345" or "node:12345". We only want relations.
fn osm_relation_id(osm_id: &str) -> Option<i64> {
    osm_id
        .strip_prefix("relation:")
        .and_then(|s| s.parse().ok())
}

/// Convert a geo_types MultiPolygon to a GeoJSON string for SpatiaLite.
fn multipolygon_to_geojson(mp: &geo_types::MultiPolygon<f64>) -> String {
    let geojson_geom = geojson::Geometry::new(GeoJsonValue::from(mp));
    geojson_geom.to_string()
}

/// Convert cosmogony's ZoneType to its DB column string.
///
/// We store ALL cosmogony zone types in the regions DB, not just the ones
/// we currently cluster on. The cluster queries filter by zone_type at
/// query time (using `chronoscope_api_client::ZoneType` which only has
/// the 4 levels we cluster at: country, state, state_district, city).
/// Storing all types means we don't need to rebuild the regions DB if
/// we later add finer clustering levels like city_district.
fn zone_type_str(zt: &ZoneType) -> &'static str {
    use chronoscope_api_client::ZoneType as OurZoneType;
    match zt {
        // Variants that map to our ZoneType — use as_ref() for compile-time sync.
        ZoneType::Country => OurZoneType::Country.as_ref(),
        ZoneType::State => OurZoneType::State.as_ref(),
        ZoneType::StateDistrict => OurZoneType::StateDistrict.as_ref(),
        ZoneType::City => OurZoneType::City.as_ref(),
        // Cosmogony-only variants stored but not currently queried for clustering.
        ZoneType::CountryRegion => "country_region",
        ZoneType::CityDistrict => "city_district",
        ZoneType::Suburb => "suburb",
        ZoneType::NonAdministrative => "non_administrative",
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (input_path, output_path) = parse_args();

    // Remove output if it exists (fresh build each time).
    if output_path.exists() {
        std::fs::remove_file(&output_path)?;
    }

    // --- Phase 1: Build cosmogony from PBF ---
    eprintln!("Building cosmogony from {}", input_path.display());
    let cosmogony = build_cosmogony(
        input_path.to_string_lossy().to_string(),
        None,  // no country code filter
        false, // don't disable voronoi
        &[],   // no language filter
    )?;
    eprintln!(
        "Cosmogony built: {} zones from {}",
        cosmogony.zones.len(),
        cosmogony.meta.osm_filename
    );

    // --- Phase 2: Filter and collect zones ---
    let zones: Vec<&Zone> = cosmogony
        .zones
        .iter()
        .filter(|z| {
            z.zone_type
                .as_ref()
                .map_or(false, |zt| !matches!(zt, ZoneType::NonAdministrative))
        })
        .filter(|z| z.boundary.is_some())
        .filter(|z| osm_relation_id(&z.osm_id).is_some())
        .collect();

    eprintln!(
        "Filtered to {} zones (excluded {} without boundary/relation/zone_type)",
        zones.len(),
        cosmogony.zones.len() - zones.len()
    );

    // Build index→osm_id map for parent resolution.
    let index_to_osm_id: std::collections::HashMap<usize, i64> = cosmogony
        .zones
        .iter()
        .filter_map(|z| {
            let rel_id = osm_relation_id(&z.osm_id)?;
            Some((z.id.index, rel_id))
        })
        .collect();

    // --- Phase 3: Open SQLite + SpatiaLite ---
    let spatialite_dir = std::env::var("SPATIALITE_LIBRARY_PATH").map_err(|_| {
        anyhow::anyhow!("SPATIALITE_LIBRARY_PATH must be set (are you in a Nix shell?)")
    })?;

    let options = SqliteConnectOptions::new()
        .filename(&output_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .extension(format!("{spatialite_dir}/mod_spatialite"));

    let mut conn = options.connect().await?;
    conn.execute("SELECT InitSpatialMetaData(1)").await?;

    // Create regions table. No FK on parent_osm_id: a zone's parent may
    // be a zone type we filtered out (e.g., CityDistrict).
    conn.execute(
        "CREATE TABLE regions (
            osm_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            zone_type TEXT NOT NULL,
            admin_level INTEGER NOT NULL,
            parent_osm_id INTEGER,
            country_code TEXT,
            wikidata_id TEXT,
            international_names TEXT
        )",
    )
    .await?;

    conn.execute("SELECT AddGeometryColumn('regions', 'geometry', 4326, 'MULTIPOLYGON', 'XY')")
        .await?;

    conn.execute("CREATE INDEX idx_regions_zone_type ON regions(zone_type)")
        .await?;
    conn.execute("CREATE INDEX idx_regions_parent ON regions(parent_osm_id)")
        .await?;
    conn.execute(
        "CREATE INDEX idx_regions_wikidata ON regions(wikidata_id) WHERE wikidata_id IS NOT NULL",
    )
    .await?;

    // --- Phase 4: Insert zones ---
    eprintln!("Inserting zones into SpatiaLite database...");
    conn.execute("BEGIN IMMEDIATE").await?;

    let mut inserted = 0u64;
    let mut duplicates = 0u64;

    for zone in &zones {
        let osm_rel_id = osm_relation_id(&zone.osm_id).expect("filtered in phase 2");

        let boundary = zone.boundary.as_ref().expect("filtered in phase 2");

        let zone_type = zone.zone_type.as_ref().expect("filtered in phase 2");

        let admin_level = zone.admin_level.unwrap_or(0) as i64;

        let parent_osm_id: Option<i64> = zone
            .parent
            .and_then(|idx| index_to_osm_id.get(&idx.index).copied());

        let names_json = if zone.international_names.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&zone.international_names)?)
        };

        let geojson_str = multipolygon_to_geojson(boundary);

        let result = sqlx::query(
            "INSERT OR IGNORE INTO regions
                (osm_id, name, zone_type, admin_level, parent_osm_id,
                 country_code, wikidata_id, international_names,
                 geometry)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                    SetSRID(GeomFromGeoJSON(?9), 4326)",
        )
        .bind(osm_rel_id)
        .bind(&zone.name)
        .bind(zone_type_str(zone_type))
        .bind(admin_level)
        .bind(parent_osm_id)
        .bind(zone.country_code.as_deref())
        .bind(zone.wikidata.as_deref())
        .bind(names_json.as_deref())
        .bind(&geojson_str)
        .execute(&mut conn)
        .await
        .map_err(|e| {
            anyhow::anyhow!("Failed to insert zone {} ({}): {e}", osm_rel_id, zone.name)
        })?;

        if result.rows_affected() > 0 {
            inserted += 1;
            if inserted % 1000 == 0 {
                eprintln!("  ... {inserted} zones inserted");
            }
        } else {
            // INSERT OR IGNORE: rows_affected == 0 means PK conflict
            eprintln!(
                "Warning: duplicate osm_id {} ({}) — skipping",
                osm_rel_id, zone.name
            );
            duplicates += 1;
        }
    }

    conn.execute("COMMIT").await?;
    eprintln!("Inserted {inserted} zones ({duplicates} duplicates skipped)");

    // --- Phase 5: Validate + index ---

    // Hard-fail on NULL geometry (GeomFromGeoJSON silently returns NULL on
    // invalid input — catch it here rather than at query time).
    let null_geom: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM regions WHERE geometry IS NULL")
        .fetch_one(&mut conn)
        .await?;
    if null_geom.0 > 0 {
        anyhow::bail!(
            "{} region rows have NULL geometry — refusing to ship a broken DB",
            null_geom.0
        );
    }

    eprintln!("Creating spatial index...");
    let index_result: (i64,) = sqlx::query_as("SELECT CreateSpatialIndex('regions', 'geometry')")
        .fetch_one(&mut conn)
        .await?;
    if index_result.0 != 1 {
        anyhow::bail!("CreateSpatialIndex returned {}, expected 1", index_result.0);
    }

    eprintln!("Running ANALYZE...");
    conn.execute("ANALYZE").await?;

    // Summary
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM regions")
        .fetch_one(&mut conn)
        .await?;
    eprintln!("Done. Database contains {} regions.", count.0);

    let rows = sqlx::query(
        "SELECT zone_type, COUNT(*) as cnt FROM regions GROUP BY zone_type ORDER BY cnt DESC",
    )
    .fetch_all(&mut conn)
    .await?;
    for row in &rows {
        let zt: &str = row.get("zone_type");
        let cnt: i64 = row.get("cnt");
        eprintln!("  {zt}: {cnt}");
    }

    conn.close().await?;
    Ok(())
}
