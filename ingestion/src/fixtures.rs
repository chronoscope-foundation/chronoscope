//! Test fixtures for entity lifecycle scenarios.
//!
//! Fixtures are organized as [`TestBundle`]s — groups of related entities with
//! their relationships, mirroring how data arrives through ingestion.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use chrono::NaiveDate;
use oxilangtag::LanguageTag;

use chronoscope_core::ingestion::TestBundle;
use chronoscope_core::{
    Cited, DateBound, DatePrecision, Entity, EntityName, EntityRelation, EntityRelationType,
    EntityTransition, GeoPoint, Location, MoveMethod, NameType, UncertainDate, UnresolvedLocation,
    Usage,
};

type FixtureEntity = Entity<&'static str>;

/// Fixture builders compose smart constructors from several core modules
/// ([`UncertainDate`], [`GeoPoint`], [`Location`]) plus BCP 47 tag parsing,
/// each with its own error type. The hardcoded inputs are expected to be
/// valid, but we propagate the constructor errors rather than panicking so a
/// mistyped literal surfaces as a test failure with the real cause attached.
type FixtureResult<T> = Result<T, Box<dyn std::error::Error>>;

fn year(y: i32) -> FixtureResult<UncertainDate> {
    Ok(UncertainDate::with_precision(
        NaiveDate::from_ymd_opt(y, 1, 1).ok_or("fixture year is not a valid calendar date")?,
        DatePrecision::Year,
    )?)
}

fn exact(y: i32, m: u32, d: u32) -> FixtureResult<UncertainDate> {
    Ok(UncertainDate::with_precision(
        NaiveDate::from_ymd_opt(y, m, d).ok_or("fixture date is not a valid calendar date")?,
        DatePrecision::Day,
    )?)
}

fn range(y1: i32, y2: i32) -> FixtureResult<UncertainDate> {
    Ok(UncertainDate::bounded(
        Some(DateBound::new(
            NaiveDate::from_ymd_opt(y1, 1, 1)
                .ok_or("fixture range start is not a valid calendar date")?,
            DatePrecision::Year,
        )?),
        Some(DateBound::new(
            NaiveDate::from_ymd_opt(y2, 1, 1)
                .ok_or("fixture range end is not a valid calendar date")?,
            DatePrecision::Year,
        )?),
    )?)
}

fn uncited<T>(value: T) -> Cited<T, &'static str> {
    Cited::uncited(value)
}

/// Parse a hardcoded BCP 47 language tag. Only used with string literals.
fn lang(tag: &str) -> FixtureResult<LanguageTag<String>> {
    Ok(LanguageTag::parse(tag.to_string())?)
}

fn coords(lat: f64, lon: f64, radius_m: Option<f64>) -> FixtureResult<UnresolvedLocation> {
    let center = GeoPoint::new(lat, lon)?;
    let location = match radius_m {
        Some(r) => Location::circle(center, r)?,
        None => Location::point(center),
    };
    Ok(UnresolvedLocation::Resolved(location))
}

fn en_name(name: &str, name_type: NameType) -> FixtureResult<Cited<EntityName, &'static str>> {
    Ok(uncited(EntityName {
        name: name.to_string(),
        name_type,
        language: lang("en")?,
        valid_from: None,
        valid_to: None,
    }))
}

fn name_with_validity(
    name: &str,
    name_type: NameType,
    language: &str,
    valid_from: Option<UncertainDate>,
    valid_to: Option<UncertainDate>,
) -> FixtureResult<Cited<EntityName, &'static str>> {
    Ok(uncited(EntityName {
        name: name.to_string(),
        name_type,
        language: lang(language)?,
        valid_from,
        valid_to,
    }))
}

fn penn_station_original() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("Pennsylvania Station", NameType::Official)?],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(1904)?)),
                completed_at: Some(uncited(year(1910)?)),
                location: Some(uncited(coords(40.7505, -73.9934, Some(10.0))?)),
                trigger_event: None,
            },
            EntityTransition::Modified {
                started_at: Some(uncited(exact(1963, 10, 28)?)),
                completed_at: Some(uncited(exact(1964, 10, 28)?)),
                description: Some(
                    "above-ground structure demolished, underground portions remain".to_string(),
                ),
                trigger_event: None,
            },
        ],
    })
}

