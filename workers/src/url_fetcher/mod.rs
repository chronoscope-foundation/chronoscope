//! URL fetcher worker.
//!
//! This module provides the `UrlFetcherWorker` which implements the `Worker` trait
//! to fetch URLs, extract content, and discover embedded media.
//!
//! # Architecture
//!
//! URL fetching is split between two layers:
//!
//! - **Integrations** (`chronoscope-integrations`): Domain-specific knowledge for platforms
//!   like Reddit and Instagram. Returns `FetchedContent` - pure data with no database access.
//!
//! - **Workers** (this crate): Generic fetching, persistence, and coordination. Calls
//!   integrations for specialized URLs, handles database operations for all URLs.

mod content;
mod fetcher;
mod generic;
#[cfg(test)]
pub(crate) mod test_harness;

use std::sync::Arc;

use chronoscope_db::media_store::MediaStore;
use chronoscope_db::{Database, MediaSlot, PageData, ResearchUrl};
use chronoscope_integrations::{
    BatchFetcher, FetchedContent, HttpClient, Integration, IntegrationName, IntegrationRegistry,
    SingleFetcher,
};
use tracing::{Instrument, error, info_span};
use url::Url;

use crate::worker::{ItemResult, Worker};

pub use content::{ContentType, ImageFormat, VideoFormat, content_hash, storage_key};
pub use fetcher::{FetchContext, FetchError, FetchOutcome, FetchResult, FetcherConfig};
pub use generic::GenericFetcher;

/// Worker that fetches URLs and extracts content.
///
/// Uses an `IntegrationRegistry` to route URLs to domain-specific integrations
/// (Reddit, Instagram, etc.) or falls back to a generic fetcher for other URLs.
pub struct UrlFetcherWorker {
    /// Registry of domain-specific integrations.
    integration_registry: IntegrationRegistry,
    /// Generic fetcher for URLs not handled by integrations.
    generic_fetcher: GenericFetcher,
    /// Shared context (database, HTTP client, media store).
    ctx: Arc<FetchContext>,
}

impl UrlFetcherWorker {
    /// Create a new URL fetcher worker.
    ///
    /// Uses the provided integration registry to route URLs to appropriate integrations.
    /// The context provides database, HTTP client, and media store access.
    #[must_use]
    pub fn new(integration_registry: IntegrationRegistry, ctx: Arc<FetchContext>) -> Self {
        Self {
            integration_registry,
            generic_fetcher: GenericFetcher::new(),
            ctx,
        }
    }

    /// Create a new URL fetcher worker with default integrations.
    ///
    /// Sets up the generic fetcher for all URLs plus integrations for Reddit, etc.
    /// Use `new()` for custom configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if integration registration fails (due to domain conflicts).
    /// This should never happen with the default integrations, but callers must handle it.
    pub fn with_defaults(
        db: Arc<Database>,
        http: Arc<dyn HttpClient>,
        media_store: Arc<dyn MediaStore>,
    ) -> Result<Self, chronoscope_integrations::RegistrationError> {
        let mut integration_registry = IntegrationRegistry::new();

        // Register domain-specific integrations
        integration_registry.register(Integration::Single(Arc::new(
            chronoscope_integrations::RedditIntegration::new(),
        )))?;

        let ctx = Arc::new(FetchContext {
            db,
            http,
            media_store,
            config: FetcherConfig::default(),
        });

        Ok(Self::new(integration_registry, ctx))
    }

    /// Process a URL using the appropriate integration or generic fetcher.
    async fn process_url(
        &self,
        research_url: &ResearchUrl,
        url: &Url,
    ) -> Result<FetchResult, FetchError> {
        // Check if we have a specialized integration for this URL
        if let Some(integration) = self.integration_registry.get(url) {
            self.process_with_integration(research_url, url, integration)
                .await
        } else {
            // Use generic fetcher
            self.generic_fetcher.process(&self.ctx, research_url).await
        }
    }

    /// Process a single URL using a specialized integration.
    ///
    /// For batch integrations, prefer using `process_batch_integration` which is more efficient.
    async fn process_with_integration(
        &self,
        research_url: &ResearchUrl,
        url: &Url,
        integration: &Integration,
    ) -> Result<FetchResult, FetchError> {
        match integration {
            Integration::Single(fetcher) => {
                self.process_single_integration(research_url, url, fetcher.as_ref())
                    .await
            }
            Integration::Batch(fetcher) => {
                // Process as a single-item batch
                self.process_batch_integration(
                    &[(research_url.clone(), url.clone())],
                    fetcher.as_ref(),
                )
                .await
                .pop()
                .map(|(_, result)| result)
                .unwrap_or_else(|| {
                    Err(FetchError::ContentProcessing(
                        "batch returned no results".into(),
                    ))
                })
            }
        }
    }

