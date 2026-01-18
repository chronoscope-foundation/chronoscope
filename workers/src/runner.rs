//! Generic worker runner.
//!
//! The runner handles the worker loop: claiming items, calling the worker,
//! and updating DB status based on results.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use chronoscope_db::Database;
use tokio::sync::watch;
use tracing::{debug, debug_span, error, info, instrument, warn};

use crate::config::WorkerConfig;
use crate::worker::{ItemResult, Worker};

/// Configuration for retry behavior.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retries before giving up (0 = no retries)
    pub max_retries: u32,
    /// Base delay for exponential backoff
    pub base_delay: Duration,
    /// Maximum delay cap
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_secs(60),
            max_delay: Duration::from_secs(3600),
        }
    }
}

impl RetryConfig {
    /// Compute the retry delay for a given attempt count.
    /// Returns None if max retries exceeded (permanent failure).
    #[must_use]
    pub fn compute_retry_delay(&self, attempt_count: i32) -> Option<Duration> {
        // Negative attempt counts are treated as zero (no retries yet)
        let attempt = u32::try_from(attempt_count).unwrap_or(0);

        if attempt >= self.max_retries {
            return None; // Permanent failure
        }

        // Exponential backoff: base_delay * 2^attempt
        let multiplier = 2_u32.saturating_pow(attempt);
        let delay_secs = self
            .base_delay
            .as_secs()
            .saturating_mul(u64::from(multiplier));
        Some(Duration::from_secs(
            delay_secs.min(self.max_delay.as_secs()),
        ))
    }
}

/// Error from the runner.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("database error: {0}")]
    Database(#[from] chronoscope_db::DbError),
}

/// Callback trait for enqueueing discovered items.
///
/// The runner calls this when a worker returns Success with discovered items.
/// This abstraction allows different workers to enqueue different types of items.
#[async_trait::async_trait]
pub trait Enqueuer<D>: Send + Sync {
    /// Enqueue discovered items.
    async fn enqueue(&self, items: Vec<D>) -> Result<(), RunnerError>;
}

/// URL enqueuer that submits discovered URLs to the research queue.
pub struct UrlEnqueuer {
    db: Arc<Database>,
    /// User ID to associate with discovered URLs (system user)
    system_user_id: chronoscope_db::UserId,
}

impl UrlEnqueuer {
    /// Create a new URL enqueuer.
    #[must_use]
    pub fn new(db: Arc<Database>, system_user_id: chronoscope_db::UserId) -> Self {
        Self { db, system_user_id }
    }
}

#[async_trait::async_trait]
impl Enqueuer<url::Url> for UrlEnqueuer {
    async fn enqueue(&self, items: Vec<url::Url>) -> Result<(), RunnerError> {
        for url in items {
            match self.db.submit_url(&self.system_user_id, url.as_str()).await {
                Ok((id, created)) => {
                    if created {
                        debug!(url_id = %id, url = %url, "enqueued discovered URL");
                    } else {
                        debug!(url_id = %id, url = %url, "discovered URL already exists");
                    }
                }
                Err(e) => {
                    warn!(url = %url, error = %e, "failed to enqueue discovered URL");
                    // Continue with other URLs rather than failing the whole batch
                }
            }
        }
        Ok(())
    }
}

/// No-op enqueuer for workers that don't produce discovered items.
///
/// Only implements `Enqueuer<()>`, requiring workers that don't discover
/// anything to explicitly declare `type Discovered = ()`. This prevents
/// accidentally silencing discovered items.
pub struct NoOpEnqueuer;

#[async_trait::async_trait]
impl Enqueuer<()> for NoOpEnqueuer {
    async fn enqueue(&self, _items: Vec<()>) -> Result<(), RunnerError> {
        Ok(())
    }
}

// ==================== Work Queue Abstraction ====================