fn madison_square_garden() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("Madison Square Garden", NameType::Official)?],
        transitions: vec![EntityTransition::Constructed {
            started_at: Some(uncited(year(1964)?)),
            completed_at: Some(uncited(year(1968)?)),
            location: Some(uncited(coords(40.7505, -73.9934, Some(10.0))?)),
            trigger_event: None,
        }],
    })
}

fn palace_of_fine_arts_original() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("Palace of Fine Arts", NameType::Official)?],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(1914)?)),
                completed_at: Some(uncited(year(1915)?)),
                location: Some(uncited(coords(37.8029, -122.4484, Some(20.0))?)),
                trigger_event: None,
            },
            EntityTransition::Demolished {
                started_at: Some(uncited(year(1964)?)),
                completed_at: Some(uncited(year(1964)?)),
                cause: Some("deterioration of temporary materials".to_string()),
                trigger_event: None,
            },
        ],
    })
}

fn palace_of_fine_arts_rebuilt() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("Palace of Fine Arts", NameType::Official)?],
        transitions: vec![EntityTransition::Constructed {
            started_at: Some(uncited(year(1964)?)),
            completed_at: Some(uncited(year(1967)?)),
            location: Some(uncited(coords(37.8029, -122.4484, Some(20.0))?)),
            trigger_event: None,
        }],
    })
}

fn statue_of_liberty() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![
            en_name("Statue of Liberty", NameType::Common)?,
            en_name("Liberty Enlightening the World", NameType::Official)?,
        ],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(1876)?)),
                completed_at: Some(uncited(year(1884)?)),
                location: Some(uncited(coords(48.8566, 2.3522, Some(100.0))?)),
                trigger_event: None,
            },
            EntityTransition::Moved {
                occurred_at: Some(uncited(range(1885, 1886)?)),
                location: Some(uncited(coords(40.6892, -74.0445, Some(10.0))?)),
                cause: Some("gift from France to United States".to_string()),
                method: Some(MoveMethod::Disassembled),
                trigger_event: None,
            },
            EntityTransition::Modified {
                started_at: Some(uncited(year(1984)?)),
                completed_at: Some(uncited(year(1986)?)),
                description: Some(
                    "internal iron structure replaced with stainless steel".to_string(),
                ),
                trigger_event: None,
            },
        ],
    })
}

fn route_66() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("U.S. Route 66", NameType::Official)?],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(1926)?)),
                completed_at: Some(uncited(year(1938)?)),
                location: None,
                trigger_event: None,
            },
            EntityTransition::UsageModified {
                occurred_at: Some(uncited(exact(1985, 6, 27)?)),
                new_usages: BTreeSet::new(),
                description: Some("officially decommissioned from US Highway System".to_string()),
                trigger_event: None,
            },
        ],
    })
}

fn route_66_illinois_segment() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("Route 66 Illinois segment", NameType::Common)?],
        transitions: vec![EntityTransition::Modified {
            started_at: Some(uncited(year(1990)?)),
            completed_at: None,
            description: Some("designated as Historic Route 66".to_string()),
            trigger_event: None,
        }],
    })
}

fn route_66_arizona_segment() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("Route 66 Arizona segment", NameType::Common)?],
        transitions: vec![EntityTransition::Modified {
            started_at: Some(uncited(year(1984)?)),
            completed_at: None,
            description: Some("incorporated into I-40".to_string()),
            trigger_event: None,
        }],
    })
}

