//! URL security validation to prevent SSRF attacks.
//!
//! This module provides validation for URLs before they're stored or fetched,
//! blocking requests to private networks, cloud metadata services, and other
//! potentially dangerous destinations.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use dropshot::HttpError;

use crate::state::DnsResolver;

/// Maximum allowed URL length in bytes.
/// Generous limit to accommodate URLs with long query strings.
pub const MAX_URL_LENGTH: usize = 8192;

/// Validate URL format (length, scheme, structure) without DNS resolution.
///
/// This performs the synchronous checks that don't require network access.
/// Used internally by `validate_url` and exposed for unit testing.
///
/// # Errors
///
/// Returns `HttpError` if the URL fails format validation.
fn validate_url_format(url_str: &str) -> Result<url::Url, HttpError> {
    // Check length first (before parsing to avoid DoS)
    if url_str.len() > MAX_URL_LENGTH {
        return Err(HttpError::for_bad_request(
            None,
            format!("URL exceeds maximum length of {MAX_URL_LENGTH} bytes"),
        ));
    }

    // Parse URL
    let url = url::Url::parse(url_str)
        .map_err(|e| HttpError::for_bad_request(None, format!("Invalid URL: {e}")))?;

    // Check scheme
    if !["http", "https"].contains(&url.scheme()) {
        return Err(HttpError::for_bad_request(
            None,
            "Only HTTP and HTTPS URLs are allowed".to_string(),
        ));
    }

    // Check host exists
    url.host_str()
        .ok_or_else(|| HttpError::for_bad_request(None, "URL must have a host".to_string()))?;

    Ok(url)
}

/// Validate a URL for security issues.
///
/// Checks:
/// - URL length is within limits
/// - URL can be parsed
/// - Scheme is HTTP or HTTPS
/// - Host does not resolve to a private/reserved IP address
///
/// The resolver should be created once at startup and reused - it maintains
/// connection pools and caches DNS responses.
///
/// # Errors
///
/// Returns `HttpError` if the URL fails any security check.
pub async fn validate_url(
    url_str: &str,
    resolver: &(impl DnsResolver + ?Sized),
) -> Result<url::Url, HttpError> {
    let url = validate_url_format(url_str)?;

    let host = url
        .host_str()
        .ok_or_else(|| HttpError::for_bad_request(None, "URL must have a host".to_string()))?;

    validate_host(host, resolver).await?;

    Ok(url)
}

/// Validate that a host does not resolve to any blocked IP addresses.
///
/// Uses the provided DNS resolver for async resolution.
///
/// # Errors
///
/// Returns `HttpError` if:
/// - DNS resolution fails
/// - Any resolved IP is in a blocked range
async fn validate_host(
    host: &str,
    resolver: &(impl DnsResolver + ?Sized),
) -> Result<(), HttpError> {
    // Try to parse as IP address first (no DNS needed)
    if let Ok(ip) = host.parse::<IpAddr>() {
        return validate_ip(&ip);
    }

    // Resolve hostname to IP addresses using the provided resolver
    let ips = resolver.lookup_ip(host).await?;

    if ips.is_empty() {
        return Err(HttpError::for_bad_request(
            None,
            "Host did not resolve to any IP addresses".to_string(),
        ));
    }

    for ip in ips {
        validate_ip(&ip)?;
    }

    Ok(())
}

/// Check if an IP address is in a blocked range.
///
/// Blocked ranges include:
/// - Loopback (127.0.0.0/8, ::1)
/// - Private networks (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16)
/// - Link-local (169.254.0.0/16 - includes AWS IMDS at 169.254.169.254)
/// - IPv6 link-local (fe80::/10)
/// - Multicast
/// - Broadcast
/// - Documentation ranges
/// - Other reserved ranges
///
/// # Errors
///
/// Returns `HttpError` if the IP is in a blocked range.
fn validate_ip(ip: &IpAddr) -> Result<(), HttpError> {
    let blocked_reason = match ip {
        IpAddr::V4(ipv4) => check_ipv4_blocked(*ipv4),
        IpAddr::V6(ipv6) => check_ipv6_blocked(ipv6),
    };

    if let Some(reason) = blocked_reason {
        return Err(HttpError {
            status_code: dropshot::ErrorStatusCode::BAD_REQUEST,
            error_code: None,
            external_message: "URL not allowed".to_string(),
            internal_message: format!("Blocked IP {ip}: {reason}"),
            headers: None,
        });
    }

    Ok(())
}