    /// Process multiple URLs using a batch integration (e.g., Instagram via Apify).
    async fn process_batch_integration(
        &self,
        items: &[(ResearchUrl, Url)],
        fetcher: &dyn BatchFetcher,
    ) -> Vec<(ResearchUrl, Result<FetchResult, FetchError>)> {
        if items.is_empty() {
            return Vec::new();
        }

        // Extract just the URLs for the batch fetcher
        let urls: Vec<Url> = items.iter().map(|(_, url)| url.clone()).collect();

        // Call the batch fetcher
        let fetch_results = fetcher.fetch_batch(self.ctx.http.as_ref(), &urls).await;

        // Build a map from URL to (research_url, result) for matching
        let research_url_map: std::collections::HashMap<String, &ResearchUrl> = items
            .iter()
            .map(|(ru, url)| (url.to_string(), ru))
            .collect();

        // Match results back to research URLs and persist
        let mut results = Vec::with_capacity(items.len());

        for (url, fetch_result) in fetch_results {
            let url_str = url.to_string();
            let Some(research_url) = research_url_map.get(&url_str) else {
                // This shouldn't happen, but log and skip if it does
                error!(url = %url_str, "batch result URL not found in input");
                continue;
            };
            let research_url = (*research_url).clone();

            let result = match fetch_result {
                Ok(content) => {
                    self.persist_fetched_content(&research_url, content, Some(fetcher.name()))
                        .await
                }
                Err(e) => Err(FetchError::Integration(e)),
            };

            results.push((research_url, result));
        }

        // Check for any items that didn't get results (e.g., batch fetcher missed them)
        for (research_url, _url) in items {
            if !results.iter().any(|(ru, _)| ru.id == research_url.id) {
                results.push((
                    research_url.clone(),
                    Err(FetchError::Integration(
                        chronoscope_integrations::FetchError::NotFound,
                    )),
                ));
            }
        }

        results
    }

    /// Process a URL using a single-URL integration (e.g., Reddit).
    async fn process_single_integration(
        &self,
        research_url: &ResearchUrl,
        url: &Url,
        fetcher: &dyn SingleFetcher,
    ) -> Result<FetchResult, FetchError> {
        // Fetch content using the integration
        let fetched = fetcher.fetch(self.ctx.http.as_ref(), url).await?;

        // Persist with the integration's source type
        self.persist_fetched_content(research_url, fetched, Some(fetcher.name()))
            .await
    }

    /// Persist fetched content to the database.
    ///
    /// The `integration_name` indicates which integration processed this content.
    /// `None` means the generic fetcher was used.
    async fn persist_fetched_content(
        &self,
        research_url: &ResearchUrl,
        content: FetchedContent,
        integration_name: Option<IntegrationName>,
    ) -> Result<FetchResult, FetchError> {
        let now = chrono::Utc::now().naive_utc();

        // Create media slots from discovered media URLs
        let media_slots: Vec<MediaSlot> = content
            .media
            .iter()
            .map(|url| MediaSlot::pending(url.to_string()))
            .collect();

        // Build PageData
        let page_data = PageData {
            source_type: integration_name.unwrap_or(IntegrationName::Generic),
            title: content.title,
            author: content.author,
            published: content
                .published_at
                .and_then(|dt| chronoscope_core::UncertainDate::exact(dt).ok()),
            content: content.content,
            fetched_at: now,
            media: media_slots,
        };

        // Persist the page
        let page_id = self.ctx.db.create_page(&page_data).await?;

        // Mark the URL as resolved
        self.ctx
            .db
            .mark_url_resolved_to_page(&research_url.id, &page_id)
            .await?;

        // Combine media URLs and discovered URLs for the result
        let mut discovered_urls = content.media;
        discovered_urls.extend(content.discovered_urls);

        Ok(FetchResult {
            outcome: FetchOutcome::Page { page_id },
            discovered_urls,
        })
    }
}

#[async_trait::async_trait]
impl Worker for UrlFetcherWorker {
    type Item = ResearchUrl;
    type Discovered = Url;
    type Error = FetchError;

    async fn process_batch(
        &self,
        items: Vec<Self::Item>,
    ) -> Vec<(Self::Item, ItemResult<Self::Discovered, Self::Error>)> {
        let mut results = Vec::with_capacity(items.len());

        for item in items {
            let url = match Url::parse(&item.url) {
                Ok(u) => u,
                Err(e) => {
                    error!(url = %item.url, error = %e, "invalid URL in queue");
                    results.push((
                        item,
                        ItemResult::PermanentFailure {
                            error: FetchError::ContentProcessing(format!("invalid URL: {e}")),
                        },
                    ));
                    continue;
                }
            };

            // Process the URL within a span
            let span = info_span!("fetch_url", url = %url);
            let result = async {
                match self.process_url(&item, &url).await {
                    Ok(fetch_result) => ItemResult::Success {
                        discovered: fetch_result.discovered_urls,
                    },
                    Err(e) if e.is_retriable() => ItemResult::RetriableFailure { error: e },
                    Err(e) => ItemResult::PermanentFailure { error: e },
                }
            }
            .instrument(span)
            .await;

            results.push((item, result));
        }

        results
    }
}