fn high_line() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![
            en_name("High Line", NameType::Common)?,
            name_with_validity(
                "West Side Elevated Line",
                NameType::Official,
                "en",
                Some(year(1934)?),
                Some(year(1980)?),
            )?,
        ],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(1929)?)),
                completed_at: Some(uncited(year(1934)?)),
                location: None,
                trigger_event: None,
            },
            EntityTransition::UsageModified {
                occurred_at: Some(uncited(year(1934)?)),
                new_usages: BTreeSet::from([Usage::Transportation]),
                description: Some("opened for freight rail service".to_string()),
                trigger_event: None,
            },
            EntityTransition::UsageModified {
                occurred_at: Some(uncited(year(1980)?)),
                new_usages: BTreeSet::new(),
                description: Some("last train ran, line abandoned".to_string()),
                trigger_event: None,
            },
            EntityTransition::Modified {
                started_at: Some(uncited(year(2009)?)),
                completed_at: Some(uncited(year(2014)?)),
                description: Some("converted to elevated linear park".to_string()),
                trigger_event: None,
            },
            EntityTransition::UsageModified {
                occurred_at: Some(uncited(exact(2009, 6, 9)?)),
                new_usages: BTreeSet::from([Usage::Cultural]),
                description: Some("opened to public as park".to_string()),
                trigger_event: None,
            },
        ],
    })
}

fn berlin_wall() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![
            en_name("Berlin Wall", NameType::Common)?,
            uncited(EntityName {
                name: "Berliner Mauer".to_string(),
                name_type: NameType::Official,
                language: lang("de")?,
                valid_from: None,
                valid_to: None,
            }),
        ],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(exact(1961, 8, 13)?)),
                completed_at: Some(uncited(range(1961, 1975)?)),
                location: None,
                trigger_event: None,
            },
            EntityTransition::Modified {
                started_at: Some(uncited(exact(1989, 11, 9)?)),
                completed_at: Some(uncited(year(1992)?)),
                description: Some(
                    "partially demolished, some sections preserved as memorial".to_string(),
                ),
                trigger_event: None,
            },
        ],
    })
}

fn east_side_gallery() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![
            en_name("East Side Gallery", NameType::Common)?,
            uncited(EntityName {
                name: "East Side Gallery".to_string(),
                name_type: NameType::Common,
                language: lang("de")?,
                valid_from: None,
                valid_to: None,
            }),
        ],
        transitions: vec![EntityTransition::Modified {
            started_at: Some(uncited(year(1990)?)),
            completed_at: None,
            description: Some("preserved and converted to open-air gallery".to_string()),
            trigger_event: None,
        }],
    })
}

fn burning_man_2023() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("Black Rock City 2023", NameType::Official)?],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(exact(2023, 8, 20)?)),
                completed_at: Some(uncited(exact(2023, 8, 27)?)),
                location: Some(uncited(coords(40.7864, -119.2065, Some(100.0))?)),
                trigger_event: None,
            },
            EntityTransition::Demolished {
                started_at: Some(uncited(exact(2023, 9, 4)?)),
                completed_at: Some(uncited(exact(2023, 9, 5)?)),
                cause: Some("leave no trace".to_string()),
                trigger_event: None,
            },
        ],
    })
}

fn hagia_sophia() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![
            en_name("Hagia Sophia", NameType::Common)?,
            uncited(EntityName {
                name: "Ayasofya".to_string(),
                name_type: NameType::Common,
                language: lang("tr")?,
                valid_from: None,
                valid_to: None,
            }),
        ],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(532)?)),
                completed_at: Some(uncited(year(537)?)),
                location: Some(uncited(coords(41.0086, 28.9802, Some(20.0))?)),
                trigger_event: None,
            },
            EntityTransition::UsageModified {
                occurred_at: Some(uncited(year(537)?)),
                new_usages: BTreeSet::from([Usage::Religious]),
                description: Some("consecrated as Eastern Orthodox cathedral".to_string()),
                trigger_event: None,
            },
            EntityTransition::Modified {
                started_at: Some(uncited(year(1453)?)),
                completed_at: None,
                description: Some("minarets added".to_string()),
                trigger_event: None,
            },
            EntityTransition::UsageModified {
                occurred_at: Some(uncited(year(1453)?)),
                new_usages: BTreeSet::from([Usage::Religious]),
                description: Some("converted to mosque".to_string()),
                trigger_event: None,
            },
            EntityTransition::UsageModified {
                occurred_at: Some(uncited(year(1935)?)),
                new_usages: BTreeSet::from([Usage::Cultural]),
                description: Some("converted to museum".to_string()),
                trigger_event: None,
            },
            EntityTransition::UsageModified {
                occurred_at: Some(uncited(exact(2020, 7, 24)?)),
                new_usages: BTreeSet::from([Usage::Religious]),
                description: Some("reconverted to mosque".to_string()),
                trigger_event: None,
            },
        ],
    })
}

