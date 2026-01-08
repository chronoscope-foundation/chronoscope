//! Research URL tests: CRUD operations, validation, pagination, multi-user scenarios

use super::*;

// ==================== Research CRUD Tests ====================

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
    assert_eq!(following.items[0].url, "https://example.com/article");

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

    // GET /research/{id} is now public
    let resp = ctx.get(&format!("/research/{id}")).await?;
    assert_eq!(resp.status(), 200);

    let item: crate::research::ResearchUrlResponse = resp.json().await?;
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
async fn test_follow_nonexistent_url() -> TestResult {
    let ctx = TestContext::new().await?;
    let user = ctx.new_user().await?;

    // Try to follow a non-existent URL
    let fake_id = ResearchUrlId::generate();
    let resp = user.follow(&fake_id).await?;
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

// ==================== Follow/Unfollow Tests ====================

#[tokio::test]
async fn test_unfollow_url() -> TestResult {
    let ctx = TestContext::new().await?;
    let user = ctx.new_user().await?;

    let id = user.add_research("https://example.com/to-unfollow").await?;

    // Unfollow the URL
    assert_eq!(user.unfollow(&id).await?.status(), 204);

    // Should no longer be in following list
    assert_eq!(user.list_following("").await?.items.len(), 0);

    // But should still exist in global research
    let research = ctx.list_research("").await?;
    assert_eq!(research.items.len(), 1);
    Ok(())
}

#[tokio::test]
async fn test_unfollow_nonexistent() -> TestResult {
    let ctx = TestContext::new().await?;
    let user = ctx.new_user().await?;

    // Try to unfollow something we never followed
    let fake_id = ResearchUrlId::generate();
    assert_eq!(user.unfollow(&fake_id).await?.status(), 404);
    Ok(())
}

// ==================== URL Validation Tests ====================

#[tokio::test]
async fn test_validation_empty_url() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let req = SubmitResearchRequest {
        url: "".to_string(),
    };
    assert_eq!(
        ctx.post_auth("/research", &token, &req).await?.status(),
        400
    );
    Ok(())
}

#[tokio::test]
async fn test_validation_invalid_url_format() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let req = SubmitResearchRequest {
        url: "not a valid url".to_string(),
    };
    assert_eq!(
        ctx.post_auth("/research", &token, &req).await?.status(),
        400
    );
    Ok(())
}

#[tokio::test]
async fn test_validation_non_http_schemes_rejected() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Test various non-HTTP schemes that should be rejected
    for invalid_url in &[
        "ftp://example.com/file",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "mailto:test@example.com",
    ] {
        let req = SubmitResearchRequest {
            url: invalid_url.to_string(),
        };
        let resp = ctx.post_auth("/research", &token, &req).await?;
        assert_eq!(resp.status(), 400, "Expected 400 for URL: {invalid_url}");
    }
    Ok(())
}

#[tokio::test]
async fn test_validation_http_schemes_accepted() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Both http and https should work
    let req = SubmitResearchRequest {
        url: "http://example.com/article".to_string(),
    };
    assert_eq!(
        ctx.post_auth("/research", &token, &req).await?.status(),
        201
    );

    let req = SubmitResearchRequest {
        url: "https://example.com/secure".to_string(),
    };
    assert_eq!(
        ctx.post_auth("/research", &token, &req).await?.status(),
        201
    );
    Ok(())
}

// ==================== SSRF Protection Tests ====================

#[tokio::test]
async fn test_ssrf_private_ip_rejected() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // URLs with private/internal IPs should be rejected to prevent SSRF attacks
    let blocked_urls = [
        ("http://127.0.0.1/admin", "loopback"),
        ("http://169.254.169.254/latest/meta-data/", "AWS IMDS"),
        ("http://10.0.0.1/internal", "private 10.x"),
        ("http://192.168.1.1/router", "private 192.168.x"),
        ("http://172.16.0.1/internal", "private 172.16.x"),
    ];

    for (url, desc) in blocked_urls {
        let req = SubmitResearchRequest {
            url: url.to_string(),
        };
        let resp = ctx.post_auth("/research", &token, &req).await?;
        assert_eq!(resp.status(), 400, "Expected {desc} to be blocked: {url}");

        let body: serde_json::Value = resp.json().await?;
        assert_eq!(
            body["message"].as_str(),
            Some("URL not allowed"),
            "Error message for {desc}"
        );
    }
    Ok(())
}

