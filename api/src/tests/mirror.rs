//! Tests for the mirror sweep trigger.

use super::*;
use crate::mirror::SWEEP_TOKEN_HEADER;

/// Without the token the endpoint refuses before anything else — the gate that
/// keeps an open endpoint from being swept by whoever finds it, giving up
/// nothing, not even whether a queue is configured.
#[tokio::test]
async fn sweep_without_the_token_is_forbidden() -> TestResult {
    let ctx = TestContext::new().await?;

    let resp = ctx.post("/mirror/sweep").await?;
    assert_eq!(resp.status(), 403);
    Ok(())
}

/// A present-but-wrong token is refused too: the header must match the server's
/// secret, not merely be there.
#[tokio::test]
async fn sweep_with_a_wrong_token_is_forbidden() -> TestResult {
    let ctx = TestContext::new().await?;

    let resp = ctx
        .client
        .reqwest_client()
        .post(ctx.url("/mirror/sweep"))
        .header(SWEEP_TOKEN_HEADER, "not-the-token")
        .send()
        .await?;
    assert_eq!(resp.status(), 403);
    Ok(())
}

/// With the right token but no configured queue it refuses with 503 — the auth
/// gate passed, the queue check did not, and it never touched the store.
#[tokio::test]
async fn sweep_with_the_token_refuses_when_unconfigured() -> TestResult {
    let ctx = TestContext::new().await?;

    let resp = ctx
        .client
        .reqwest_client()
        .post(ctx.url("/mirror/sweep"))
        .header(SWEEP_TOKEN_HEADER, TEST_SWEEP_TOKEN)
        .send()
        .await?;
    assert_eq!(resp.status(), 503);
    Ok(())
}