/// Check if an IPv4 address is in a blocked range.
/// Returns the reason if blocked, None if allowed.
fn check_ipv4_blocked(ip: Ipv4Addr) -> Option<&'static str> {
    let octets = ip.octets();

    // Loopback: 127.0.0.0/8
    if octets[0] == 127 {
        return Some("loopback");
    }

    // Private: 10.0.0.0/8
    if octets[0] == 10 {
        return Some("private network");
    }

    // Private: 172.16.0.0/12 (172.16.0.0 - 172.31.255.255)
    if octets[0] == 172 && (16..=31).contains(&octets[1]) {
        return Some("private network");
    }

    // Private: 192.168.0.0/16
    if octets[0] == 192 && octets[1] == 168 {
        return Some("private network");
    }

    // Link-local: 169.254.0.0/16 (includes AWS IMDS at 169.254.169.254)
    if octets[0] == 169 && octets[1] == 254 {
        return Some("link-local/cloud metadata");
    }

    // Broadcast: 255.255.255.255
    if ip.is_broadcast() {
        return Some("broadcast");
    }

    // Documentation: 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24
    if (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
    {
        return Some("documentation range");
    }

    // Shared address space: 100.64.0.0/10 (carrier-grade NAT)
    if octets[0] == 100 && (64..=127).contains(&octets[1]) {
        return Some("shared address space");
    }

    // Multicast: 224.0.0.0/4
    if ip.is_multicast() {
        return Some("multicast");
    }

    // Unspecified: 0.0.0.0
    if ip.is_unspecified() {
        return Some("unspecified");
    }

    // Reserved for future use: 240.0.0.0/4 (except broadcast)
    if octets[0] >= 240 && !ip.is_broadcast() {
        return Some("reserved");
    }

    None
}