fn pioneer_building_seattle() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("Pioneer Building", NameType::Official)?],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(1889)?)),
                completed_at: Some(uncited(year(1892)?)),
                location: Some(uncited(coords(47.6021, -122.3319, Some(10.0))?)),
                trigger_event: None,
            },
            EntityTransition::Modified {
                started_at: Some(uncited(year(1907)?)),
                completed_at: Some(uncited(year(1910)?)),
                description: Some(
                    "street level raised, original ground floor became basement".to_string(),
                ),
                trigger_event: None,
            },
        ],
    })
}

fn kowloon_walled_city() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("Kowloon Walled City", NameType::Common)?],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(1847)?)),
                completed_at: Some(uncited(year(1847)?)),
                location: Some(uncited(coords(22.3321, 114.1907, Some(50.0))?)),
                trigger_event: None,
            },
            EntityTransition::Modified {
                started_at: Some(uncited(year(1950)?)),
                completed_at: Some(uncited(year(1970)?)),
                description: Some("organic uncontrolled growth to extreme density".to_string()),
                trigger_event: None,
            },
            EntityTransition::Demolished {
                started_at: Some(uncited(year(1993)?)),
                completed_at: Some(uncited(year(1994)?)),
                cause: Some("government clearance".to_string()),
                trigger_event: None,
            },
        ],
    })
}

// --- James River and Kanawha Canal (Richmond, VA) ---
// Exercises: parent with system-level transitions, child segments with divergent fates,
// gradual processes via date ranges, destruction/preservation/restoration patterns.

fn jrk_canal() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name(
            "James River and Kanawha Canal",
            NameType::Official,
        )?],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(1785)?)),
                completed_at: Some(uncited(year(1854)?)),
                location: None,
                trigger_event: None,
            },
            EntityTransition::Designated {
                occurred_at: Some(uncited(year(1878)?)),
                designation: "right-of-way sold to Richmond & Alleghany Railroad".to_string(),
                description: None,
                trigger_event: None,
            },
        ],
    })
}

/// Bosher Dam to Pump House Park — still an active waterway feeding city water supply.
fn jrk_canal_bosher_dam_segment() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name(
            "Bosher Dam to Pump House segment",
            NameType::Common,
        )?],
        transitions: vec![EntityTransition::Constructed {
            started_at: Some(uncited(year(1835)?)),
            completed_at: Some(uncited(year(1835)?)),
            location: None,
            trigger_event: None,
        }],
    })
}

/// Tidewater Connection Locks 1-3 — destroyed by I-195 expressway construction in 1976.
fn jrk_canal_tidewater_locks_1_3() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("Tidewater Connection Locks 1-3", NameType::Common)?],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(1850)?)),
                completed_at: Some(uncited(year(1854)?)),
                location: None,
                trigger_event: None,
            },
            EntityTransition::Demolished {
                started_at: Some(uncited(year(1976)?)),
                completed_at: Some(uncited(year(1976)?)),
                cause: Some("Downtown Expressway (I-195) construction".to_string()),
                trigger_event: None,
            },
        ],
    })
}

/// Tidewater Connection Locks 4-5 — preserved in situ by Reynolds Metals.
fn jrk_canal_tidewater_locks_4_5() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("Tidewater Connection Locks 4-5", NameType::Common)?],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(1850)?)),
                completed_at: Some(uncited(year(1854)?)),
                location: None,
                trigger_event: None,
            },
            EntityTransition::Designated {
                occurred_at: Some(uncited(year(1976)?)),
                designation: "preserved by Reynolds Metals during I-195 construction".to_string(),
                description: None,
                trigger_event: None,
            },
        ],
    })
}

