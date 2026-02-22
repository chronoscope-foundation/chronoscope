use super::*;
use crate::error::DbError;
use crate::models::MediaSlot;
use crate::queue::{Queue, url_queue_config};
use crate::types::{Email, MediaAnalysisState, MediaType, ResearchUrlStatus, SourceType, UserId};
use chrono::{Duration, Utc};
use chronoscope_core::UncertainLocation;

macro_rules! assert_approx_eq {
    ($left:expr, $right:expr, $epsilon:expr) => {
        let (left, right) = ($left, $right);
        assert!(
            (left - right).abs() < $epsilon,
            "assertion failed: |{left} - {right}| = {} >= {epsilon}",
            (left - right).abs(),
            epsilon = $epsilon,
        );
    };
}

/// Helper to create a database and test user.
async fn setup() -> DbResult<(Database, UserId)> {
    let db = Database::new("sqlite::memory:").await?;
    let user_id = UserId::generate();
    let email = Email::new("test@example.com");
    db.create_user(&user_id, "testuser", &email).await?;
    Ok((db, user_id))
}

#[tokio::test]
async fn test_claim_urls_marks_as_processing() -> DbResult<()> {
    let (db, user_id) = setup().await?;
    let (url_id, _) = db.submit_url(&user_id, "https://example.com/test").await?;

    let stale = Utc::now().naive_utc() - Duration::hours(1);
    let claimed = db.url_queue_generic.claim("worker-1", 1, stale).await?;

    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, url_id);
    assert_eq!(claimed[0].status, ResearchUrlStatus::Processing);

    Ok(())
}

#[tokio::test]
async fn test_claimed_url_not_reclaimable() -> DbResult<()> {
    let (db, user_id) = setup().await?;
    db.submit_url(&user_id, "https://example.com/test").await?;

    let stale = Utc::now().naive_utc() - Duration::hours(1);

    // First worker claims
    let claimed1 = db.url_queue_generic.claim("worker-1", 1, stale).await?;
    assert_eq!(claimed1.len(), 1);

    // Second worker tries to claim - should get nothing
    let claimed2 = db.url_queue_generic.claim("worker-2", 1, stale).await?;
    assert_eq!(claimed2.len(), 0);

    Ok(())
}

#[tokio::test]
async fn test_stale_claim_can_be_reclaimed() -> DbResult<()> {
    let (db, user_id) = setup().await?;
    let (url_id, _) = db.submit_url(&user_id, "https://example.com/test").await?;

    // First worker claims
    let old_stale = Utc::now().naive_utc() - Duration::hours(1);
    let claimed1 = db.url_queue_generic.claim("worker-1", 1, old_stale).await?;
    assert_eq!(claimed1.len(), 1);

    // Simulate time passing - stale cutoff is now AFTER the claim time
    let new_stale = Utc::now().naive_utc() + Duration::hours(1);

    // Second worker can now reclaim
    let claimed2 = db.url_queue_generic.claim("worker-2", 1, new_stale).await?;
    assert_eq!(claimed2.len(), 1);
    assert_eq!(claimed2[0].id, url_id);

    Ok(())
}

#[tokio::test]
async fn test_retry_after_in_future_not_claimable() -> DbResult<()> {
    let (db, user_id) = setup().await?;
    let (url_id, _) = db.submit_url(&user_id, "https://example.com/test").await?;

    // Mark as failed with retry_after in the future
    let retry_after = Utc::now().naive_utc() + Duration::hours(1);
    db.url_queue_generic
        .mark_failed(&url_id, "temporary error", Some(retry_after))
        .await?;

    // Try to claim - should get nothing (retry_after not reached)
    let stale = Utc::now().naive_utc() - Duration::hours(1);
    let claimed = db.url_queue_generic.claim("worker-1", 1, stale).await?;
    assert_eq!(claimed.len(), 0);

    Ok(())
}

