//! URL fetcher worker.
//!
//! This module provides the `UrlFetcherWorker` which implements the `Worker` trait
//! to fetch URLs, extract content, and discover embedded media.

mod content;
mod fetcher;
mod generic;

use std::sync::Arc;

use chronoscope_db::media_store::MediaStore;
use chronoscope_db::{Database, ResearchUrl};
use tracing::{Instrument, error, info_span};
use url::Url;

use crate::http::HttpClient;
use crate::worker::{ItemResult, Worker};

pub use content::{ContentType, ImageFormat, VideoFormat, content_hash, storage_key};
pub use fetcher::{
    FetchContext, FetchError, FetchOutcome, FetchResult, Fetcher, FetcherConfig, FetcherRegistry,
};
pub use generic::GenericFetcher;

/// Worker that fetches URLs and extracts content.
///
/// Wraps a `FetcherRegistry` to route URLs to domain-specific or generic fetchers.
pub struct UrlFetcherWorker {
    registry: FetcherRegistry,
    ctx: Arc<FetchContext>,
}

impl UrlFetcherWorker {
    /// Create a new URL fetcher worker.
    ///
    /// Uses the provided registry to route URLs to appropriate fetchers.
    /// The context provides database, HTTP client, and media store access.
    #[must_use]
    pub fn new(registry: FetcherRegistry, ctx: Arc<FetchContext>) -> Self {
        Self { registry, ctx }
    }

    /// Create a new URL fetcher worker with default generic fetcher.
    ///
    /// Sets up a generic fetcher for all URLs. Use `new()` to provide
    /// domain-specific fetchers.
    #[must_use]
    pub fn with_defaults(
        db: Arc<Database>,
        http: Arc<dyn HttpClient>,
        media_store: Arc<dyn MediaStore>,
    ) -> Self {
        let generic: Arc<dyn Fetcher> = Arc::new(GenericFetcher::new());
        let registry = FetcherRegistry::new(generic);

        let ctx = Arc::new(FetchContext {
            db,
            http,
            media_store,
            config: FetcherConfig::default(),
        });

        Self::new(registry, ctx)
    }
}

#[async_trait::async_trait]
impl Worker for UrlFetcherWorker {
    type Item = ResearchUrl;
    type Discovered = Url;

    async fn process_batch(
        &self,
        items: Vec<Self::Item>,
    ) -> Vec<(Self::Item, ItemResult<Self::Discovered>)> {
        let mut results = Vec::with_capacity(items.len());

        for item in items {
            let url = match Url::parse(&item.url) {
                Ok(u) => u,
                Err(e) => {
                    error!(url = %item.url, error = %e, "invalid URL in queue");
                    results.push((
                        item,
                        ItemResult::PermanentFailure {
                            error: format!("invalid URL: {e}"),
                        },
                    ));
                    continue;
                }
            };

            // Get appropriate fetcher for this URL
            let fetcher = self.registry.get(&url);

            // Process the URL within a span
            let span = info_span!("fetch_url", url = %url);
            let result = async {
                match fetcher.process(&self.ctx, &item).await {
                    Ok(fetch_result) => ItemResult::Success {
                        discovered: fetch_result.discovered_urls,
                    },
                    Err(e) if e.is_retriable() => ItemResult::RetriableFailure {
                        error: e.to_string(),
                    },
                    Err(e) => ItemResult::PermanentFailure {
                        error: e.to_string(),
                    },
                }
            }
            .instrument(span)
            .await;

            results.push((item, result));
        }

        results
    }
}
