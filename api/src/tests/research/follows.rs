//! Follow/unfollow tests and multi-user isolation

use super::*;

// ==================== Follow/Unfollow Tests ====================

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
    assert_eq!(
        alice_list.items[0].research_url.url,
        "https://example.com/shared"
    );
    assert_eq!(
        bob_list.items[0].research_url.url,
        "https://example.com/shared"
    );
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
        alice_following.items[0].research_url.url,
        "https://example.com/alice-only"
    );
    assert_eq!(
        bob_following.items[0].research_url.url,
        "https://example.com/bob-only"
    );

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
    assert_eq!(
        bob_list.items[0].research_url.url,
        "https://example.com/shared"
    );

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