#[tokio::test]
async fn test_retry_after_in_past_is_claimable() -> DbResult<()> {
    let (db, user_id) = setup().await?;
    let (url_id, _) = db.submit_url(&user_id, "https://example.com/test").await?;

    // Mark as failed with retry_after in the past
    let retry_after = Utc::now().naive_utc() - Duration::hours(1);
    db.url_queue_generic
        .mark_failed(&url_id, "temporary error", Some(retry_after))
        .await?;

    // Try to claim - should succeed (retry_after has passed)
    let stale = Utc::now().naive_utc() - Duration::hours(2);
    let claimed = db.url_queue_generic.claim("worker-1", 1, stale).await?;
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, url_id);

    Ok(())
}

#[tokio::test]
async fn test_failed_without_retry_after_not_claimable() -> DbResult<()> {
    let (db, user_id) = setup().await?;
    let (url_id, _) = db.submit_url(&user_id, "https://example.com/test").await?;

    // Mark as failed with NO retry_after (permanent failure)
    db.url_queue_generic
        .mark_failed(&url_id, "permanent error", None)
        .await?;

    // Try to claim - should get nothing (no retry_after means don't retry)
    let stale = Utc::now().naive_utc() - Duration::hours(1);
    let claimed = db.url_queue_generic.claim("worker-1", 1, stale).await?;
    assert_eq!(claimed.len(), 0);

    Ok(())
}

#[tokio::test]
async fn test_failed_url_increments_attempt_count() -> DbResult<()> {
    let (db, user_id) = setup().await?;
    let (url_id, _) = db.submit_url(&user_id, "https://example.com/test").await?;

    // Fail multiple times
    db.url_queue_generic
        .mark_failed(&url_id, "error 1", None)
        .await?;
    db.url_queue_generic
        .mark_failed(&url_id, "error 2", None)
        .await?;
    db.url_queue_generic
        .mark_failed(&url_id, "error 3", None)
        .await?;

    // Check attempt count via raw query
    let row: (i32,) = sqlx::query_as("SELECT attempt_count FROM research_urls WHERE id = ?")
        .bind(&url_id)
        .fetch_one(&db.pool)
        .await?;

    assert_eq!(row.0, 3);

    Ok(())
}

#[tokio::test]
async fn test_claim_batch_respects_limit() -> DbResult<()> {
    let (db, user_id) = setup().await?;

    // Create 5 URLs
    for i in 0..5 {
        db.submit_url(&user_id, &format!("https://example.com/test{i}"))
            .await?;
    }

    let stale = Utc::now().naive_utc() - Duration::hours(1);

    // Claim batch of 3
    let claimed = db.url_queue_generic.claim("worker-1", 3, stale).await?;
    assert_eq!(claimed.len(), 3);

    // Claim another batch - should get remaining 2
    let claimed2 = db.url_queue_generic.claim("worker-2", 3, stale).await?;
    assert_eq!(claimed2.len(), 2);

    Ok(())
}