/// A work queue that provides items for processing.
///
/// This trait abstracts queue operations (claim, `mark_failed`) from the
/// runner, allowing different queue backends (DB tables, SQS, etc.)
/// and different item types (URLs, media items, etc.).
///
/// # Design Note: Success Handling
///
/// TODO: Currently there's an asymmetry in who handles success vs failure:
/// - **Failure**: Runner calls `queue.mark_failed()` (queue manages retry state)
/// - **Success**: Worker writes to DB directly (creates page/media, marks URL resolved)
///
/// This split is defensible: the `research_urls` table serves dual purposes:
/// - Queue mechanics (status, `claimed_at`, `retry_after`) -> managed by runner
/// - Domain data (`page_id`, `media_id`) -> managed by worker
///
/// Workers must write to the DB anyway to store content (pages/media), so having
/// them also mark resolution (`page_id`/`media_id`) isn't additional coupling.
///
/// Revisit this when we have more worker implementations to see if a cleaner
/// pattern emerges (e.g., `WorkQueue::mark_success()` that takes a "resolved to"
/// payload from the worker).
#[async_trait::async_trait]
pub trait WorkQueue: Send + Sync {
    /// The type of items in this queue.
    type Item: Send;

    /// The type of item identifiers (for logging and status updates).
    type ItemId: std::fmt::Display + Send + Sync + Clone;

    /// Extract the identifier from an item.
    #[must_use]
    fn item_id(item: &Self::Item) -> Self::ItemId;

    /// Get the attempt count for retry logic.
    #[must_use]
    fn attempt_count(item: &Self::Item) -> i32;

    /// Claim a batch of items for processing.
    async fn claim(
        &self,
        worker_id: &str,
        batch_size: u32,
        stale_cutoff: chrono::NaiveDateTime,
    ) -> Result<Vec<Self::Item>, RunnerError>;

    /// Mark an item as failed with optional retry.
    async fn mark_failed(
        &self,
        item_id: &Self::ItemId,
        error: &str,
        retry_after: Option<chrono::NaiveDateTime>,
    ) -> Result<(), RunnerError>;
}

/// Work queue for research URLs.
///
/// Claims URLs based on worker affinity:
/// - `affinity = None`: claims generic URLs only (`worker_affinity` IS NULL)
/// - `affinity = Some(name)`: claims URLs with matching `worker_affinity`
pub struct UrlQueue {
    db: Arc<Database>,
    affinity: Option<chronoscope_integrations::IntegrationName>,
}

impl UrlQueue {
    /// Create a queue with optional integration affinity.
    ///
    /// - `affinity = None`: claims generic URLs only (`worker_affinity` IS NULL)
    /// - `affinity = Some(name)`: claims URLs with matching `worker_affinity`
    #[must_use]
    pub fn new(
        db: Arc<Database>,
        affinity: Option<chronoscope_integrations::IntegrationName>,
    ) -> Self {
        Self { db, affinity }
    }
}

#[async_trait::async_trait]
impl WorkQueue for UrlQueue {
    type Item = chronoscope_db::ResearchUrl;
    type ItemId = chronoscope_db::ResearchUrlId;

    fn item_id(item: &Self::Item) -> Self::ItemId {
        item.id.clone()
    }

    fn attempt_count(item: &Self::Item) -> i32 {
        item.attempt_count
    }

    async fn claim(
        &self,
        worker_id: &str,
        batch_size: u32,
        stale_cutoff: chrono::NaiveDateTime,
    ) -> Result<Vec<Self::Item>, RunnerError> {
        let urls = match self.affinity {
            None => {
                self.db
                    .claim_urls(worker_id, batch_size, stale_cutoff)
                    .await?
            }
            Some(affinity) => {
                self.db
                    .claim_urls_with_affinity(worker_id, batch_size, stale_cutoff, affinity)
                    .await?
            }
        };
        Ok(urls)
    }

    async fn mark_failed(
        &self,
        item_id: &Self::ItemId,
        error: &str,
        retry_after: Option<chrono::NaiveDateTime>,
    ) -> Result<(), RunnerError> {
        Ok(self.db.mark_url_failed(item_id, error, retry_after).await?)
    }
}

