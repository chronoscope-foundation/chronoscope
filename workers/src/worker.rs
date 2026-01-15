//! Worker trait and result types.
//!
//! This module defines the core abstraction for background workers.
//! Workers process items from a queue and return results indicating
//! success or failure, with the runner handling DB status updates.

use std::fmt::Display;

/// Result of processing a single work item.
///
/// The error type `E` defaults to `String` for simple cases, but workers
/// can use a richer error type (e.g., `FetchError`) to preserve error details.
#[derive(Debug, Clone)]
pub enum ItemResult<D, E = String> {
    /// Successfully processed. Worker has already marked the item as resolved.
    /// May include discovered items to enqueue.
    Success {
        /// New items discovered during processing (e.g., URLs found in a page)
        discovered: Vec<D>,
    },

    /// Temporary failure - should retry later.
    /// Runner will mark as failed with `retry_after` based on attempt count.
    RetriableFailure {
        /// Error describing what went wrong
        error: E,
    },

    /// Permanent failure - don't retry.
    /// Runner will mark as failed without `retry_after`.
    PermanentFailure {
        /// Error describing what went wrong
        error: E,
    },
}

impl<D, E: Display> ItemResult<D, E> {
    /// Get the error message as a string, if this is a failure.
    pub fn error_message(&self) -> Option<String> {
        match self {
            Self::Success { .. } => None,
            Self::RetriableFailure { error } | Self::PermanentFailure { error } => {
                Some(error.to_string())
            }
        }
    }
}

/// Trait for background workers that process items from a queue.
///
/// Workers receive a batch of items, process them, and return a result
/// for each item. The runner handles:
/// - Claiming items from the queue
/// - Calling `process_batch`
/// - Marking items as failed based on results
/// - Enqueueing discovered items
///
/// On success, the worker is responsible for marking the item as resolved
/// (e.g., calling `mark_url_resolved_to_page`). The runner only handles
/// failure cases and discovered item enqueueing.
#[async_trait::async_trait]
pub trait Worker: Send + Sync {
    /// The type of item this worker processes (e.g., `ResearchUrl`)
    type Item: Send;

    /// The type of items discovered during processing (e.g., `Url`)
    type Discovered: Send;

    /// The error type for failures (e.g., `FetchError`)
    type Error: Display + Send;

    /// Process a batch of items and return results.
    ///
    /// Returns a vec of (item, result) pairs. Every item in the input batch
    /// should have a corresponding result in the output.
    async fn process_batch(
        &self,
        items: Vec<Self::Item>,
    ) -> Vec<(Self::Item, ItemResult<Self::Discovered, Self::Error>)>;
}
