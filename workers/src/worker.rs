//! Worker trait and result types.
//!
//! This module defines the core abstraction for background workers.
//! Workers process items from a queue and return results indicating
//! success or failure, with the runner handling DB status updates.

/// Result of processing a single work item.
#[derive(Debug, Clone)]
pub enum ItemResult<D> {
    /// Successfully processed. Worker has already marked the item as resolved.
    /// May include discovered items to enqueue.
    Success {
        /// New items discovered during processing (e.g., URLs found in a page)
        discovered: Vec<D>,
    },

    /// Temporary failure - should retry later.
    /// Runner will mark as failed with `retry_after` based on attempt count.
    RetriableFailure {
        /// Error message describing what went wrong
        error: String,
    },

    /// Permanent failure - don't retry.
    /// Runner will mark as failed without `retry_after`.
    PermanentFailure {
        /// Error message describing what went wrong
        error: String,
    },
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

    /// The type of items discovered during processing (e.g., Url)
    type Discovered: Send;

    /// Process a batch of items and return results.
    ///
    /// Returns a vec of (item, result) pairs. Every item in the input batch
    /// should have a corresponding result in the output.
    async fn process_batch(
        &self,
        items: Vec<Self::Item>,
    ) -> Vec<(Self::Item, ItemResult<Self::Discovered>)>;
}