/// Check if an IPv6 address is in a blocked range.
/// Returns the reason if blocked, None if allowed.
fn check_ipv6_blocked(ip: &Ipv6Addr) -> Option<&'static str> {
    // Loopback: ::1
    if ip.is_loopback() {
        return Some("loopback");
    }

    // Unspecified: ::
    if ip.is_unspecified() {
        return Some("unspecified");
    }

    // Multicast: ff00::/8
    if ip.is_multicast() {
        return Some("multicast");
    }

    // Link-local: fe80::/10
    let segments = ip.segments();
    if (segments[0] & 0xffc0) == 0xfe80 {
        return Some("link-local");
    }

    // Unique local: fc00::/7 (private in IPv6)
    if (segments[0] & 0xfe00) == 0xfc00 {
        return Some("unique local/private");
    }

    // IPv4-mapped: ::ffff:0:0/96 - check the embedded IPv4
    if let Some(ipv4) = ip.to_ipv4_mapped() {
        return check_ipv4_blocked(ipv4);
    }

    // Documentation: 2001:db8::/32
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return Some("documentation range");
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    // ==================== URL Length Tests ====================

    #[test]
    fn test_url_length_limit() {
        let long_path = "a".repeat(MAX_URL_LENGTH);
        let long_url = format!("https://example.com/{long_path}");
        assert!(validate_url_format(&long_url).is_err());
    }

    #[test]
    fn test_url_at_length_limit() {
        // URL just at the limit should be allowed
        let url = format!("https://example.com/{}", "a".repeat(MAX_URL_LENGTH - 30));
        let result = validate_url_format(&url);
        // Should NOT be a length error
        assert!(
            !result
                .as_ref()
                .err()
                .map(|e| e.external_message.contains("length"))
                .unwrap_or(false)
        );
    }

    // ==================== IPv4 Blocking Tests ====================

    #[test]
    fn test_loopback_blocked() -> TestResult {
        assert!(check_ipv4_blocked("127.0.0.1".parse()?).is_some());
        assert!(check_ipv4_blocked("127.255.255.255".parse()?).is_some());
        Ok(())
    }

    #[test]
    fn test_private_10_blocked() -> TestResult {
        assert!(check_ipv4_blocked("10.0.0.1".parse()?).is_some());
        assert!(check_ipv4_blocked("10.255.255.255".parse()?).is_some());
        Ok(())
    }

    #[test]
    fn test_private_172_blocked() -> TestResult {
        assert!(check_ipv4_blocked("172.16.0.1".parse()?).is_some());
        assert!(check_ipv4_blocked("172.31.255.255".parse()?).is_some());
        // 172.15.x.x and 172.32.x.x are NOT private
        assert!(check_ipv4_blocked("172.15.0.1".parse()?).is_none());
        assert!(check_ipv4_blocked("172.32.0.1".parse()?).is_none());
        Ok(())
    }

    #[test]
    fn test_private_192_168_blocked() -> TestResult {
        assert!(check_ipv4_blocked("192.168.0.1".parse()?).is_some());
        assert!(check_ipv4_blocked("192.168.255.255".parse()?).is_some());
        Ok(())
    }

    #[test]
    fn test_link_local_blocked() -> TestResult {
        // Includes AWS IMDS
        assert!(check_ipv4_blocked("169.254.169.254".parse()?).is_some());
        assert!(check_ipv4_blocked("169.254.0.1".parse()?).is_some());
        Ok(())
    }

    #[test]
    fn test_multicast_blocked() -> TestResult {
        assert!(check_ipv4_blocked("224.0.0.1".parse()?).is_some());
        assert!(check_ipv4_blocked("239.255.255.255".parse()?).is_some());
        Ok(())
    }

    #[test]
    fn test_public_ips_allowed() -> TestResult {
        assert!(check_ipv4_blocked("8.8.8.8".parse()?).is_none());
        assert!(check_ipv4_blocked("1.1.1.1".parse()?).is_none());
        assert!(check_ipv4_blocked("93.184.216.34".parse()?).is_none()); // example.com
        Ok(())
    }

    // ==================== IPv6 Blocking Tests ====================

    #[test]
    fn test_ipv6_loopback_blocked() -> TestResult {
        assert!(check_ipv6_blocked(&"::1".parse()?).is_some());
        Ok(())
    }

    #[test]
    fn test_ipv6_link_local_blocked() -> TestResult {
        assert!(check_ipv6_blocked(&"fe80::1".parse()?).is_some());
        Ok(())
    }

    #[test]
    fn test_ipv6_unique_local_blocked() -> TestResult {
        assert!(check_ipv6_blocked(&"fc00::1".parse()?).is_some());
        assert!(check_ipv6_blocked(&"fd00::1".parse()?).is_some());
        Ok(())
    }

    #[test]
    fn test_ipv6_public_allowed() -> TestResult {
        assert!(check_ipv6_blocked(&"2606:4700:4700::1111".parse()?).is_none()); // Cloudflare
        Ok(())
    }

    #[test]
    fn test_ipv4_mapped_ipv6_blocked() -> TestResult {
        // IPv4-mapped IPv6 addresses (::ffff:x.x.x.x) should check the embedded IPv4
        // These are a common SSRF bypass vector
        assert!(
            check_ipv6_blocked(&"::ffff:127.0.0.1".parse()?).is_some(),
            "IPv4-mapped loopback should be blocked"
        );
        assert!(
            check_ipv6_blocked(&"::ffff:10.0.0.1".parse()?).is_some(),
            "IPv4-mapped private 10.x should be blocked"
        );
        assert!(
            check_ipv6_blocked(&"::ffff:169.254.169.254".parse()?).is_some(),
            "IPv4-mapped AWS IMDS should be blocked"
        );
        assert!(
            check_ipv6_blocked(&"::ffff:192.168.1.1".parse()?).is_some(),
            "IPv4-mapped private 192.168.x should be blocked"
        );
        // Public IPv4-mapped should be allowed
        assert!(
            check_ipv6_blocked(&"::ffff:93.184.216.34".parse()?).is_none(),
            "IPv4-mapped public IP should be allowed"
        );
        Ok(())
    }

    // ==================== Scheme Tests ====================

    #[test]
    fn test_invalid_schemes_rejected() {
        assert!(validate_url_format("ftp://example.com/file").is_err());
        assert!(validate_url_format("file:///etc/passwd").is_err());
        assert!(validate_url_format("javascript:alert(1)").is_err());
    }

    #[test]
    fn test_valid_schemes_accepted() {
        assert!(validate_url_format("http://example.com/").is_ok());
        assert!(validate_url_format("https://example.com/").is_ok());
    }

    // ==================== SSRF via DNS Tests ====================

    use std::collections::HashMap;

    struct MockResolver(HashMap<String, Vec<IpAddr>>);

    #[async_trait]
    impl DnsResolver for MockResolver {
        async fn lookup_ip(&self, host: &str) -> Result<Vec<IpAddr>, HttpError> {
            self.0
                .get(host)
                .cloned()
                .ok_or_else(|| HttpError::for_bad_request(None, format!("Host not found: {host}")))
        }
    }

    #[tokio::test]
    async fn test_ssrf_blocked_ips_via_dns() -> TestResult {
        let cases: Vec<(&str, &str)> = vec![
            ("127.0.0.1", "loopback"),
            ("10.0.0.1", "private 10.x"),
            ("172.16.0.1", "private 172.16.x"),
            ("192.168.1.1", "private 192.168.x"),
            ("169.254.169.254", "AWS IMDS"),
            ("::1", "IPv6 loopback"),
            ("fe80::1", "IPv6 link-local"),
            ("fc00::1", "IPv6 unique local"),
        ];

        for (ip, desc) in cases {
            let host = format!("{}.evil.test", ip.replace([':', '.'], "-"));
            let resolver = MockResolver([(host.clone(), vec![ip.parse()?])].into_iter().collect());

            let result = validate_url(&format!("https://{host}/"), &resolver).await;
            assert!(result.is_err(), "{desc} ({ip}) should be blocked");
            let err = result.err().ok_or("expected error")?;
            assert_eq!(
                err.external_message, "URL not allowed",
                "{desc} external message"
            );
            assert!(
                err.internal_message.contains(ip),
                "{desc} internal message should contain IP, got: {}",
                err.internal_message
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_public_ips_allowed_via_dns() -> TestResult {
        let resolver = MockResolver(
            [
                ("example.com".into(), vec!["93.184.216.34".parse()?]),
                ("cloudflare.com".into(), vec!["104.16.132.229".parse()?]),
            ]
            .into_iter()
            .collect(),
        );

        assert!(
            validate_url("https://example.com/", &resolver)
                .await
                .is_ok()
        );
        assert!(
            validate_url("https://cloudflare.com/", &resolver)
                .await
                .is_ok()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_mixed_ips_blocked_if_any_unsafe() -> TestResult {
        let resolver = MockResolver(
            [(
                "mixed.test".into(),
                vec!["93.184.216.34".parse()?, "127.0.0.1".parse()?],
            )]
            .into_iter()
            .collect(),
        );

        let result = validate_url("https://mixed.test/", &resolver).await;
        assert!(result.is_err(), "should block if any IP is unsafe");
        Ok(())
    }
}
