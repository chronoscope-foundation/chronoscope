//! Tests for the readiness probe.
//!
//! The point of the endpoint is that it can say no, so the closed-pool cases
//! carry the weight: each kills one of the two stores and expects the probe to
//! stop passing. The saturated case pins the other half of that judgment, that
//! a full pool is a different answer from a dead one.

use super::*;

#[tokio::test]
async fn health_answers_unauthenticated_when_both_stores_answer() -> TestResult {
    let ctx = TestContext::new().await?;

    let resp = ctx.get("/health").await?;
    assert_eq!(resp.status(), 204);
    assert!(resp.bytes().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn health_reports_unavailable_when_the_app_pool_is_gone() -> TestResult {
    let ctx = TestContext::new().await?;
    ctx.app_state.db.close().await;

    let resp = ctx.get("/health").await?;
    assert_eq!(resp.status(), 503);
    Ok(())
}

#[tokio::test]
async fn health_reports_unavailable_when_the_fact_store_is_gone() -> TestResult {
    let ctx = TestContext::new().await?;
    ctx.app_state.facts.close().await;

    let resp = ctx.get("/health").await?;
    assert_eq!(resp.status(), 503);
    Ok(())
}

/// The instance under the heaviest load is the one whose pool runs dry, so a
/// probe that reads that as failure recycles exactly the wrong instance.
#[tokio::test]
async fn health_stays_healthy_when_the_app_pool_has_nothing_free() -> TestResult {
    let ctx = TestContext::new().await?;

    // Held across the request below: the probe has to find nothing free.
    let pool = ctx.app_state.db.pool_ref();
    let mut held = Vec::new();
    for _ in 0..pool.options().get_max_connections() {
        held.push(pool.acquire().await?);
    }

    let resp = ctx.get("/health").await?;
    assert_eq!(resp.status(), 204);
    assert!(resp.bytes().await?.is_empty());
    Ok(())
}
