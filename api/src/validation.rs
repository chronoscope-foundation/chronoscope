//! Shared validation utilities for request data.

use chronoscope_db::DbError;
use dropshot::{Body, HttpError};
use http::Response;

// Re-export from db crate for convenience
pub use chronoscope_db::is_unique_violation;

/// Convert a `DbError` to an `HttpError` for use in API handlers.
///
/// This logs the detailed error internally (visible in Dropshot logs) while
/// returning a generic "Internal Server Error" to clients. CORS headers are
/// included so cross-origin clients can read the error response.
pub fn db_err(e: DbError) -> HttpError {
    let mut err = HttpError::for_internal_error(e.to_string());
    add_cors_headers(&mut err);
    err
}

/// Convert a fact-store backend error to an `HttpError`, mirroring [`db_err`].
///
/// Covers both `MemoryFactStore`'s own `Error` and the `listing`/`projection`
/// module wrapper errors (`ListError<E>` and friends) — none of them impl
/// `Display`, so this formats via `Debug`.
pub fn fact_store_err(e: impl std::fmt::Debug) -> HttpError {
    let mut err = HttpError::for_internal_error(format!("{e:?}"));
    add_cors_headers(&mut err);
    err
}

/// A `400 Bad Request` carrying CORS headers, for the public no-auth endpoints
/// whose malformed-input rejections a browser must be able to read cross-origin.
pub fn bad_request_with_cors(message: String) -> HttpError {
    let mut err = HttpError::for_bad_request(None, message);
    add_cors_headers(&mut err);
    err
}

