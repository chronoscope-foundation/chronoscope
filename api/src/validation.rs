//! Shared validation utilities for request data.

use chronoscope_db::DbError;
use dropshot::HttpError;

// Re-export from db crate for convenience
pub use chronoscope_db::is_unique_violation;

/// Convert a `DbError` to an `HttpError` for use in API handlers.
///
/// This logs the detailed error internally (visible in Dropshot logs) while
/// returning a generic "Internal Server Error" to clients.
pub fn db_err(e: DbError) -> HttpError {
    HttpError::for_internal_error(e.to_string())
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
