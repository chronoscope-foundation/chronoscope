//! Pagination tests for research URL listings

use super::*;

#[tokio::test]
async fn test_pagination_empty_list() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let list = ctx.list_following(&token, "").await?;
    assert!(list.items.is_empty());
    assert!(list.next_page.is_none());
    Ok(())
}

#[tokio::test]
async fn test_pagination_exactly_limit_items() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    for i in 0..5 {
        ctx.add_research(&token, &format!("https://example.com/page{i}"))
            .await?;
    }

    // First page returns all 5 items with a next_page token
    let page1 = ctx.list_following(&token, "limit=5").await?;
    assert_eq!(page1.items.len(), 5);
    assert!(page1.next_page.is_some());

    // Following the token returns an empty page (Dropshot's pagination pattern)
    let page_token = page1.next_page.ok_or("expected next_page token")?;
    let page2 = ctx
        .list_following(&token, &format!("page_token={page_token}"))
        .await?;
    assert!(page2.items.is_empty());
    assert!(page2.next_page.is_none());
    Ok(())
}

#[tokio::test]
async fn test_pagination_more_than_limit() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    for i in 0..7 {
        ctx.add_research(&token, &format!("https://example.com/item{i}"))
            .await?;
    }

    // First page: 5 items, has next page
    let page1 = ctx.list_following(&token, "limit=5").await?;
    assert_eq!(page1.items.len(), 5);
    assert!(page1.next_page.is_some());

    // Second page: 2 remaining items
    let page_token = page1.next_page.ok_or("expected next_page token")?;
    let page2 = ctx
        .list_following(&token, &format!("page_token={page_token}"))
        .await?;
    assert_eq!(page2.items.len(), 2);
    assert!(page2.next_page.is_some());

    // Third page: empty (Dropshot's pagination pattern)
    let page_token = page2.next_page.ok_or("expected next_page token")?;
    let page3 = ctx
        .list_following(&token, &format!("page_token={page_token}"))
        .await?;
    assert!(page3.items.is_empty());
    assert!(page3.next_page.is_none());
    Ok(())
}

#[tokio::test]
async fn test_pagination_limit_capped_at_100() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    for i in 0..5 {
        ctx.add_research(&token, &format!("https://example.com/cap{i}"))
            .await?;
    }

    // Server caps at 100, should still work
    let list = ctx.list_following(&token, "limit=999").await?;
    assert_eq!(list.items.len(), 5);
    Ok(())
}

#[tokio::test]
async fn test_pagination_order_newest_first() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Insert URLs and set explicit timestamps to control ordering
    let base_time =
        chrono::NaiveDateTime::parse_from_str("2024-01-01 12:00:00", "%Y-%m-%d %H:%M:%S")?;
    let urls = [
        ("https://example.com/order0", base_time),
        (
            "https://example.com/order1",
            base_time + chrono::Duration::seconds(1),
        ),
        (
            "https://example.com/order2",
            base_time + chrono::Duration::seconds(2),
        ),
    ];

    for (url, timestamp) in &urls {
        let id = ctx.add_research(&token, url).await?;
        ctx.set_research_timestamp(&id, *timestamp).await?;
    }

    let list = ctx.list_following(&token, "").await?;
    // Newest first means order2 should be first
    assert!(list.items[0].research_url.url.contains("order2"));
    assert!(list.items[1].research_url.url.contains("order1"));
    assert!(list.items[2].research_url.url.contains("order0"));
    Ok(())
}

#[tokio::test]
async fn test_pagination_malformed_page_token() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Add some research so pagination would normally work
    ctx.add_research(&token, "https://example.com/item1")
        .await?;

    // Try with a malformed page_token
    let resp = ctx
        .get_auth("/users/me/following?page_token=invalid_token_data", &token)
        .await?;
    // Dropshot should return 400 for invalid page tokens
    assert_eq!(resp.status(), 400);
    Ok(())
}

// ==================== Pagination Edge Cases ====================

#[tokio::test]
async fn test_pagination_limit_one() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Add 3 items
    for i in 0..3 {
        ctx.add_research(&token, &format!("https://example.com/single{i}"))
            .await?;
    }

    // Request with limit=1
    let page1 = ctx.list_following(&token, "limit=1").await?;
    assert_eq!(page1.items.len(), 1, "Should return exactly 1 item");
    assert!(page1.next_page.is_some(), "Should have next page");

    // Follow pagination - limit must be specified on each request
    let page_token = page1.next_page.ok_or("expected next_page")?;
    let page2 = ctx
        .list_following(&token, &format!("limit=1&page_token={page_token}"))
        .await?;
    assert_eq!(page2.items.len(), 1, "Second page should have 1 item");
    assert!(page2.next_page.is_some());

    let page_token = page2.next_page.ok_or("expected next_page")?;
    let page3 = ctx
        .list_following(&token, &format!("limit=1&page_token={page_token}"))
        .await?;
    assert_eq!(page3.items.len(), 1, "Third page should have 1 item");
    assert!(page3.next_page.is_some());

    // Fourth page should be empty (Dropshot pattern)
    let page_token = page3.next_page.ok_or("expected next_page")?;
    let page4 = ctx
        .list_following(&token, &format!("limit=1&page_token={page_token}"))
        .await?;
    assert!(page4.items.is_empty(), "Fourth page should be empty");
    assert!(page4.next_page.is_none());

    Ok(())
}

#[tokio::test]
async fn test_pagination_limit_zero_rejected() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    ctx.add_research(&token, "https://example.com/item1")
        .await?;

    // limit=0 should be rejected by Dropshot's validation
    let resp = ctx
        .get_auth("/users/me/following?limit=0", &token)
        .await?;
    assert_eq!(
        resp.status(),
        400,
        "limit=0 should be rejected as invalid"
    );
    Ok(())
}

#[tokio::test]
async fn test_pagination_negative_limit_rejected() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    ctx.add_research(&token, "https://example.com/item1")
        .await?;

    // Negative limit should be rejected
    let resp = ctx
        .get_auth("/users/me/following?limit=-1", &token)
        .await?;
    assert_eq!(
        resp.status(),
        400,
        "Negative limit should be rejected as invalid"
    );
    Ok(())
}