#[tokio::test]
async fn test_claim_prioritizes_retry_after_nulls_first() -> DbResult<()> {
    let (db, user_id) = setup().await?;

    // Create URLs in specific order
    let (url1, _) = db.submit_url(&user_id, "https://example.com/first").await?;
    let (url2, _) = db
        .submit_url(&user_id, "https://example.com/second")
        .await?;
    let (url3, _) = db.submit_url(&user_id, "https://example.com/third").await?;

    // Set url2 with a retry_after in the past (so it should come last in priority)
    let retry_after = Utc::now().naive_utc() - Duration::minutes(5);
    db.url_queue_generic
        .mark_failed(&url2, "temporary", Some(retry_after))
        .await?;

    let stale = Utc::now().naive_utc() - Duration::hours(1);

    // Claim one at a time to check order
    let claimed1 = db.url_queue_generic.claim("worker-1", 1, stale).await?;
    let claimed2 = db.url_queue_generic.claim("worker-1", 1, stale).await?;
    let claimed3 = db.url_queue_generic.claim("worker-1", 1, stale).await?;

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

// ==================== create_page tests ====================

#[tokio::test]
async fn test_create_page_without_media() -> DbResult<()> {
    let (db, _) = setup().await?;

    let page_data = PageData {
        source_type: SourceType::Generic,
        title: Some("Test Article".to_string()),
        author: Some("Test Author".to_string()),
        published: None,
        content: Some("This is test content.".to_string()),
        fetched_at: Utc::now().naive_utc(),
        media: vec![],
    };

    let page_id = db.create_page(&page_data).await?;

    // Verify page was created
    let row: Option<(String,)> = sqlx::query_as("SELECT title FROM pages WHERE id = ?")
        .bind(&page_id)
        .fetch_optional(&db.pool)
        .await?;

    assert!(row.is_some());
    assert_eq!(row.as_ref().map(|(t,)| t.as_str()), Some("Test Article"));

    Ok(())
}

#[tokio::test]
async fn test_create_page_with_media_slots() -> DbResult<()> {
    let (db, _) = setup().await?;

    let media_urls = vec![
        "https://example.com/image1.jpg",
        "https://example.com/image2.jpg",
    ];

    let page_data = PageData {
        source_type: SourceType::Reddit,
        title: Some("Post with Images".to_string()),
        author: None,
        published: None,
        content: None,
        fetched_at: Utc::now().naive_utc(),
        media: media_urls.iter().map(|u| MediaSlot::pending(*u)).collect(),
    };

    let page_id = db.create_page(&page_data).await?;

    // Verify research_urls were created for media
    for url in &media_urls {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT status FROM research_urls WHERE url = ?")
                .bind(*url)
                .fetch_optional(&db.pool)
                .await?;
        assert!(row.is_some(), "URL {url} should exist in research_urls");
        assert_eq!(row.as_ref().map(|(s,)| s.as_str()), Some("pending"));
    }

    // Verify page_media links were created with correct order
    let links: Vec<(i32,)> = sqlx::query_as(
        "SELECT source_order FROM page_media WHERE page_id = ? ORDER BY source_order",
    )
    .bind(&page_id)
    .fetch_all(&db.pool)
    .await?;

    assert_eq!(links.len(), 2);
    assert_eq!(links[0].0, 0); // First image
    assert_eq!(links[1].0, 1); // Second image

    Ok(())
}

#[tokio::test]
async fn test_create_page_rejects_pre_resolved_media() -> DbResult<()> {
    let (db, _) = setup().await?;

    // Create a MediaSlot with resolved content (which is invalid for create_page)
    let mut slot = MediaSlot::pending("https://example.com/image.jpg");
    slot.resolved = Some(crate::models::Media {
        id: MediaId::generate(),
        data: MediaData {
            exact_hash: vec![1, 2, 3],
            perceptual_hash: None,
            storage_key: "test/key".to_string(),
            media_type: MediaType::Image,
            width: 100,
            height: 100,
            duration_seconds: None,
            captured: None,
            location: None,
            source_metadata: None,
            fetched_at: Utc::now().naive_utc(),
        },
        created_at: Utc::now().naive_utc(),
        analysis: MediaAnalysisState::Pending,
    });

    let page_data = PageData {
        source_type: SourceType::Generic,
        title: None,
        author: None,
        published: None,
        content: None,
        fetched_at: Utc::now().naive_utc(),
        media: vec![slot],
    };

    let result = db.create_page(&page_data).await;
    assert!(matches!(result, Err(DbError::InvalidArgument(_))));

    Ok(())
}

#[tokio::test]
async fn test_create_page_with_existing_media_url() -> DbResult<()> {
    let (db, user_id) = setup().await?;

    // Pre-create a URL via submit_url
    let existing_url = "https://example.com/existing.jpg";
    let (existing_id, _) = db.submit_url(&user_id, existing_url).await?;

    // Create page referencing the existing URL plus a new one
    let page_data = PageData {
        source_type: SourceType::Generic,
        title: None,
        author: None,
        published: None,
        content: None,
        fetched_at: Utc::now().naive_utc(),
        media: vec![
            MediaSlot::pending(existing_url),
            MediaSlot::pending("https://example.com/new.jpg"),
        ],
    };

    let page_id = db.create_page(&page_data).await?;

    // Verify the existing URL wasn't duplicated
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM research_urls WHERE url = ?")
        .bind(existing_url)
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(count.0, 1, "Should not duplicate existing URL");

    // Verify page_media links to the existing URL
    let link: Option<(ResearchUrlId,)> =
        sqlx::query_as("SELECT url_id FROM page_media WHERE page_id = ? AND source_order = 0")
            .bind(&page_id)
            .fetch_optional(&db.pool)
            .await?;

    assert_eq!(link.map(|(id,)| id), Some(existing_id));

    Ok(())
}

// ==================== get_or_create_media tests ====================

#[tokio::test]
async fn test_get_or_create_media_creates_new() -> DbResult<()> {
    let (db, _) = setup().await?;

    let media_data = MediaData {
        exact_hash: vec![1, 2, 3, 4, 5],
        perceptual_hash: Some(vec![10, 20, 30]),
        storage_key: "test/hash12345.jpg".to_string(),
        media_type: MediaType::Image,
        width: 1920,
        height: 1080,
        duration_seconds: None,
        captured: None,
        location: None,
        source_metadata: Some(r#"{"camera": "test"}"#.to_string()),
        fetched_at: Utc::now().naive_utc(),
    };

    let media_id = db.get_or_create_media(&media_data).await?;

    // Verify media was created with correct data
    let row: Option<(i32, i32, String)> =
        sqlx::query_as("SELECT width, height, storage_key FROM media WHERE id = ?")
            .bind(&media_id)
            .fetch_optional(&db.pool)
            .await?;

    let (width, height, key) =
        row.ok_or_else(|| DbError::InvalidArgument("media should exist".to_string()))?;
    assert_eq!(width, 1920);
    assert_eq!(height, 1080);
    assert_eq!(key, "test/hash12345.jpg");

    Ok(())
}

#[tokio::test]
async fn test_get_or_create_media_deduplicates_by_hash() -> DbResult<()> {
    let (db, _) = setup().await?;

    let hash = vec![42, 43, 44, 45, 46];

    // Create first media
    let media1 = MediaData {
        exact_hash: hash.clone(),
        perceptual_hash: None,
        storage_key: "test/first.jpg".to_string(),
        media_type: MediaType::Image,
        width: 100,
        height: 100,
        duration_seconds: None,
        captured: None,
        location: None,
        source_metadata: None,
        fetched_at: Utc::now().naive_utc(),
    };
    let id1 = db.get_or_create_media(&media1).await?;

    // Create second media with same hash but different metadata
    let media2 = MediaData {
        exact_hash: hash.clone(),
        perceptual_hash: None,
        storage_key: "test/second.jpg".to_string(), // Different key
        media_type: MediaType::Image,
        width: 200, // Different dimensions
        height: 200,
        duration_seconds: None,
        captured: None,
        location: None,
        source_metadata: None,
        fetched_at: Utc::now().naive_utc(),
    };
    let id2 = db.get_or_create_media(&media2).await?;

    // Should return the same ID (deduplication)
    assert_eq!(id1, id2, "Same hash should return same media ID");

    // Verify only one row exists
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM media WHERE exact_hash = ?")
        .bind(&hash)
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(count.0, 1, "Should not create duplicate media");

    Ok(())
}

#[tokio::test]
async fn test_get_or_create_media_different_hash_creates_new() -> DbResult<()> {
    let (db, _) = setup().await?;

    let media1 = MediaData {
        exact_hash: vec![1, 1, 1],
        perceptual_hash: None,
        storage_key: "test/a.jpg".to_string(),
        media_type: MediaType::Image,
        width: 100,
        height: 100,
        duration_seconds: None,
        captured: None,
        location: None,
        source_metadata: None,
        fetched_at: Utc::now().naive_utc(),
    };

    let media2 = MediaData {
        exact_hash: vec![2, 2, 2],
        perceptual_hash: None,
        storage_key: "test/b.jpg".to_string(),
        media_type: MediaType::Image,
        width: 100,
        height: 100,
        duration_seconds: None,
        captured: None,
        location: None,
        source_metadata: None,
        fetched_at: Utc::now().naive_utc(),
    };

    let id1 = db.get_or_create_media(&media1).await?;
    let id2 = db.get_or_create_media(&media2).await?;

    assert_ne!(id1, id2, "Different hashes should create different media");

    Ok(())
}

#[tokio::test]
async fn test_get_or_create_media_with_video() -> DbResult<()> {
    let (db, _) = setup().await?;

    let media_data = MediaData {
        exact_hash: vec![99, 98, 97],
        perceptual_hash: None,
        storage_key: "test/video.mp4".to_string(),
        media_type: MediaType::Video,
        width: 1280,
        height: 720,
        duration_seconds: Some(120.5),
        captured: None,
        location: None,
        source_metadata: None,
        fetched_at: Utc::now().naive_utc(),
    };

    let media_id = db.get_or_create_media(&media_data).await?;

    let row: Option<(String, f64)> =
        sqlx::query_as("SELECT media_type, duration_seconds FROM media WHERE id = ?")
            .bind(&media_id)
            .fetch_optional(&db.pool)
            .await?;

    let (media_type, duration) =
        row.ok_or_else(|| DbError::InvalidArgument("media should exist".to_string()))?;
    assert_eq!(media_type, "video");
    assert_approx_eq!(duration, 120.5, 1e-4);

    Ok(())
}

#[tokio::test]
async fn test_get_or_create_media_with_location() -> DbResult<()> {
    let (db, _) = setup().await?;

    let media_data = MediaData {
        exact_hash: vec![77, 78, 79],
        perceptual_hash: None,
        storage_key: "test/geotagged.jpg".to_string(),
        media_type: MediaType::Image,
        width: 640,
        height: 480,
        duration_seconds: None,
        captured: None,
        location: Some(
            UncertainLocation::coordinates(37.7749, -122.4194, None, None)
                .expect("valid test coordinates"),
        ),
        source_metadata: None,
        fetched_at: Utc::now().naive_utc(),
    };

    let media_id = db.get_or_create_media(&media_data).await?;

    // Verify shadow columns
    let row: Option<(f64, f64)> =
        sqlx::query_as("SELECT latitude, longitude FROM media WHERE id = ?")
            .bind(&media_id)
            .fetch_optional(&db.pool)
            .await?;

    let (lat, lon) =
        row.ok_or_else(|| DbError::InvalidArgument("media should exist".to_string()))?;
    assert_approx_eq!(lat, 37.7749, 1e-4);
    assert_approx_eq!(lon, -122.4194, 1e-4);

    // Verify meta JSON roundtrip
    let meta_row: Option<(String,)> =
        sqlx::query_as("SELECT location_meta FROM media WHERE id = ?")
            .bind(&media_id)
            .fetch_optional(&db.pool)
            .await?;
    let (meta_json,) =
        meta_row.ok_or_else(|| DbError::InvalidArgument("media should exist".to_string()))?;
    let loc: UncertainLocation = serde_json::from_str(&meta_json)
        .map_err(|e| DbError::InvalidArgument(format!("bad json: {e}")))?;
    match loc {
        UncertainLocation::Coordinates { lat, lon, .. } => {
            assert_approx_eq!(lat, 37.7749, 1e-4);
            assert_approx_eq!(lon, -122.4194, 1e-4);
        }
        other => panic!("expected Coordinates, got {other:?}"),
    }

    Ok(())
}

// ==================== mark_url_resolved tests ====================

#[tokio::test]
async fn test_mark_url_resolved_to_page() -> DbResult<()> {
    let (db, user_id) = setup().await?;
    let (url_id, _) = db
        .submit_url(&user_id, "https://example.com/article")
        .await?;

    // Claim the URL first (simulating worker flow)
    let stale = Utc::now().naive_utc() - Duration::hours(1);
    let claimed = db.url_queue_generic.claim("worker-1", 1, stale).await?;
    assert_eq!(claimed.len(), 1);

    // Create a page and mark resolved
    let page_data = PageData {
        source_type: SourceType::Generic,
        title: Some("Test".to_string()),
        author: None,
        published: None,
        content: None,
        fetched_at: Utc::now().naive_utc(),
        media: vec![],
    };
    let page_id = db.create_page(&page_data).await?;
    db.mark_url_resolved_to_page(&url_id, &page_id).await?;

    // Verify: page_id set, status complete, claim cleared
    let row: (Option<PageId>, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT page_id, status, claimed_at, claimed_by FROM research_urls WHERE id = ?",
    )
    .bind(&url_id)
    .fetch_one(&db.pool)
    .await?;

    assert_eq!(row.0, Some(page_id));
    assert_eq!(row.1, "complete");
    assert!(row.2.is_none(), "claimed_at should be cleared");
    assert!(row.3.is_none(), "claimed_by should be cleared");

    Ok(())
}

#[tokio::test]
async fn test_mark_url_resolved_to_media() -> DbResult<()> {
    let (db, user_id) = setup().await?;
    let (url_id, _) = db
        .submit_url(&user_id, "https://example.com/photo.jpg")
        .await?;

    // Claim the URL first
    let stale = Utc::now().naive_utc() - Duration::hours(1);
    let claimed = db.url_queue_generic.claim("worker-1", 1, stale).await?;
    assert_eq!(claimed.len(), 1);

    // Create media and mark resolved
    let media_data = MediaData {
        exact_hash: vec![1, 2, 3],
        perceptual_hash: None,
        storage_key: "test/photo.jpg".to_string(),
        media_type: MediaType::Image,
        width: 100,
        height: 100,
        duration_seconds: None,
        captured: None,
        location: None,
        source_metadata: None,
        fetched_at: Utc::now().naive_utc(),
    };
    let media_id = db.get_or_create_media(&media_data).await?;
    db.mark_url_resolved_to_media(&url_id, &media_id).await?;

    // Verify: media_id set, status complete, claim cleared
    let row: (Option<MediaId>, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT media_id, status, claimed_at, claimed_by FROM research_urls WHERE id = ?",
    )
    .bind(&url_id)
    .fetch_one(&db.pool)
    .await?;

    assert_eq!(row.0, Some(media_id));
    assert_eq!(row.1, "complete");
    assert!(row.2.is_none(), "claimed_at should be cleared");
    assert!(row.3.is_none(), "claimed_by should be cleared");

    Ok(())
}

// ==================== Affinity-Based Claiming Tests ====================

use crate::models::ResearchUrl;
use chronoscope_integrations::IntegrationName;

#[tokio::test]
async fn test_claim_urls_with_affinity_only_claims_matching() -> DbResult<()> {
    let (db, user_id) = setup().await?;

    // Submit a Reddit URL (gets affinity "reddit" from built-in registry)
    let (reddit_id, _) = db
        .submit_url(&user_id, "https://reddit.com/r/rust/comments/abc123")
        .await?;

    let stale = Utc::now().naive_utc() - Duration::hours(1);

    // Create affinity-specific queues for testing
    let instagram_queue: Queue<ResearchUrl> = Queue::new(
        db.pool.clone(),
        url_queue_config(Some(IntegrationName::Instagram)),
    );
    let reddit_queue: Queue<ResearchUrl> = Queue::new(
        db.pool.clone(),
        url_queue_config(Some(IntegrationName::Reddit)),
    );

    // Claim with Instagram affinity - should get nothing
    let claimed = instagram_queue.claim("worker-1", 1, stale).await?;
    assert_eq!(
        claimed.len(),
        0,
        "Instagram worker should not claim Reddit URL"
    );

    // Generic claim_urls should also not get it (has affinity)
    let claimed = db
        .url_queue_generic
        .claim("worker-generic", 10, stale)
        .await?;
    assert_eq!(
        claimed.len(),
        0,
        "Generic worker should not claim Reddit URL"
    );

    // Claim with Reddit affinity - should get the URL
    let claimed = reddit_queue.claim("worker-2", 1, stale).await?;
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, reddit_id);

    Ok(())
}

// ==================== submit_url worker affinity tests ====================

/// Helper to assert the affinity of a submitted URL.
async fn assert_affinity(
    db: &Database,
    url_id: &crate::types::ResearchUrlId,
    expected: Option<&str>,
) -> DbResult<()> {
    let affinity: Option<(Option<String>,)> =
        sqlx::query_as("SELECT worker_affinity FROM research_urls WHERE id = ?")
            .bind(url_id.as_str())
            .fetch_optional(&db.pool)
            .await?;
    assert_eq!(
        affinity.map(|(a,)| a),
        Some(expected.map(String::from)),
        "URL {} should have affinity {:?}",
        url_id.as_str(),
        expected
    );
    Ok(())
}

#[tokio::test]
async fn test_submit_url_sets_reddit_affinity() -> DbResult<()> {
    let (db, user_id) = setup().await?;
    let (url_id, _) = db
        .submit_url(&user_id, "https://reddit.com/r/rust/comments/abc123")
        .await?;
    assert_affinity(&db, &url_id, Some("reddit")).await
}

#[tokio::test]
async fn test_submit_url_sets_instagram_affinity() -> DbResult<()> {
    let (db, user_id) = setup().await?;
    let (url_id, _) = db
        .submit_url(&user_id, "https://instagram.com/p/ABC123xyz")
        .await?;
    assert_affinity(&db, &url_id, Some("instagram")).await
}

#[tokio::test]
async fn test_submit_url_sets_no_affinity_for_generic_url() -> DbResult<()> {
    let (db, user_id) = setup().await?;
    let (url_id, _) = db
        .submit_url(&user_id, "https://example.com/article")
        .await?;
    assert_affinity(&db, &url_id, None).await
}

#[tokio::test]
async fn test_submit_url_www_subdomain_gets_affinity() -> DbResult<()> {
    let (db, user_id) = setup().await?;
    let (url_id, _) = db
        .submit_url(&user_id, "https://www.reddit.com/r/rust")
        .await?;
    assert_affinity(&db, &url_id, Some("reddit")).await
}

#[tokio::test]
async fn test_claim_urls_generic_ignores_affinity_urls() -> DbResult<()> {
    let (db, user_id) = setup().await?;

    // Submit a Reddit URL (gets affinity "reddit" from built-in registry)
    db.submit_url(&user_id, "https://reddit.com/r/rust/comments/abc123")
        .await?;

    // Submit a generic URL (no affinity - domain not in registry)
    let (generic_id, _) = db
        .submit_url(&user_id, "https://example.com/article")
        .await?;

    let stale = Utc::now().naive_utc() - Duration::hours(1);

    // Generic claim_urls should only get the generic URL, not the Reddit one
    let claimed = db.url_queue_generic.claim("worker-1", 10, stale).await?;
    assert_eq!(
        claimed.len(),
        1,
        "Generic worker should only claim generic URLs"
    );
    assert_eq!(claimed[0].id, generic_id);

    Ok(())
}

#[tokio::test]
async fn test_claim_urls_affinity_ignores_generic_urls() -> DbResult<()> {
    let (db, user_id) = setup().await?;

    // Submit a generic URL (no affinity)
    db.submit_url(&user_id, "https://example.com/article")
        .await?;

    let stale = Utc::now().naive_utc() - Duration::hours(1);

    // Create Reddit queue
    let reddit_queue: Queue<ResearchUrl> = Queue::new(
        db.pool.clone(),
        url_queue_config(Some(IntegrationName::Reddit)),
    );

    // Reddit worker should not claim generic URLs
    let claimed = reddit_queue.claim("worker-1", 10, stale).await?;
    assert_eq!(
        claimed.len(),
        0,
        "Reddit worker should not claim generic URLs"
    );

    Ok(())
}