/// Great Basin — progressively covered by railroad yards (1880s) then development (by 1920s).
fn jrk_canal_great_basin() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("Great Basin", NameType::Common)?],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(1800)?)),
                completed_at: Some(uncited(year(1800)?)),
                location: None,
                trigger_event: None,
            },
            EntityTransition::Modified {
                started_at: Some(uncited(range(1880, 1889)?)),
                completed_at: None,
                description: Some("railroad yards cover half of basin".to_string()),
                trigger_event: None,
            },
            EntityTransition::Demolished {
                started_at: Some(uncited(range(1880, 1889)?)),
                completed_at: Some(uncited(range(1920, 1929)?)),
                cause: Some("progressively covered by railroad yards and development".to_string()),
                trigger_event: None,
            },
        ],
    })
}

/// Canal Walk segment — buried in late 19th century, restored 1995-1999.
fn jrk_canal_canal_walk() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![
            en_name("Canal Walk", NameType::Common)?,
            name_with_validity(
                "Richmond Canal Walk",
                NameType::Official,
                "en",
                Some(exact(1999, 6, 4)?),
                None,
            )?,
        ],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(1785)?)),
                completed_at: Some(uncited(range(1785, 1790)?)),
                location: None,
                trigger_event: None,
            },
            EntityTransition::UsageModified {
                occurred_at: Some(uncited(range(1880, 1899)?)),
                new_usages: BTreeSet::new(),
                description: Some("buried beneath paving and structures".to_string()),
                trigger_event: None,
            },
            EntityTransition::Repaired {
                started_at: Some(uncited(year(1995)?)),
                completed_at: Some(uncited(exact(1999, 6, 4)?)),
                description: Some(
                    "restored as Canal Walk; 550 tons of historic canal stone recovered"
                        .to_string(),
                ),
                trigger_event: None,
            },
        ],
    })
}

/// Great Ship Lock — intact and operational since 1854, renovated 2013.
fn jrk_canal_great_ship_lock() -> FixtureResult<FixtureEntity> {
    Ok(Entity {
        names: vec![en_name("Great Ship Lock", NameType::Common)?],
        transitions: vec![
            EntityTransition::Constructed {
                started_at: Some(uncited(year(1850)?)),
                completed_at: Some(uncited(year(1854)?)),
                location: Some(uncited(coords(37.5295, -77.4175, Some(30.0))?)),
                trigger_event: None,
            },
            EntityTransition::Repaired {
                started_at: Some(uncited(year(2013)?)),
                completed_at: Some(uncited(year(2013)?)),
                description: Some(
                    "$450,000 renovation as part of Virginia Capital Trail".to_string(),
                ),
                trigger_event: None,
            },
        ],
    })
}

// --- Bundle constructors ---

fn contains(from: &'static str, to: &'static str) -> EntityRelation<&'static str, &'static str> {
    EntityRelation {
        from_entity: from,
        to_entity: to,
        relation_type: EntityRelationType::Contains,
        evidence: vec![],
    }
}

fn replaces(from: &'static str, to: &'static str) -> EntityRelation<&'static str, &'static str> {
    EntityRelation {
        from_entity: from,
        to_entity: to,
        relation_type: EntityRelationType::Replaces,
        evidence: vec![],
    }
}

/// Penn Station head house demolished, MSG built above the still-operational underground station.
fn penn_station_bundle() -> FixtureResult<TestBundle> {
    Ok(TestBundle {
        entities: BTreeMap::from([
            ("penn_station", penn_station_original()?),
            ("msg", madison_square_garden()?),
        ]),
        ..TestBundle::new()
    })
}

/// Original Palace demolished, rebuilt on same site.
fn palace_of_fine_arts_bundle() -> FixtureResult<TestBundle> {
    Ok(TestBundle {
        entities: BTreeMap::from([
            ("palace_original", palace_of_fine_arts_original()?),
            ("palace_rebuilt", palace_of_fine_arts_rebuilt()?),
        ]),
        entity_relations: vec![replaces("palace_rebuilt", "palace_original")],
        ..TestBundle::new()
    })
}

