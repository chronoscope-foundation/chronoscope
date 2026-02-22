//! Dossier tests: resolved content, media, GPS, deduplication

use super::*;

// Test coordinates: Gary, Indiana - home of the Jackson 5
const GARY_INDIANA_LAT: f64 = 41.5908;
const GARY_INDIANA_LON: f64 = -87.3467;
const GARY_INDIANA_ALT: f64 = 180.5;

#[tokio::test]
async fn test_dossier_pending_has_no_resolved_content() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let id = ctx
        .add_research(&token, "https://example.com/pending")
        .await?;

    let dossier: ResearchUrlDossier = ctx.get(&format!("/research/{id}")).await?.json().await?;

    assert_eq!(dossier.url, "https://example.com/pending");
    assert_eq!(dossier.status, ResearchUrlStatus::Pending);
    assert!(
        dossier.resolved.is_none(),
        "Pending URL should have no resolved content"
    );
    Ok(())
}

#[tokio::test]
async fn test_dossier_with_page_content() -> TestResult {
    use crate::research_types::ResolvedContent;

    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Create URL and resolve it to a page
    let url_id = ctx
        .add_research(&token, "https://reddit.com/r/test/post")
        .await?;
    let page_data = TestContext::test_page_data(SourceType::Reddit, &[]);
    ctx.create_page_for_url(&url_id, &page_data).await?;

    let dossier: ResearchUrlDossier = ctx
        .get(&format!("/research/{url_id}"))
        .await?
        .json()
        .await?;

    assert_eq!(dossier.status, ResearchUrlStatus::Complete);
    let page = match &dossier.resolved {
        Some(ResolvedContent::Page(page)) => page,
        Some(ResolvedContent::Media(_)) => return Err("Expected page content, got media".into()),
        None => return Err("Expected resolved content, got none".into()),
    };
    assert_eq!(page.source_type, SourceType::Reddit);
    assert_eq!(page.title.as_deref(), Some("Test Post"));
    assert_eq!(page.author.as_deref(), Some("testuser"));
    assert_eq!(page.content.as_deref(), Some("This is test content"));
    assert!(page.media.is_empty(), "Page should have no media yet");
    Ok(())
}

#[tokio::test]
async fn test_dossier_with_direct_media() -> TestResult {
    use crate::research_types::ResolvedContent;

    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Create URL and resolve it directly to media
    let url_id = ctx
        .add_research(&token, "https://example.com/image.jpg")
        .await?;
    let media_data = TestContext::test_media_data(&[0x01, 0x02, 0x03, 0x04]);
    ctx.create_media_for_url(&url_id, &media_data).await?;

    let dossier: ResearchUrlDossier = ctx
        .get(&format!("/research/{url_id}"))
        .await?
        .json()
        .await?;

    assert_eq!(dossier.status, ResearchUrlStatus::Complete);
    let media = match &dossier.resolved {
        Some(ResolvedContent::Media(media)) => media,
        Some(ResolvedContent::Page(_)) => return Err("Expected media content, got page".into()),
        None => return Err("Expected resolved content, got none".into()),
    };
    assert_eq!(media.media_type, MediaType::Image);
    assert_eq!(media.width, 800);
    assert_eq!(media.height, 600);
    assert!(media.thumbnail_url.contains("chronoscope.io"));
    assert!(media.full_url.contains("chronoscope.io"));
    Ok(())
}

#[tokio::test]
async fn test_dossier_page_with_fetched_media() -> TestResult {
    use crate::research_types::{MediaReference, ResolvedContent};

    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let media_url = "https://instagram.com/media/img1.jpg";

    // Create media URL first so we can resolve it later
    let media_url_id = ctx.add_research(&token, media_url).await?;

    // Create page URL with media included
    let page_url_id = ctx
        .add_research(&token, "https://instagram.com/p/abc123")
        .await?;
    let page_data = TestContext::test_page_data(SourceType::Instagram, &[media_url]);
    ctx.create_page_for_url(&page_url_id, &page_data).await?;

    // Resolve the media
    let media_data = TestContext::test_media_data(&[0x10, 0x20, 0x30]);
    ctx.create_media_for_url(&media_url_id, &media_data).await?;

    let dossier: ResearchUrlDossier = ctx
        .get(&format!("/research/{page_url_id}"))
        .await?
        .json()
        .await?;

    assert_eq!(dossier.status, ResearchUrlStatus::Complete);
    let page = match &dossier.resolved {
        Some(ResolvedContent::Page(page)) => page,
        _ => return Err("Expected page content".into()),
    };
    assert_eq!(page.media.len(), 1);
    let media = match &page.media[0] {
        MediaReference::Fetched(media) => media,
        MediaReference::Pending { .. } => return Err("Expected fetched media, got pending".into()),
    };
    assert_eq!(media.media_type, MediaType::Image);
    assert_eq!(media.width, 800);
    Ok(())
}

