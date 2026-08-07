//! A credential this process depends on, and where its loss gets reported.
//!
//! Some pools authenticate with something that expires and has to be renewed;
//! Cloud SQL IAM database authentication, where a short-lived token rides in the
//! password field, is the one this crate implements. A renewal that cannot
//! succeed is fatal on a delay: the pool keeps serving from what it has open and
//! stops being able to open more, so the process that owns it should finish the
//! work in flight and exit while it still can.
//!
//! Producing that report is a backend's business. Acting on one is the
//! process's, and nothing about it is Postgres-shaped, so the signal lives above
//! the backends that create it and every build has it. A pool with nothing to
//! lose hands out [`CredentialWatch::never`].

use std::time::Duration;

use tokio::sync::watch;

/// The report that a credential this process authenticates with is gone: the one
/// in hand ran out with every attempt at a replacement failing.
///
/// Which credential is the context of whoever handed the watch over; what this
/// carries is why renewing it stopped working.
#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "the database credential expired with no replacement: {attempts} attempts over \
     {retried_for:?} all failed, the last with: {last_error}"
)]
pub struct CredentialsLost {
    /// Fetches attempted between the refresh falling due and the token expiring.
    pub attempts: u32,
    /// How long those attempts spanned, which is the margin the schedule left.
    pub retried_for: Duration,
    /// The last failure, rendered: a source's error type is its own, and this
    /// report is cloned to every watcher.
    pub last_error: String,
}

/// Where a pool's credential loss is reported. Cloning is cheap and every clone
/// sees the same one report.
#[derive(Debug, Clone)]
pub struct CredentialWatch(watch::Receiver<Option<CredentialsLost>>);

impl CredentialWatch {
    /// A fresh watch and the end whoever keeps the credential fresh reports on.
    ///
    /// Dropping that end without a report says the keeper stopped for its own
    /// reasons, which leaves the watch pending forever.
    pub(crate) fn reporting() -> (watch::Sender<Option<CredentialsLost>>, Self) {
        let (report, watch) = watch::channel(None);
        (report, Self(watch))
    }

    /// A watch nothing reports on, its reporting end dropped where it was made.
    /// What a connection string or a file path authenticates with lasts as long
    /// as the configuration does, so there is no credential to lose.
    pub fn never() -> Self {
        Self::reporting().1
    }

    /// A watch already reporting `loss`, with no keeper left to change its mind.
    /// The settled twin of [`never`](Self::never).
    pub fn already_lost(loss: CredentialsLost) -> Self {
        Self(watch::channel(Some(loss)).1)
    }

    /// The report, if the credential is already gone. Synchronous, for a health
    /// probe that has to answer now.
    pub fn reported(&self) -> Option<CredentialsLost> {
        self.0.borrow().clone()
    }

    /// Resolve once the credential is gone.
    ///
    /// A keeper that ends without a report ended on its pool's close, and
    /// whoever closed the pool is already shutting down, so this stays pending
    /// there and leaves the shutdown to them.
    pub async fn lost(&self) -> CredentialsLost {
        let mut watch = self.0.clone();
        loop {
            let seen = watch.borrow_and_update().clone();
            if let Some(loss) = seen {
                return loss;
            }
            if watch.changed().await.is_err() {
                return std::future::pending().await;
            }
        }
    }

    /// Whether anything still holds the reporting end.
    #[cfg(test)]
    pub(crate) fn reporter_alive(&self) -> bool {
        self.0.has_changed().is_ok()
    }

    /// Resolve once nothing holds the reporting end.
    #[cfg(test)]
    pub(crate) async fn reporter_ended(mut self) {
        while self.0.changed().await.is_ok() {}
    }
}

#[cfg(test)]
mod tests {
    use super::{CredentialWatch, CredentialsLost};
    use std::time::Duration;

    /// How much virtual time a case lets pass before calling a watch silent. The
    /// clock is paused, so this costs nothing and reads as "never".
    const LONG_ENOUGH: Duration = Duration::from_secs(86_400);

    fn a_loss() -> CredentialsLost {
        CredentialsLost {
            attempts: 3,
            retried_for: Duration::from_secs(90),
            last_error: "the issuer refused".to_owned(),
        }
    }

    /// A keeper ends without a report when whatever it was keeping fresh was
    /// closed under it, which means the process is already on its way down.
    /// Resolving here would start a second shutdown on every clean close.
    #[tokio::test(start_paused = true)]
    async fn a_keeper_that_ends_without_reporting_leaves_the_watch_pending() {
        let (report, watch) = CredentialWatch::reporting();
        assert!(watch.reporter_alive());

        drop(report);
        watch.clone().reporter_ended().await;

        assert!(watch.reported().is_none());
        assert!(
            tokio::time::timeout(LONG_ENOUGH, watch.lost())
                .await
                .is_err(),
            "a watch nothing reported on must stay pending"
        );
    }

    /// The probe reads the report synchronously and the shutdown path awaits it,
    /// so a watch handed over already settled has to answer both.
    #[tokio::test(start_paused = true)]
    async fn a_watch_built_already_lost_answers_both_readers() {
        let watch = CredentialWatch::already_lost(a_loss());

        assert_eq!(
            watch.reported().map(|loss| loss.last_error),
            Some(a_loss().last_error)
        );
        let awaited = tokio::time::timeout(LONG_ENOUGH, watch.lost()).await;
        assert_eq!(
            awaited.map(|loss| loss.last_error).ok(),
            Some(a_loss().last_error)
        );
    }
}
