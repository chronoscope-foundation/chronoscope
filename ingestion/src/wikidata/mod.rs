//! Wikidata ingestion module.
//!
//! Transforms filtered Wikidata architectural entities into Chronoscope schema.
//!
//! # Usage
//!
//! ```ignore
//! use chronoscope_ingestion::wikidata::{filter, ingest};
//!
//! // Filter a dump
//! filter::filter_dump(&input_path, &output_path, None, verbose).await?;
//!
//! // Or ingest pre-filtered data
//! let config = ingest::Config { ... };
//! ingest::run(&config).await?;
//! ```

pub mod filter;
pub mod handlers;
pub mod ingest;
pub mod lifecycle;
pub mod parsing;
pub mod stream;
pub mod usage;
