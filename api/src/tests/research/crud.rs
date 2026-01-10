//! Basic research URL CRUD operations

use super::*;

#[tokio::test]
async fn test_research_add_and_list() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let _id = ctx
        .add_research(&token, "https://example.com/article")
        .await?;

    // Should appear in user's following list
    let following = ctx.list_following(&token, "").await?;
    assert_eq!(following.items.len(), 1);
    assert_eq!(
        following.items[0].research_url.url,
        "https://example.com/article"
    );

    // Should also appear in global research list
    let research = ctx.list_research("").await?;
    assert_eq!(research.items.len(), 1);
    assert_eq!(research.items[0].url, "https://example.com/article");
    Ok(())
}

#[tokio::test]
async fn test_research_duplicate_url_idempotent() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let id1 = ctx
        .add_research(&token, "https://example.com/duplicate")
        .await?;
    let id2 = ctx
        .add_research(&token, "https://example.com/duplicate")
        .await?;

    // Should return the same ID (idempotent)
    assert_eq!(id1, id2);
    assert_eq!(ctx.list_following(&token, "").await?.items.len(), 1);
    Ok(())
}

#[tokio::test]
async fn test_research_get_single() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let id = ctx
        .add_research(&token, "https://example.com/single")
        .await?;

    let resp = ctx.get(&format!("/research/{id}")).await?;
    assert_eq!(resp.status(), 200);

    let item: ResearchUrlDossier = resp.json().await?;
    assert_eq!(item.url, "https://example.com/single");
    Ok(())
}

#[tokio::test]
async fn test_research_get_not_found() -> TestResult {
    let ctx = TestContext::new().await?;

    // Try to get a non-existent research URL
    let fake_id = ResearchUrlId::generate();
    let resp = ctx.get(&format!("/research/{fake_id}")).await?;
    assert_eq!(resp.status(), 404);
    Ok(())
}

#[tokio::test]
async fn test_research_list_is_public() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    ctx.add_research(&token, "https://example.com/public-item")
        .await?;

    // GET /research should work without auth
    let list = ctx.list_research("").await?;
    assert_eq!(list.items.len(), 1);
    assert_eq!(list.items[0].url, "https://example.com/public-item");
    Ok(())
}
