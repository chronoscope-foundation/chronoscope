//! Ingestion library for transforming external data sources into Chronoscope format.
//!
//! This library provides modules for ingesting data from various knowledge bases
//! and transforming them into Chronoscope's unified schema.

pub mod ids;
pub mod wikidata;

pub use ids::SourceIdx;
