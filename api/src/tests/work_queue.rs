//! Work queue tests: URL claiming, retry behavior, batch operations

use super::*;

#[tokio::test]
async fn test_claim_urls_marks_as_analyzing() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let url_id = ctx.add_research(&token, "https://example.com/test").await?;

    let stale = chrono::Utc::now().naive_utc() - chrono::Duration::hours(1);
    let claimed = ctx.app_state.db.claim_urls("worker-1", 1, stale).await?;

    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, url_id);
    assert_eq!(claimed[0].status, ResearchUrlStatus::Analyzing);

    Ok(())
}

#[tokio::test]
async fn test_claimed_url_not_reclaimable() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    ctx.add_research(&token, "https://example.com/test").await?;

    let stale = chrono::Utc::now().naive_utc() - chrono::Duration::hours(1);

    // First worker claims
    let claimed1 = ctx.app_state.db.claim_urls("worker-1", 1, stale).await?;
    assert_eq!(claimed1.len(), 1);

    // Second worker tries to claim - should get nothing
    let claimed2 = ctx.app_state.db.claim_urls("worker-2", 1, stale).await?;
    assert_eq!(claimed2.len(), 0);

    Ok(())
}

#[tokio::test]
async fn test_stale_claim_can_be_reclaimed() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let url_id = ctx.add_research(&token, "https://example.com/test").await?;

    // First worker claims
    let old_stale = chrono::Utc::now().naive_utc() - chrono::Duration::hours(1);
    let claimed1 = ctx
        .app_state
        .db
        .claim_urls("worker-1", 1, old_stale)
        .await?;
    assert_eq!(claimed1.len(), 1);

    // Simulate time passing - stale threshold is now AFTER the claim time
    let new_stale = chrono::Utc::now().naive_utc() + chrono::Duration::hours(1);

    // Second worker can now reclaim
    let claimed2 = ctx
        .app_state
        .db
        .claim_urls("worker-2", 1, new_stale)
        .await?;
    assert_eq!(claimed2.len(), 1);
    assert_eq!(claimed2[0].id, url_id);

    Ok(())
}

#[tokio::test]
async fn test_retry_after_in_future_not_claimable() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let url_id = ctx.add_research(&token, "https://example.com/test").await?;

    // Mark as failed with retry_after in the future
    let retry_after = chrono::Utc::now().naive_utc() + chrono::Duration::hours(1);
    ctx.app_state
        .db
        .mark_url_failed(&url_id, "temporary error", Some(retry_after))
        .await?;

    // Try to claim - should get nothing (retry_after not reached)
    let stale = chrono::Utc::now().naive_utc() - chrono::Duration::hours(1);
    let claimed = ctx.app_state.db.claim_urls("worker-1", 1, stale).await?;
    assert_eq!(claimed.len(), 0);

    Ok(())
}

#[tokio::test]
async fn test_retry_after_in_past_is_claimable() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let url_id = ctx.add_research(&token, "https://example.com/test").await?;

    // Mark as failed with retry_after in the past
    let retry_after = chrono::Utc::now().naive_utc() - chrono::Duration::hours(1);
    ctx.app_state
        .db
        .mark_url_failed(&url_id, "temporary error", Some(retry_after))
        .await?;

    // Try to claim - should succeed (retry_after has passed)
    let stale = chrono::Utc::now().naive_utc() - chrono::Duration::hours(2);
    let claimed = ctx.app_state.db.claim_urls("worker-1", 1, stale).await?;
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, url_id);

    Ok(())
}

#[tokio::test]
async fn test_failed_without_retry_after_not_claimable() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let url_id = ctx.add_research(&token, "https://example.com/test").await?;

    // Mark as failed with NO retry_after (permanent failure)
    ctx.app_state
        .db
        .mark_url_failed(&url_id, "permanent error", None)
        .await?;

    // Try to claim - should get nothing (no retry_after means don't retry)
    let stale = chrono::Utc::now().naive_utc() - chrono::Duration::hours(1);
    let claimed = ctx.app_state.db.claim_urls("worker-1", 1, stale).await?;
    assert_eq!(claimed.len(), 0);

    Ok(())
}

#[tokio::test]
async fn test_failed_url_increments_attempt_count() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let url_id = ctx.add_research(&token, "https://example.com/test").await?;

    // Fail multiple times
    ctx.app_state
        .db
        .mark_url_failed(&url_id, "error 1", None)
        .await?;
    ctx.app_state
        .db
        .mark_url_failed(&url_id, "error 2", None)
        .await?;
    ctx.app_state
        .db
        .mark_url_failed(&url_id, "error 3", None)
        .await?;

    // Check attempt count via raw query
    let row: (i32,) = sqlx::query_as("SELECT attempt_count FROM research_urls WHERE id = ?")
        .bind(&url_id)
        .fetch_one(ctx.app_state.db.pool_ref())
        .await?;

    assert_eq!(row.0, 3);

    Ok(())
}

#[tokio::test]
async fn test_claim_batch_respects_limit() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Create 5 URLs
    for i in 0..5 {
        ctx.add_research(&token, &format!("https://example.com/test{i}"))
            .await?;
    }

    let stale = chrono::Utc::now().naive_utc() - chrono::Duration::hours(1);

    // Claim batch of 3
    let claimed = ctx.app_state.db.claim_urls("worker-1", 3, stale).await?;
    assert_eq!(claimed.len(), 3);

    // Claim another batch - should get remaining 2
    let claimed2 = ctx.app_state.db.claim_urls("worker-2", 3, stale).await?;
    assert_eq!(claimed2.len(), 2);

    Ok(())
}

#[tokio::test]
async fn test_claim_prioritizes_retry_after_nulls_first() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Create URLs in specific order
    let url1 = ctx
        .add_research(&token, "https://example.com/first")
        .await?;
    let url2 = ctx
        .add_research(&token, "https://example.com/second")
        .await?;
    let url3 = ctx
        .add_research(&token, "https://example.com/third")
        .await?;

    // Set url2 with a retry_after in the past (so it should come last in priority)
    // First mark it failed, then we'll test that pending URLs come first
    let retry_after = chrono::Utc::now().naive_utc() - chrono::Duration::minutes(5);
    ctx.app_state
        .db
        .mark_url_failed(&url2, "temporary", Some(retry_after))
        .await?;

    let stale = chrono::Utc::now().naive_utc() - chrono::Duration::hours(1);

    // Claim one at a time to check order
    let claimed1 = ctx.app_state.db.claim_urls("worker-1", 1, stale).await?;
    let claimed2 = ctx.app_state.db.claim_urls("worker-1", 1, stale).await?;
    let claimed3 = ctx.app_state.db.claim_urls("worker-1", 1, stale).await?;

    // Pending URLs (url1, url3) should come before failed URL (url2)
    // because ORDER BY retry_after NULLS FIRST
    assert!(
        claimed1[0].id == url1 || claimed1[0].id == url3,
        "First claimed should be a pending URL"
    );
    assert!(
        claimed2[0].id == url1 || claimed2[0].id == url3,
        "Second claimed should be a pending URL"
    );
    assert_eq!(claimed3[0].id, url2, "Last claimed should be the retry URL");

    Ok(())
}