/// Route 66 parent with Illinois and Arizona segments.
fn route_66_bundle() -> FixtureResult<TestBundle> {
    Ok(TestBundle {
        entities: BTreeMap::from([
            ("route_66", route_66()?),
            ("route_66_illinois", route_66_illinois_segment()?),
            ("route_66_arizona", route_66_arizona_segment()?),
        ]),
        entity_relations: vec![
            contains("route_66", "route_66_illinois"),
            contains("route_66", "route_66_arizona"),
        ],
        ..TestBundle::new()
    })
}

/// Berlin Wall with East Side Gallery preserved section.
fn berlin_wall_bundle() -> FixtureResult<TestBundle> {
    Ok(TestBundle {
        entities: BTreeMap::from([
            ("berlin_wall", berlin_wall()?),
            ("east_side_gallery", east_side_gallery()?),
        ]),
        entity_relations: vec![contains("berlin_wall", "east_side_gallery")],
        ..TestBundle::new()
    })
}

/// James River and Kanawha Canal with all segments.
fn jrk_canal_bundle() -> FixtureResult<TestBundle> {
    Ok(TestBundle {
        entities: BTreeMap::from([
            ("jrk_canal", jrk_canal()?),
            ("jrk_bosher_dam", jrk_canal_bosher_dam_segment()?),
            ("jrk_tidewater_1_3", jrk_canal_tidewater_locks_1_3()?),
            ("jrk_tidewater_4_5", jrk_canal_tidewater_locks_4_5()?),
            ("jrk_great_basin", jrk_canal_great_basin()?),
            ("jrk_canal_walk", jrk_canal_canal_walk()?),
            ("jrk_great_ship_lock", jrk_canal_great_ship_lock()?),
        ]),
        entity_relations: vec![
            contains("jrk_canal", "jrk_bosher_dam"),
            contains("jrk_canal", "jrk_tidewater_1_3"),
            contains("jrk_canal", "jrk_tidewater_4_5"),
            contains("jrk_canal", "jrk_great_basin"),
            contains("jrk_canal", "jrk_canal_walk"),
            contains("jrk_canal", "jrk_great_ship_lock"),
        ],
        ..TestBundle::new()
    })
}

/// All fixture bundles, keyed by name.
pub(crate) fn all_bundles() -> FixtureResult<Vec<(&'static str, TestBundle)>> {
    let mut bundles = vec![
        ("penn_station", penn_station_bundle()?),
        ("palace_of_fine_arts", palace_of_fine_arts_bundle()?),
        ("route_66", route_66_bundle()?),
        ("berlin_wall", berlin_wall_bundle()?),
        ("jrk_canal", jrk_canal_bundle()?),
    ];

    let singletons: Vec<(&str, FixtureEntity)> = vec![
        ("statue_of_liberty", statue_of_liberty()?),
        ("high_line", high_line()?),
        ("burning_man_2023", burning_man_2023()?),
        ("hagia_sophia", hagia_sophia()?),
        ("pioneer_building", pioneer_building_seattle()?),
        ("kowloon_walled_city", kowloon_walled_city()?),
    ];

    bundles.extend(
        singletons
            .into_iter()
            .map(|(name, entity)| (name, TestBundle::single(name, entity))),
    );

    Ok(bundles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_bundles_pass_consistency_checks() -> Result<(), Box<dyn std::error::Error>> {
        let mut failures = Vec::new();

        for (bundle_name, bundle) in &all_bundles()? {
            for (entity_key, entity) in &bundle.entities {
                let warnings = entity.check_consistency();
                if !warnings.is_empty() {
                    failures.push(format!(
                        "Bundle '{bundle_name}', entity '{entity_key}': {warnings:?}"
                    ));
                }
            }

            if let Err(errors) = bundle.validate_references() {
                failures.push(format!(
                    "Bundle '{bundle_name}' reference errors: {errors:?}"
                ));
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("\n").into())
        }
    }
}
