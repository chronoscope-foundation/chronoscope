//! Ingestion library for transforming external data sources into Chronoscope format.
//!
//! This library provides modules for ingesting data from various knowledge bases
//! and transforming them into Chronoscope's unified schema.

pub mod check;
#[cfg(test)]
pub mod fixtures;
pub mod ids;
pub mod wikidata;

pub use ids::{EntityIdx, IngestionOutput, IngestionRelation, LinkIdx, SourceIdx};