#[tokio::test]
async fn test_dossier_page_with_pending_media() -> TestResult {
    use crate::research_types::{MediaReference, ResolvedContent};

    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let media_url = "https://instagram.com/media/pending.jpg";

    // Create page URL with pending media included (media URL not resolved)
    let page_url_id = ctx
        .add_research(&token, "https://instagram.com/p/def456")
        .await?;
    let page_data = TestContext::test_page_data(SourceType::Instagram, &[media_url]);
    ctx.create_page_for_url(&page_url_id, &page_data).await?;

    let dossier: ResearchUrlDossier = ctx
        .get(&format!("/research/{page_url_id}"))
        .await?
        .json()
        .await?;

    // Page URL is complete even though embedded media is still pending
    assert_eq!(dossier.status, ResearchUrlStatus::Complete);
    let page = match &dossier.resolved {
        Some(ResolvedContent::Page(page)) => page,
        _ => return Err("Expected page content".into()),
    };
    assert_eq!(page.media.len(), 1);
    let source_url = match &page.media[0] {
        MediaReference::Pending { source_url } => source_url,
        MediaReference::Fetched(_) => return Err("Expected pending media, got fetched".into()),
    };
    assert_eq!(source_url, "https://instagram.com/media/pending.jpg");
    Ok(())
}

#[tokio::test]
async fn test_dossier_page_with_mixed_media_order() -> TestResult {
    use crate::research_types::{MediaReference, ResolvedContent};

    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let media1_url = "https://i.redd.it/img1.jpg";
    let media2_url = "https://i.redd.it/img2.jpg";
    let media3_url = "https://i.redd.it/img3.jpg";

    // Create media URLs first so we can resolve some of them
    let media1_url_id = ctx.add_research(&token, media1_url).await?;
    let _media2_url_id = ctx.add_research(&token, media2_url).await?;
    let media3_url_id = ctx.add_research(&token, media3_url).await?;

    // Create page with all 3 media URLs
    let page_url_id = ctx
        .add_research(&token, "https://reddit.com/gallery")
        .await?;
    let page_data =
        TestContext::test_page_data(SourceType::Reddit, &[media1_url, media2_url, media3_url]);
    ctx.create_page_for_url(&page_url_id, &page_data).await?;

    // Resolve first and third media, leave second pending
    let media1_data = TestContext::test_media_data(&[0x01]);
    ctx.create_media_for_url(&media1_url_id, &media1_data)
        .await?;

    let media3_data = TestContext::test_media_data(&[0x03]);
    ctx.create_media_for_url(&media3_url_id, &media3_data)
        .await?;

    let dossier: ResearchUrlDossier = ctx
        .get(&format!("/research/{page_url_id}"))
        .await?
        .json()
        .await?;

    assert_eq!(dossier.status, ResearchUrlStatus::Complete);
    let page = match &dossier.resolved {
        Some(ResolvedContent::Page(page)) => page,
        _ => return Err("Expected page content".into()),
    };
    assert_eq!(page.media.len(), 3);

    // First should be fetched
    assert!(matches!(&page.media[0], MediaReference::Fetched(_)));

    // Second should be pending
    let source_url = match &page.media[1] {
        MediaReference::Pending { source_url } => source_url,
        _ => return Err("Expected pending media at index 1".into()),
    };
    assert_eq!(source_url, "https://i.redd.it/img2.jpg");

    // Third should be fetched
    assert!(matches!(&page.media[2], MediaReference::Fetched(_)));
    Ok(())
}

