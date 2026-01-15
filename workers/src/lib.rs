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

#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]
#![deny(unsafe_code)]

pub mod config;
pub mod http;
pub mod runner;
pub mod url_fetcher;
pub mod worker;

pub use config::WorkerConfig;
pub use runner::{
    Enqueuer, NoOpEnqueuer, RetryConfig, RunnerError, UrlEnqueuer, UrlQueue, WorkQueue, run,
};
pub use worker::{ItemResult, Worker};
