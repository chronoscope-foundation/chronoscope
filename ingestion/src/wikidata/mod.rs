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

use anyhow::Result;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{RetryTransientMiddleware, policies::ExponentialBackoff};
use std::time::Duration;

/// User-Agent string for all Wikidata/Commons API requests.
pub const USER_AGENT: &str = "ChronoscopeBot/1.0";

/// Build an HTTP client with retry middleware.
pub fn build_client(timeout: Duration) -> Result<ClientWithMiddleware> {
    let retry_policy = ExponentialBackoff::builder()
        .retry_bounds(Duration::from_secs(1), Duration::from_secs(60))
        .build_with_max_retries(3);

    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .build()?;

    Ok(ClientBuilder::new(client)
        .with(RetryTransientMiddleware::new_with_policy(retry_policy))
        .build())
}

pub mod commons;
pub mod filter;
pub mod handlers;
pub mod ingest;
pub mod lifecycle;
pub mod parsing;
pub mod stream;
pub mod types;
pub mod usage;
