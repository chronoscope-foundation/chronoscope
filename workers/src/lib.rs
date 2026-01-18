//! Chronoscope background workers.
//!
//! This crate provides background worker implementations for processing
//! tasks from database-backed queues.
//!
//! # Architecture
//!
//! Workers run in a loop, claiming items from a queue, processing them,
//! and reporting results. The worker infrastructure handles:
//!
//! - Batch claiming with optimistic locking (stale claims are reclaimed)
//! - Status updates (success/retry/permanent failure)
//! - Graceful idle backoff when the queue is empty
//!
//! # Available Workers
//!
//! - **URL Fetcher**: Fetches URLs, extracts content (HTML/images/video), discovers embedded media

pub mod config;
pub mod runner;
pub mod url_fetcher;
pub mod worker;

// Re-export types from integrations for convenience
pub use chronoscope_integrations::{
    ApifyConfig, CacheMode, CachingClient, HttpClient, HttpError, HttpRequest, HttpResponse,
    InstagramIntegration, Integration, IntegrationName, IntegrationRegistry, RedditIntegration,
    ReqwestClient, ReqwestConfig, create_registry,
};

pub use config::WorkerConfig;
pub use runner::{
    Enqueuer, NoOpEnqueuer, RetryConfig, RunnerError, UrlEnqueuer, UrlQueue, WorkQueue, run,
};
pub use worker::{ItemResult, Worker};