#[tokio::test]
async fn test_dossier_media_with_gps_location() -> TestResult {
    use crate::research_types::ResolvedContent;

    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let url_id = ctx
        .add_research(&token, "https://example.com/geotagged.jpg")
        .await?;

    // Create media with GPS data
    let media_data = MediaData {
        exact_hash: vec![0xAB, 0xCD, 0xEF],
        perceptual_hash: None,
        storage_key: "test/geotagged.jpg".to_string(),
        media_type: MediaType::Image,
        width: 1920,
        height: 1080,
        duration_seconds: None,
        captured: Some(chronoscope_core::UncertainDate::exact(
            chrono::NaiveDate::from_ymd_opt(1965, 8, 15)
                .ok_or("valid date")?
                .and_hms_opt(12, 0, 0)
                .ok_or("valid time")?,
        )?),
        location: Some(
            chronoscope_core::UncertainLocation::coordinates(
                GARY_INDIANA_LAT,
                GARY_INDIANA_LON,
                Some(chronoscope_core::Elevation::SeaLevelOffset {
                    meters: GARY_INDIANA_ALT as i32,
                }),
                None,
            )
            .expect("valid test coordinates"),
        ),
        source_metadata: None,
        fetched_at: chrono::Utc::now().naive_utc(),
    };
    ctx.create_media_for_url(&url_id, &media_data).await?;

    let dossier: ResearchUrlDossier = ctx
        .get(&format!("/research/{url_id}"))
        .await?
        .json()
        .await?;

    assert_eq!(dossier.status, ResearchUrlStatus::Complete);
    let media = match &dossier.resolved {
        Some(ResolvedContent::Media(media)) => media,
        _ => return Err("Expected media content".into()),
    };
    assert_eq!(media.width, 1920);
    assert_eq!(media.height, 1080);
    assert!(media.captured.is_some());

    let location = media.location.as_ref().ok_or("should have GPS location")?;
    if let chronoscope_core::UncertainLocation::Coordinates {
        lat,
        lon,
        elevation,
        ..
    } = location
    {
        assert_eq!(*lat, GARY_INDIANA_LAT);
        assert_eq!(*lon, GARY_INDIANA_LON);
        assert_eq!(
            *elevation,
            Some(chronoscope_core::Elevation::SeaLevelOffset {
                meters: GARY_INDIANA_ALT as i32,
            })
        );
    } else {
        return Err("expected Coordinates location".into());
    }
    Ok(())
}

#[tokio::test]
async fn test_dossier_failed_url_shows_status() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let url_id = ctx
        .add_research(&token, "https://example.com/will-fail")
        .await?;

    // Mark the URL as failed (simulating worker failure)
    ctx.mark_url_failed(&url_id, "Connection timeout").await?;

    let dossier: ResearchUrlDossier = ctx
        .get(&format!("/research/{url_id}"))
        .await?
        .json()
        .await?;

    assert_eq!(dossier.status, ResearchUrlStatus::Failed);
    assert!(
        dossier.resolved.is_none(),
        "Failed URL should have no resolved content"
    );
    Ok(())
}

#[tokio::test]
async fn test_media_deduplication_returns_same_id() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Create two different URLs (different paths, same eventual content)
    let url1 = ctx
        .add_research(&token, "https://example.com/image1.jpg")
        .await?;
    let url2 = ctx
        .add_research(&token, "https://example.com/alternate/same-image.jpg")
        .await?;

    // Resolve both URLs to media with the SAME hash (simulating same image from different sources)
    let same_hash = &[0xDE, 0xAD, 0xBE, 0xEF];
    let media_data = TestContext::test_media_data(same_hash);

    let media_id1 = ctx.create_media_for_url(&url1, &media_data).await?;
    let media_id2 = ctx.create_media_for_url(&url2, &media_data).await?;

    // Should return the same media ID (deduplicated by hash)
    assert_eq!(
        media_id1, media_id2,
        "Same hash should produce same media ID"
    );

    // Both dossiers should reference the same media
    let dossier1: ResearchUrlDossier = ctx.get(&format!("/research/{url1}")).await?.json().await?;
    let dossier2: ResearchUrlDossier = ctx.get(&format!("/research/{url2}")).await?.json().await?;

    use crate::research_types::ResolvedContent;
    let (m1, m2) = match (&dossier1.resolved, &dossier2.resolved) {
        (Some(ResolvedContent::Media(m1)), Some(ResolvedContent::Media(m2))) => (m1, m2),
        _ => return Err("Expected both URLs to resolve to media".into()),
    };
    assert_eq!(m1.id, m2.id, "Both URLs should resolve to same media");
    Ok(())
}