// ==================== Pagination Tests ====================

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

    let list = ctx.list_following(&token, "limit=5").await?;
    assert_eq!(list.items.len(), 5);
    assert!(list.next_page.is_none());
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

    // Second page: use page_token from first page
    let page_token = page1.next_page.ok_or("expected next_page token")?;
    let page2 = ctx
        .list_following(&token, &format!("page_token={page_token}"))
        .await?;
    assert_eq!(page2.items.len(), 2);
    assert!(page2.next_page.is_none());
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
    assert!(list.items[0].url.contains("order2"));
    assert!(list.items[1].url.contains("order1"));
    assert!(list.items[2].url.contains("order0"));
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

// ==================== Multi-User Tests ====================

#[tokio::test]
async fn test_multiuser_both_can_follow_same_url() -> TestResult {
    let ctx = TestContext::new().await?;

    let alice = ctx.new_user().await?;
    let bob = ctx.new_user().await?;

    // Alice submits a URL (creates and follows)
    let url_id = alice.add_research("https://example.com/shared").await?;

    // Bob follows the same URL
    let resp = bob.follow(&url_id).await?;
    assert_eq!(resp.status(), 204);

    // Both should see the URL in their following list
    let alice_list = alice.list_following("").await?;
    let bob_list = bob.list_following("").await?;

    assert_eq!(alice_list.items.len(), 1);
    assert_eq!(bob_list.items.len(), 1);
    assert_eq!(alice_list.items[0].url, "https://example.com/shared");
    assert_eq!(bob_list.items[0].url, "https://example.com/shared");
    Ok(())
}

#[tokio::test]
async fn test_multiuser_each_sees_only_own_follows() -> TestResult {
    let ctx = TestContext::new().await?;

    let alice = ctx.new_user().await?;
    let bob = ctx.new_user().await?;

    // Alice submits her URL
    alice.add_research("https://example.com/alice-only").await?;

    // Bob submits his URL
    bob.add_research("https://example.com/bob-only").await?;

    // Each should only see their own in following
    let alice_following = alice.list_following("").await?;
    let bob_following = bob.list_following("").await?;

    assert_eq!(alice_following.items.len(), 1);
    assert_eq!(bob_following.items.len(), 1);
    assert_eq!(
        alice_following.items[0].url,
        "https://example.com/alice-only"
    );
    assert_eq!(bob_following.items[0].url, "https://example.com/bob-only");

    // But global research list should show both
    let all_research = ctx.list_research("").await?;
    assert_eq!(all_research.items.len(), 2);
    Ok(())
}

#[tokio::test]
async fn test_multiuser_unfollow_doesnt_affect_others() -> TestResult {
    let ctx = TestContext::new().await?;

    let alice = ctx.new_user().await?;
    let bob = ctx.new_user().await?;

    // Alice submits a URL
    let url_id = alice.add_research("https://example.com/shared").await?;

    // Bob also follows it
    bob.follow(&url_id).await?;

    // Alice unfollows
    let resp = alice.unfollow(&url_id).await?;
    assert_eq!(resp.status(), 204);

    // Alice should see nothing in following, Bob should still see the URL
    let alice_list = alice.list_following("").await?;
    let bob_list = bob.list_following("").await?;

    assert_eq!(alice_list.items.len(), 0);
    assert_eq!(bob_list.items.len(), 1);
    assert_eq!(bob_list.items[0].url, "https://example.com/shared");

    // Research should still exist globally
    let research = ctx.list_research("").await?;
    assert_eq!(research.items.len(), 1);
    Ok(())
}

#[tokio::test]
async fn test_multiuser_duplicate_submit_returns_same_url() -> TestResult {
    let ctx = TestContext::new().await?;

    let alice = ctx.new_user().await?;
    let bob = ctx.new_user().await?;

    // Both submit the same URL
    let alice_url_id = alice.add_research("https://example.com/popular").await?;
    let bob_url_id = bob.add_research("https://example.com/popular").await?;

    // Should be the same URL ID (deduplicated)
    assert_eq!(alice_url_id, bob_url_id);

    // Both should see it in their following lists
    let alice_list = alice.list_following("").await?;
    let bob_list = bob.list_following("").await?;

    assert_eq!(alice_list.items.len(), 1);
    assert_eq!(bob_list.items.len(), 1);

    // But only one in global research
    let research = ctx.list_research("").await?;
    assert_eq!(research.items.len(), 1);
    Ok(())
}