/// CORS headers applied to all cross-origin responses — defined once,
/// used by both success responses (via `cors_builder`) and error
/// responses (via `add_cors_headers`).
const CORS_HEADERS: &[(http::HeaderName, &str)] = &[
    (http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
    (http::header::ACCESS_CONTROL_ALLOW_METHODS, "GET, OPTIONS"),
    (http::header::ACCESS_CONTROL_ALLOW_HEADERS, "Content-Type"),
    (http::header::ACCESS_CONTROL_MAX_AGE, "86400"),
];

/// Add CORS headers to an `HttpError` so cross-origin clients can read it.
fn add_cors_headers(err: &mut HttpError) {
    for (name, value) in CORS_HEADERS {
        if let Err(e) = err.add_header(name.clone(), http::HeaderValue::from_static(value)) {
            eprintln!("warn: failed to add CORS header {name} to error response: {e}");
        }
    }
}

/// Standard CORS headers for cross-origin access.
pub(crate) fn cors_builder() -> http::response::Builder {
    let mut builder = Response::builder();
    for (name, value) in CORS_HEADERS {
        builder = builder.header(name, *value);
    }
    builder
}

/// Wrap a serializable value in a JSON response with CORS headers.
///
/// Used by endpoints that need cross-origin access (e.g., entity endpoints
/// called from the web frontend on a different port).
pub fn json_with_cors<T: serde::Serialize>(value: &T) -> Result<Response<Body>, HttpError> {
    let body_bytes = serde_json::to_vec(value)
        .map_err(|e| HttpError::for_internal_error(format!("Failed to serialize response: {e}")))?;

    cors_builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(body_bytes.into())
        .map_err(|e| HttpError::for_internal_error(format!("Failed to build response: {e}")))
}

/// Return a JSON error response with CORS headers and the given status code.
///
/// Dropshot's `HttpError` path doesn't attach CORS headers, so cross-origin
/// clients (e.g., the web frontend on a different port) can't read the error
/// body. This helper attaches CORS headers so the browser lets the JS read
/// the 400 message.
pub fn error_with_cors(
    status: http::StatusCode,
    message: &str,
) -> Result<Response<Body>, HttpError> {
    let body_bytes = serde_json::to_vec(&serde_json::json!({"message": message}))
        .unwrap_or_else(|_| message.as_bytes().to_vec());

    cors_builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(body_bytes.into())
        .map_err(|e| HttpError::for_internal_error(format!("Failed to build error response: {e}")))
}

/// Empty CORS preflight response for OPTIONS requests.
pub fn cors_preflight() -> Result<Response<Body>, HttpError> {
    cors_builder()
        .status(http::StatusCode::NO_CONTENT)
        .body(Body::empty())
        .map_err(|e| HttpError::for_internal_error(format!("Failed to build response: {e}")))
}

/// Validate username format.
///
/// Requirements:
/// - 3-32 characters
/// - Alphanumeric, underscores, and hyphens only
///
/// # Errors
///
/// Returns `HttpError` if the username is too short, too long, or contains invalid characters.
pub fn validate_username(username: &str) -> Result<(), HttpError> {
    if username.len() < 3 {
        return Err(HttpError::for_bad_request(
            None,
            "Username must be at least 3 characters".to_string(),
        ));
    }
    if username.len() > 32 {
        return Err(HttpError::for_bad_request(
            None,
            "Username must be at most 32 characters".to_string(),
        ));
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(HttpError::for_bad_request(
            None,
            "Username can only contain letters, numbers, underscores, and hyphens".to_string(),
        ));
    }
    Ok(())
}

/// Validate email format (basic @ check).
///
/// This is a minimal check; full validation happens at delivery time.
///
/// # Errors
///
/// Returns `HttpError` if the email format is invalid.
pub fn validate_email(email: &str) -> Result<(), HttpError> {
    let parts: Vec<&str> = email.split('@').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(HttpError::for_bad_request(
            None,
            "Invalid email format".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Username Validation Tests ====================

    #[test]
    fn test_username_minimum_length() {
        assert!(validate_username("ab").is_err());
        assert!(validate_username("abc").is_ok());
    }

    #[test]
    fn test_username_maximum_length() {
        assert!(validate_username(&"a".repeat(32)).is_ok());
        assert!(validate_username(&"a".repeat(33)).is_err());
    }

    #[test]
    fn test_username_valid_characters() {
        // All valid character types
        assert!(validate_username("abcdefg").is_ok()); // lowercase
        assert!(validate_username("ABCDEFG").is_ok()); // uppercase
        assert!(validate_username("abc123").is_ok()); // with numbers
        assert!(validate_username("abc_def").is_ok()); // with underscore
        assert!(validate_username("abc-def").is_ok()); // with hyphen
        assert!(validate_username("A1_b-C").is_ok()); // mixed
    }

    #[test]
    fn test_username_invalid_characters() {
        assert!(validate_username("user name").is_err()); // space
        assert!(validate_username("user@name").is_err()); // @
        assert!(validate_username("user.name").is_err()); // .
        assert!(validate_username("user!name").is_err()); // !
        assert!(validate_username("user#name").is_err()); // #
        assert!(validate_username("user$name").is_err()); // $
        assert!(validate_username("user%name").is_err()); // %
    }

    #[test]
    fn test_username_empty() {
        assert!(validate_username("").is_err());
    }

    // ==================== Email Validation Tests ====================

    #[test]
    fn test_email_valid() {
        assert!(validate_email("user@example.com").is_ok());
        assert!(validate_email("a@b").is_ok()); // minimal valid
        assert!(validate_email("user+tag@example.com").is_ok());
    }

    #[test]
    fn test_email_missing_at() {
        assert!(validate_email("userexample.com").is_err());
        assert!(validate_email("user").is_err());
        assert!(validate_email("").is_err());
    }

    #[test]
    fn test_email_edge_cases() {
        // These are now correctly rejected (must have content on both sides of @)
        assert!(validate_email("@").is_err()); // just @
        assert!(validate_email("@b").is_err()); // no local part
        assert!(validate_email("a@").is_err()); // no domain
        assert!(validate_email("a@@b").is_err()); // multiple @
    }

    // ==================== db_err Tests ====================

    #[test]
    fn test_db_error_to_http_error_hides_details_but_logs_them() {
        // External message should be generic (sent to client)
        // Internal message should contain details (logged by Dropshot)
        let http_err = db_err(DbError::UserNotFound);

        assert!(http_err.status_code.is_server_error());
        assert_eq!(http_err.external_message, "Internal Server Error");
        assert_eq!(http_err.internal_message, "User not found");

        let http_err = db_err(DbError::CredentialNotFound);

        assert!(http_err.status_code.is_server_error());
        assert_eq!(http_err.external_message, "Internal Server Error");
        assert_eq!(http_err.internal_message, "Credential not found");
    }
}
