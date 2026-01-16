//! Tests for the embedded media endpoint.

use bytes::Bytes;
use chronoscope_db::media_store::MediaStore;

use super::{TestContext, TestResult};

#[tokio::test]
async fn test_get_media_returns_stored_content() -> TestResult {
    let (ctx, store) = TestContext::with_media_store().await?;

    // Store some media (storage key includes media/ prefix)
    let content = Bytes::from_static(b"fake jpeg data");
    store
        .put("media/image.jpg", content.clone(), "image/jpeg")
        .await?;

    // Fetch via endpoint (URL path is /media/{key} where key excludes media/ prefix)
    let resp = ctx.get("/media/image.jpg").await?;

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").map(|v| v.to_str().ok()),
        Some(Some("image/jpeg"))
    );
    assert_eq!(resp.bytes().await?, content);

    Ok(())
}

#[tokio::test]
async fn test_get_media_not_found() -> TestResult {
    let (ctx, _store) = TestContext::with_media_store().await?;

    let resp = ctx.get("/media/nonexistent.jpg").await?;

    assert_eq!(resp.status(), 404);

    Ok(())
}

#[tokio::test]
async fn test_get_media_cache_headers() -> TestResult {
    let (ctx, store) = TestContext::with_media_store().await?;

    store
        .put(
            "media/cached.png",
            Bytes::from_static(b"png data"),
            "image/png",
        )
        .await?;

    let resp = ctx.get("/media/cached.png").await?;

    assert_eq!(resp.status(), 200);
    let cache_control = resp.headers().get("cache-control").map(|v| v.to_str().ok());
    assert_eq!(
        cache_control,
        Some(Some("public, max-age=31536000, immutable"))
    );

    Ok(())
}

#[tokio::test]
async fn test_get_media_rejects_path_traversal() -> TestResult {
    let (ctx, store) = TestContext::with_media_store().await?;

    // Store something at a path that could be targeted via traversal
    store
        .put(
            "media/secret.txt",
            Bytes::from_static(b"secret"),
            "text/plain",
        )
        .await?;

    // Try to access with path traversal embedded in the key.
    // Note: /media/../x gets normalized by HTTP layer, so we use a key with embedded ..
    // that wouldn't be normalized (e.g., URL-encoded or in middle of path segment).
    // The check is defensive against keys that somehow contain ".." after decoding.
    let resp = ctx.get("/media/foo%2F..%2Fsecret.txt").await?;

    // Should be rejected with 400, not serve the file
    assert_eq!(resp.status(), 400);

    Ok(())
}
