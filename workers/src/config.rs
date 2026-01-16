//! Worker configuration.
//!
//! This module provides configuration structures for workers.
//! Workers can be configured via environment variables (production)
//! or constructed directly (tests).

use std::time::Duration;

use chrono::{NaiveDateTime, Utc};

/// Generic configuration for all worker types.
///
/// This contains queue management settings that apply to any worker,
/// regardless of what type of work it performs.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Unique identifier for this worker instance.
    /// Used for claim ownership and logging.
    pub worker_id: String,

    /// Number of items to claim per batch.
    pub batch_size: u32,

    /// Duration after which a claim is considered stale/abandoned.
    /// Other workers can reclaim items with claims older than this.
    pub stale_after: Duration,

    /// How long to wait when the queue is empty before checking again.
    pub idle_backoff: Duration,
}

impl WorkerConfig {
    /// Compute the stale cutoff timestamp for the current moment.
    ///
    /// Items claimed before this timestamp are considered abandoned
    /// and can be reclaimed by other workers.
    #[must_use]
    pub fn stale_cutoff(&self) -> NaiveDateTime {
        Utc::now().naive_utc() - self.stale_after
    }
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            worker_id: format!("worker-{}", uuid::Uuid::new_v4()),
            batch_size: 10,
            stale_after: Duration::from_mins(5),
            idle_backoff: Duration::from_secs(5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = WorkerConfig::default();

        assert!(config.worker_id.starts_with("worker-"));
        assert_eq!(config.batch_size, 10);
        assert_eq!(config.stale_after, Duration::from_secs(300));
        assert_eq!(config.idle_backoff, Duration::from_secs(5));
    }

    #[test]
    fn test_stale_cutoff() {
        let config = WorkerConfig::default();

        let cutoff = config.stale_cutoff();
        let now = Utc::now().naive_utc();

        // Cutoff should be approximately 5 minutes ago (within a second tolerance)
        let diff = now - cutoff;
        assert!(diff.num_seconds() >= 299 && diff.num_seconds() <= 301);
    }
}