#[tokio::test]
async fn test_dossier_video_with_duration() -> TestResult {
    use crate::research_types::ResolvedContent;

    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let url_id = ctx
        .add_research(&token, "https://example.com/video.mp4")
        .await?;

    // Create video media with duration
    let media_data = MediaData {
        exact_hash: vec![0x01, 0xDE, 0x00],
        perceptual_hash: None,
        storage_key: "test/video.mp4".to_string(),
        media_type: MediaType::Video,
        width: 1920,
        height: 1080,
        duration_seconds: Some(125.5), // 2 min 5.5 sec
        captured: None,
        location: None,
        source_metadata: None,
        fetched_at: chrono::Utc::now().naive_utc(),
    };
    ctx.create_media_for_url(&url_id, &media_data).await?;

    let dossier: ResearchUrlDossier = ctx
        .get(&format!("/research/{url_id}"))
        .await?
        .json()
        .await?;

    assert_eq!(dossier.status, ResearchUrlStatus::Complete);
    match &dossier.resolved {
        Some(ResolvedContent::Media(media)) => {
            assert_eq!(media.media_type, MediaType::Video);
            assert_eq!(media.width, 1920);
            assert_eq!(media.height, 1080);
            assert_eq!(media.duration_seconds, Some(125.5));
        }
        _ => return Err("Expected video media content".into()),
    }
    Ok(())
}

/// Tests that `create_page` correctly handles a mix of pre-existing and new media URLs.
/// This exercises the batch INSERT OR IGNORE + JOIN pattern.
#[tokio::test]
async fn test_create_page_with_mixed_existing_and_new_urls() -> TestResult {
    use crate::research_types::{MediaReference, ResolvedContent};

    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Pre-existing URLs: create these via add_research before creating the page
    let existing_url1 = "https://example.com/existing1.jpg";
    let existing_url2 = "https://example.com/existing2.jpg";
    let existing_url1_id = ctx.add_research(&token, existing_url1).await?;
    let existing_url2_id = ctx.add_research(&token, existing_url2).await?;

    // Resolve one of the existing URLs to verify it stays resolved
    let media_data = TestContext::test_media_data(&[0xAA, 0xBB]);
    ctx.create_media_for_url(&existing_url1_id, &media_data)
        .await?;

    // New URLs: these don't exist yet, create_page should create them
    let new_url1 = "https://example.com/brand-new1.jpg";
    let new_url2 = "https://example.com/brand-new2.jpg";

    // Create page with a mix: 2 existing + 2 new, interleaved
    let page_url_id = ctx
        .add_research(&token, "https://example.com/mixed-page")
        .await?;
    let page_data = TestContext::test_page_data(
        SourceType::Generic,
        &[existing_url1, new_url1, existing_url2, new_url2],
    );
    ctx.create_page_for_url(&page_url_id, &page_data).await?;

    // Fetch the dossier and verify all 4 media references are present in correct order
    let dossier: ResearchUrlDossier = ctx
        .get(&format!("/research/{page_url_id}"))
        .await?
        .json()
        .await?;

    assert_eq!(dossier.status, ResearchUrlStatus::Complete);

    let page = match &dossier.resolved {
        Some(ResolvedContent::Page(page)) => page,
        _ => return Err("Expected page content".into()),
    };

    // Extract the pattern: Fetched vs Pending with source_url
    let media_pattern: Vec<Option<&str>> = page
        .media
        .iter()
        .map(|m| match m {
            MediaReference::Fetched(_) => None,
            MediaReference::Pending { source_url } => Some(source_url.as_str()),
        })
        .collect();

    // Expected: existing_url1 resolved (None), new_url1 pending, existing_url2 pending, new_url2 pending
    assert_eq!(
        media_pattern,
        vec![None, Some(new_url1), Some(existing_url2), Some(new_url2)],
        "Media should be: fetched, pending(new1), pending(existing2), pending(new2)"
    );

    // Verify resolving existing_url2 (linked via batch) updates the dossier correctly
    let media_data2 = TestContext::test_media_data(&[0xCC, 0xDD]);
    ctx.create_media_for_url(&existing_url2_id, &media_data2)
        .await?;

    let dossier2: ResearchUrlDossier = ctx
        .get(&format!("/research/{page_url_id}"))
        .await?
        .json()
        .await?;

    let page2 = match &dossier2.resolved {
        Some(ResolvedContent::Page(page)) => page,
        _ => return Err("Expected page content".into()),
    };

    let media_pattern2: Vec<Option<&str>> = page2
        .media
        .iter()
        .map(|m| match m {
            MediaReference::Fetched(_) => None,
            MediaReference::Pending { source_url } => Some(source_url.as_str()),
        })
        .collect();

    // Now existing_url2 should also be fetched
    assert_eq!(
        media_pattern2,
        vec![None, Some(new_url1), None, Some(new_url2)],
        "After resolving existing_url2: fetched, pending, fetched, pending"
    );

    Ok(())
}