/// Run the worker loop.
///
/// Claims items from the queue, processes them with the worker, and handles
/// status updates based on results. Runs until the shutdown signal is received.
///
/// # Errors
///
/// Returns `RunnerError::Database` if queue operations fail.
// Uses tokio::time::sleep for idle backoff when queue is empty. The sleep is
// interruptible via the shutdown channel, so workers can shut down promptly.
#[allow(clippy::disallowed_methods)]
#[instrument(skip_all, fields(worker_id = %config.worker_id))]
pub async fn run<Q, W, E>(
    queue: Q,
    worker: W,
    enqueuer: E,
    config: WorkerConfig,
    retry_config: RetryConfig,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), RunnerError>
where
    Q: WorkQueue,
    W: Worker<Item = Q::Item>,
    E: Enqueuer<W::Discovered>,
{
    info!(batch_size = config.batch_size, "worker starting");

    loop {
        // Check for shutdown
        if *shutdown.borrow() {
            info!("shutdown signal received, stopping");
            break;
        }

        // Claim a batch
        let stale_cutoff = config.stale_cutoff();
        let items = queue
            .claim(&config.worker_id, config.batch_size, stale_cutoff)
            .await?;

        if items.is_empty() {
            debug!(
                backoff_secs = config.idle_backoff.as_secs(),
                "no work available, backing off"
            );

            tokio::select! {
                () = tokio::time::sleep(config.idle_backoff) => {}
                () = async { let _ = shutdown.changed().await; } => {
                    if *shutdown.borrow() {
                        info!("shutdown signal received during backoff");
                        break;
                    }
                }
            }
            continue;
        }

        info!(count = items.len(), "claimed items");

        // Process the batch
        let results = worker.process_batch(items).await;

        // Handle results
        for (item, result) in results {
            let attempt_count = Q::attempt_count(&item);
            let item_id = Q::item_id(&item);
            let item_span = debug_span!("item", item_id = %item_id);

            match result {
                ItemResult::Success { discovered } => {
                    item_span.in_scope(|| {
                        debug!(discovered_count = discovered.len(), "item succeeded");
                    });

                    if !discovered.is_empty()
                        && let Err(e) = enqueuer.enqueue(discovered).await
                    {
                        error!(item_id = %item_id, error = %e, "failed to enqueue discovered items");
                    }
                }

                ItemResult::RetriableFailure { error } => {
                    let retry_after = retry_config
                        .compute_retry_delay(attempt_count)
                        .map(|d| Utc::now().naive_utc() + d);

                    let error_msg = error.to_string();
                    item_span.in_scope(|| {
                        if retry_after.is_some() {
                            debug!(attempt = attempt_count + 1, error = %error_msg, "item failed (will retry)");
                        } else {
                            warn!(attempts = attempt_count + 1, error = %error_msg, "item failed (max retries exceeded)");
                        }
                    });

                    if let Err(e) = queue.mark_failed(&item_id, &error_msg, retry_after).await {
                        error!(item_id = %item_id, error = %e, "failed to mark item as failed");
                    }
                }

                ItemResult::PermanentFailure { error } => {
                    let error_msg = error.to_string();
                    item_span.in_scope(|| {
                        warn!(error = %error_msg, "item permanently failed");
                    });

                    if let Err(e) = queue.mark_failed(&item_id, &error_msg, None).await {
                        error!(item_id = %item_id, error = %e, "failed to mark item as failed");
                    }
                }
            }
        }

        info!("completed processing batch");
    }

    info!("worker stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::Worker;
    use chronoscope_db::{Database, Email, ResearchUrl, UserId};
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;
    use tokio::sync::mpsc;
    use url::Url;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    // ==================== RetryConfig Unit Tests ====================

    #[test]
    fn test_retry_config_default() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.base_delay, Duration::from_secs(60));
        assert_eq!(config.max_delay, Duration::from_secs(3600));
    }

    #[test]
    fn test_compute_retry_delay_exponential_backoff() {
        let config = RetryConfig {
            max_retries: 5,
            base_delay: Duration::from_secs(60),
            max_delay: Duration::from_secs(3600),
        };

        assert_eq!(config.compute_retry_delay(0), Some(Duration::from_secs(60)));
        assert_eq!(
            config.compute_retry_delay(1),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            config.compute_retry_delay(2),
            Some(Duration::from_secs(240))
        );
        assert_eq!(
            config.compute_retry_delay(3),
            Some(Duration::from_secs(480))
        );
        assert_eq!(
            config.compute_retry_delay(4),
            Some(Duration::from_secs(960))
        );
    }

    #[test]
    fn test_compute_retry_delay_max_delay_cap() {
        let config = RetryConfig {
            max_retries: 10,
            base_delay: Duration::from_secs(60),
            max_delay: Duration::from_secs(300),
        };

        assert_eq!(
            config.compute_retry_delay(5),
            Some(Duration::from_secs(300))
        );
        assert_eq!(
            config.compute_retry_delay(6),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn test_compute_retry_delay_max_retries_exceeded() {
        let config = RetryConfig {
            max_retries: 3,
            base_delay: Duration::from_secs(60),
            max_delay: Duration::from_secs(3600),
        };

        assert!(config.compute_retry_delay(3).is_none());
        assert!(config.compute_retry_delay(4).is_none());
    }

    #[test]
    fn test_compute_retry_delay_no_retries_allowed() {
        let config = RetryConfig {
            max_retries: 0,
            base_delay: Duration::from_secs(60),
            max_delay: Duration::from_secs(3600),
        };

        // With max_retries=0, even the first attempt returns None (permanent failure)
        assert!(config.compute_retry_delay(0).is_none());
        assert!(config.compute_retry_delay(1).is_none());
    }

    // ==================== Test Worker ====================

    /// A test worker that records calls and returns pre-configured responses.
    ///
    /// On Success, marks the URL as resolved (so it won't be reclaimed).
    /// On failure, lets the runner handle retry/permanent failure marking.
    struct TestWorker {
        db: Arc<Database>,
        /// Channel to notify test of each URL processed.
        call_tx: mpsc::UnboundedSender<String>,
        /// Pre-configured responses: each URL maps to a queue of results.
        /// The worker pops from the front on each call.
        responses: Mutex<HashMap<String, VecDeque<ItemResult<Url>>>>,
    }

    impl TestWorker {
        fn new(db: Arc<Database>, call_tx: mpsc::UnboundedSender<String>) -> Self {
            Self {
                db,
                call_tx,
                responses: Mutex::new(HashMap::new()),
            }
        }

        /// Configure a sequence of responses for a URL.
        /// First call returns first result, second call returns second, etc.
        fn on_url(self, url: &str, responses: Vec<ItemResult<Url>>) -> Self {
            // Mutex poisoning only occurs if another thread panicked while holding
            // the lock. In single-threaded test setup code, this cannot happen.
            if let Ok(mut guard) = self.responses.lock() {
                guard.insert(url.to_string(), VecDeque::from(responses));
            }
            self
        }
    }

    #[async_trait::async_trait]
    impl Worker for TestWorker {
        type Item = ResearchUrl;
        type Discovered = Url;
        type Error = String;

        async fn process_batch(
            &self,
            items: Vec<Self::Item>,
        ) -> Vec<(Self::Item, ItemResult<Self::Discovered, Self::Error>)> {
            let mut results = Vec::with_capacity(items.len());

            for item in items {
                // Notify test that we received this item
                let _ = self.call_tx.send(item.url.clone());

                // Get next configured response (or default to permanent failure)
                let result = self
                    .responses
                    .lock()
                    .ok()
                    .and_then(|mut guard| guard.get_mut(&item.url).and_then(|q| q.pop_front()))
                    .unwrap_or_else(|| ItemResult::PermanentFailure {
                        error: "no response configured".into(),
                    });

                // On success, mark the URL as resolved so it won't be reclaimed
                if matches!(result, ItemResult::Success { .. }) {
                    // Create a minimal page and mark resolved
                    let page_data = chronoscope_db::PageData {
                        source_type: chronoscope_db::SourceType::Generic,
                        title: Some(format!("Test: {}", item.url)),
                        author: None,
                        published_at: None,
                        content: None,
                        fetched_at: Utc::now().naive_utc(),
                        media: vec![],
                    };
                    if let Ok(page_id) = self.db.create_page(&page_data).await {
                        let _ = self.db.mark_url_resolved_to_page(&item.id, &page_id).await;
                    }
                }

                results.push((item, result));
            }

            results
        }
    }

    // ==================== Test Helpers ====================

    /// Enqueuer that records discovered URLs for assertion.
    struct RecordingEnqueuer {
        enqueued_tx: mpsc::UnboundedSender<Url>,
    }

    #[async_trait::async_trait]
    impl Enqueuer<Url> for RecordingEnqueuer {
        async fn enqueue(&self, items: Vec<Url>) -> Result<(), RunnerError> {
            for url in items {
                let _ = self.enqueued_tx.send(url);
            }
            Ok(())
        }
    }

    async fn setup_test_db() -> Result<Arc<Database>, chronoscope_db::DbError> {
        Ok(Arc::new(Database::new("sqlite::memory:").await?))
    }

    async fn create_test_url(db: &Database, url: &str) -> Result<(), chronoscope_db::DbError> {
        let user_id = UserId::generate();
        db.create_user(&user_id, "testuser", &Email::new("test@example.com"))
            .await?;
        db.submit_url(&user_id, url).await?;
        Ok(())
    }

    fn fast_retry_config() -> RetryConfig {
        RetryConfig {
            max_retries: 5,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        }
    }

    fn fast_worker_config() -> WorkerConfig {
        WorkerConfig {
            worker_id: "test-worker".into(),
            batch_size: 10,
            stale_after: Duration::from_millis(1), // Allow immediate re-claim for retries
            idle_backoff: Duration::from_millis(1),
        }
    }

    /// Error type for `collect_n` failures.
    #[derive(Debug)]
    struct CollectError(String);

    impl std::fmt::Display for CollectError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl std::error::Error for CollectError {}

    /// Collect N items from a channel, with a timeout.
    async fn collect_n<T>(
        rx: &mut mpsc::UnboundedReceiver<T>,
        n: usize,
    ) -> Result<Vec<T>, CollectError> {
        let mut results = Vec::with_capacity(n);
        for _ in 0..n {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Some(item)) => results.push(item),
                Ok(None) => {
                    return Err(CollectError("channel closed unexpectedly".to_string()));
                }
                Err(_) => {
                    return Err(CollectError(format!(
                        "timeout waiting for item {}/{}",
                        results.len() + 1,
                        n
                    )));
                }
            }
        }
        Ok(results)
    }

    // ==================== Runner Integration Tests ====================

    #[tokio::test]
    async fn test_runner_calls_worker_with_queued_item() -> TestResult {
        let db = setup_test_db().await?;
        create_test_url(&db, "https://example.com/test").await?;

        let (call_tx, mut call_rx) = mpsc::unbounded_channel();
        let (enqueue_tx, _enqueue_rx) = mpsc::unbounded_channel();

        let worker = TestWorker::new(db.clone(), call_tx).on_url(
            "https://example.com/test",
            vec![ItemResult::Success { discovered: vec![] }],
        );

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let queue = UrlQueue::new(db.clone(), None);

        tokio::spawn(async move {
            let _ = run(
                queue,
                worker,
                RecordingEnqueuer {
                    enqueued_tx: enqueue_tx,
                },
                fast_worker_config(),
                fast_retry_config(),
                shutdown_rx,
            )
            .await;
        });

        // Wait for worker to be called once
        let calls = collect_n(&mut call_rx, 1).await?;
        let _ = shutdown_tx.send(true);

        assert_eq!(calls, vec!["https://example.com/test"]);
        Ok(())
    }

    #[tokio::test]
    async fn test_runner_retries_on_retriable_error() -> TestResult {
        let db = setup_test_db().await?;
        create_test_url(&db, "https://example.com/retry").await?;

        let (call_tx, mut call_rx) = mpsc::unbounded_channel();
        let (enqueue_tx, _enqueue_rx) = mpsc::unbounded_channel();

        // Configure: fail twice, then succeed
        let worker = TestWorker::new(db.clone(), call_tx).on_url(
            "https://example.com/retry",
            vec![
                ItemResult::RetriableFailure {
                    error: "try 1".into(),
                },
                ItemResult::RetriableFailure {
                    error: "try 2".into(),
                },
                ItemResult::Success { discovered: vec![] },
            ],
        );

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let queue = UrlQueue::new(db.clone(), None);

        tokio::spawn(async move {
            let _ = run(
                queue,
                worker,
                RecordingEnqueuer {
                    enqueued_tx: enqueue_tx,
                },
                fast_worker_config(),
                fast_retry_config(),
                shutdown_rx,
            )
            .await;
        });

        // Should be called 3 times (2 retries + 1 success)
        let calls = collect_n(&mut call_rx, 3).await?;
        let _ = shutdown_tx.send(true);

        // All 3 calls should be for the same URL
        assert_eq!(
            calls,
            vec![
                "https://example.com/retry",
                "https://example.com/retry",
                "https://example.com/retry",
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_runner_does_not_retry_permanent_failure() -> TestResult {
        let db = setup_test_db().await?;
        create_test_url(&db, "https://example.com/permanent").await?;

        let (call_tx, mut call_rx) = mpsc::unbounded_channel();
        let (enqueue_tx, _enqueue_rx) = mpsc::unbounded_channel();

        // Configure permanent failure, then success (should never reach success)
        let worker = TestWorker::new(db.clone(), call_tx).on_url(
            "https://example.com/permanent",
            vec![
                ItemResult::PermanentFailure {
                    error: "gone".into(),
                },
                ItemResult::Success { discovered: vec![] }, // Should never be reached
            ],
        );

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let queue = UrlQueue::new(db.clone(), None);

        tokio::spawn(async move {
            let _ = run(
                queue,
                worker,
                RecordingEnqueuer {
                    enqueued_tx: enqueue_tx,
                },
                fast_worker_config(),
                fast_retry_config(),
                shutdown_rx,
            )
            .await;
        });

        // Should only be called once
        let calls = collect_n(&mut call_rx, 1).await?;

        // Give a brief moment to ensure no second call comes
        let extra = tokio::time::timeout(Duration::from_millis(50), call_rx.recv()).await;
        let _ = shutdown_tx.send(true);

        assert_eq!(calls, vec!["https://example.com/permanent"]);
        assert!(extra.is_err(), "should not have been called again");
        Ok(())
    }

    #[tokio::test]
    async fn test_runner_processes_discovered_urls() -> TestResult {
        let db = setup_test_db().await?;
        create_test_url(&db, "https://example.com/parent").await?;

        // Create system user for UrlEnqueuer
        let system_user = UserId::generate();
        db.create_user(&system_user, "system", &Email::new("system@test.io"))
            .await?;

        let (call_tx, mut call_rx) = mpsc::unbounded_channel();

        let discovered = vec![
            Url::parse("https://example.com/child1")?,
            Url::parse("https://example.com/child2")?,
        ];

        // Configure worker for parent (returns discovered) and both children (return success)
        let worker = TestWorker::new(db.clone(), call_tx)
            .on_url(
                "https://example.com/parent",
                vec![ItemResult::Success { discovered }],
            )
            .on_url(
                "https://example.com/child1",
                vec![ItemResult::Success { discovered: vec![] }],
            )
            .on_url(
                "https://example.com/child2",
                vec![ItemResult::Success { discovered: vec![] }],
            );

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let queue = UrlQueue::new(db.clone(), None);
        let enqueuer = UrlEnqueuer::new(db.clone(), system_user);

        tokio::spawn(async move {
            let _ = run(
                queue,
                worker,
                enqueuer,
                fast_worker_config(),
                fast_retry_config(),
                shutdown_rx,
            )
            .await;
        });

        // Wait for worker to be called 3 times (parent + 2 children)
        let calls = collect_n(&mut call_rx, 3).await?;
        let _ = shutdown_tx.send(true);

        // Verify all 3 URLs were processed
        assert!(
            calls.contains(&"https://example.com/parent".to_string()),
            "expected parent in calls: {calls:?}"
        );
        assert!(
            calls.contains(&"https://example.com/child1".to_string()),
            "expected child1 in calls: {calls:?}"
        );
        assert!(
            calls.contains(&"https://example.com/child2".to_string()),
            "expected child2 in calls: {calls:?}"
        );
        Ok(())
    }
}
