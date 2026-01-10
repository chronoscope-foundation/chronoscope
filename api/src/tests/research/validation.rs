//! URL validation and SSRF protection tests

use super::*;

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
