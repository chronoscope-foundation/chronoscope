//! Tests for well-known endpoints (Apple App Site Association)

use super::*;

// ==================== Apple App Site Association Tests ====================

#[tokio::test]
async fn test_aasa_without_app_id() -> TestResult {
    let ctx = TestContext::new().await?;

    let resp = ctx.get("/.well-known/apple-app-site-association").await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;

    // Should have webcredentials.apps as empty array when no app ID configured
    assert!(body["webcredentials"]["apps"].is_array());
    assert_eq!(body["webcredentials"]["apps"].as_array().unwrap().len(), 0);
    Ok(())
}

#[tokio::test]
async fn test_aasa_with_app_id() -> TestResult {
    // Create a context with ios_app_id configured
    let ctx = TestContext::with_ios_app_id("ABCD1234.com.example.chronoscope").await?;

    let resp = ctx.get("/.well-known/apple-app-site-association").await?;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await?;

    // Should have webcredentials.apps with our app ID
    let apps = body["webcredentials"]["apps"].as_array().unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0], "ABCD1234.com.example.chronoscope");
    Ok(())
}

#[tokio::test]
async fn test_aasa_content_type() -> TestResult {
    let ctx = TestContext::new().await?;

    let resp = ctx.get("/.well-known/apple-app-site-association").await?;
    assert_eq!(resp.status(), 200);

    // Dropshot returns application/json for HttpResponseOk
    let content_type = resp.headers().get("content-type").unwrap();
    assert!(content_type.to_str().unwrap().contains("application/json"));
    Ok(())
}
