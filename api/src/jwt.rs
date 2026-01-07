use dropshot::HttpError;
use jwt_compact::{
    AlgorithmExt, Claims, Header, TimeOptions, Token, UntrustedToken, alg::Hs256, alg::Hs256Key,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::UserId;

#[derive(Error, Debug)]
pub enum JwtError {
    #[error("Failed to create token: {0}")]
    Creation(#[from] jwt_compact::CreationError),

    #[error("Failed to parse token: {0}")]
    Parse(#[from] jwt_compact::ParseError),

    #[error("Invalid token: {0}")]
    Validation(#[from] jwt_compact::ValidationError),

    #[error("Invalid challenge purpose")]
    InvalidPurpose,

    #[error("Token missing required claims (exp or iat)")]
    MissingClaims,

    #[error("JWT secret not configured - set JWT_SECRET environment variable")]
    SecretNotConfigured,

    #[error("JWT secret too short - must be at least 32 bytes")]
    SecretTooShort,
}

/// Minimum secret length in bytes for HMAC-SHA256.
/// 32 bytes (256 bits) provides adequate security margin.
pub const MIN_SECRET_LENGTH: usize = 32;

/// JWT configuration for session and challenge tokens.
pub struct JwtConfig {
    /// HMAC-SHA256 key used to sign and verify all JWTs.
    key: Hs256Key,
    /// How long session tokens remain valid (default: 7 days for mobile).
    session_expiry_seconds: i64,
    /// How long `WebAuthn` challenge tokens remain valid (default: 2 minutes).
    challenge_expiry_seconds: i64,
    /// Clock skew tolerance when validating token expiration (default: 60 seconds).
    leeway_seconds: i64,
}

impl JwtConfig {
    /// Create a new JWT configuration with explicit values.
    ///
    /// # Panics
    ///
    /// Panics if the secret is shorter than `MIN_SECRET_LENGTH` bytes.
    /// For non-panicking construction, use `try_new`.
    #[must_use]
    pub fn new(
        secret: &str,
        session_expiry_seconds: i64,
        challenge_expiry_seconds: i64,
        leeway_seconds: i64,
    ) -> Self {
        Self::try_new(
            secret,
            session_expiry_seconds,
            challenge_expiry_seconds,
            leeway_seconds,
        )
        .expect("JWT secret must be at least 32 bytes")
    }

    /// Create a new JWT configuration with explicit values, returning an error
    /// if the secret is too short.
    ///
    /// # Errors
    ///
    /// Returns `JwtError::SecretTooShort` if the secret is shorter than
    /// `MIN_SECRET_LENGTH` bytes.
    pub fn try_new(
        secret: &str,
        session_expiry_seconds: i64,
        challenge_expiry_seconds: i64,
        leeway_seconds: i64,
    ) -> Result<Self, JwtError> {
        if secret.len() < MIN_SECRET_LENGTH {
            return Err(JwtError::SecretTooShort);
        }
        Ok(Self {
            key: Hs256Key::new(secret.as_bytes()),
            session_expiry_seconds,
            challenge_expiry_seconds,
            leeway_seconds,
        })
    }

    /// Load JWT configuration from environment variables
    ///
    /// Required: `JWT_SECRET` (must be at least 32 bytes)
    /// Optional: `JWT_SESSION_EXPIRY_SECONDS` (default: 7 days for mobile)
    /// Optional: `JWT_CHALLENGE_EXPIRY_SECONDS` (default: 2 minutes)
    /// Optional: `JWT_LEEWAY_SECONDS` (default: 60 seconds for clock skew)
    ///
    /// # Errors
    ///
    /// Returns `JwtError::SecretNotConfigured` if `JWT_SECRET` is not set,
    /// or `JwtError::SecretTooShort` if it's shorter than 32 bytes.
    pub fn from_env() -> Result<Self, JwtError> {
        let secret = std::env::var("JWT_SECRET").map_err(|_| JwtError::SecretNotConfigured)?;

        let session_expiry_seconds = std::env::var("JWT_SESSION_EXPIRY_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(7 * 24 * 60 * 60); // 7 days default (better for mobile)

        let challenge_expiry_seconds = std::env::var("JWT_CHALLENGE_EXPIRY_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2 * 60); // 2 minutes default

        let leeway_seconds: i64 = std::env::var("JWT_LEEWAY_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60); // 60 seconds default for clock skew

        Self::try_new(
            &secret,
            session_expiry_seconds,
            challenge_expiry_seconds,
            leeway_seconds,
        )
    }

    /// Create a session token for the given user.
    ///
    /// # Errors
    /// Returns `JwtError::Creation` if token creation fails.
    pub fn create_session_token(&self, user_id: &UserId) -> Result<String, JwtError> {
        let time_options = TimeOptions::default();
        let claims = Claims::new(SessionClaims {
            sub: user_id.to_string(),
        })
        .set_duration_and_issuance(
            &time_options,
            chrono::Duration::seconds(self.session_expiry_seconds),
        );

        Hs256
            .token(&Header::empty(), &claims, &self.key)
            .map_err(JwtError::from)
    }

    /// Validate a session token and return the user ID.
    ///
    /// # Errors
    /// Returns `JwtError::Validation` if the signature is invalid or the token is expired.
    pub fn validate_session_token(&self, token: &UntrustedToken<'_>) -> Result<UserId, JwtError> {
        let time_options = TimeOptions::from_leeway(chrono::Duration::seconds(self.leeway_seconds));
        let token: Token<SessionClaims> = Hs256.validator(&self.key).validate(token)?;

        token.claims().validate_expiration(&time_options)?;

        let claims = token.claims();
        // Validate that required standard claims are present
        claims.expiration.ok_or(JwtError::MissingClaims)?;
        claims.issued_at.ok_or(JwtError::MissingClaims)?;

        Ok(UserId::new(claims.custom.sub.clone()))
    }

    /// Create a challenge token for WebAuthn registration or login.
    ///
    /// The `state` parameter holds the serialized WebAuthn registration/authentication
    /// state that will be needed when the client completes the ceremony.
    ///
    /// # Errors
    /// Returns `JwtError::Creation` if token creation fails.
    pub fn create_challenge_token(
        &self,
        state: &str,
        user_id: &UserId,
        purpose: ChallengePurpose,
    ) -> Result<String, JwtError> {
        let time_options = TimeOptions::default();
        let claims = Claims::new(ChallengeClaims {
            state: state.to_string(),
            user_id: user_id.clone(),
            purpose,
        })
        .set_duration_and_issuance(
            &time_options,
            chrono::Duration::seconds(self.challenge_expiry_seconds),
        );

        Hs256
            .token(&Header::empty(), &claims, &self.key)
            .map_err(JwtError::from)
    }

    /// Validate a challenge token and return the claims.
    ///
    /// Challenge tokens are used for both registration and login flows.
    ///
    /// # Errors
    /// Returns `JwtError::Validation` if the signature is invalid or the token is expired, or
    /// `JwtError::InvalidPurpose` if the token purpose doesn't match the expected purpose.
    pub fn validate_challenge_token(
        &self,
        token: &UntrustedToken<'_>,
        expected_purpose: &ChallengePurpose,
    ) -> Result<ChallengeClaims, JwtError> {
        let time_options = TimeOptions::from_leeway(chrono::Duration::seconds(self.leeway_seconds));
        let token: Token<ChallengeClaims> = Hs256.validator(&self.key).validate(token)?;

        token.claims().validate_expiration(&time_options)?;

        let claims = token.claims();
        if &claims.custom.purpose != expected_purpose {
            return Err(JwtError::InvalidPurpose);
        }

        // Validate that required standard claims are present
        claims.expiration.ok_or(JwtError::MissingClaims)?;
        claims.issued_at.ok_or(JwtError::MissingClaims)?;

        Ok(claims.custom.clone())
    }
}

/// Session token claims (just the user ID, using standard JWT "sub" field).
#[derive(Debug, Serialize, Deserialize)]
struct SessionClaims {
    /// Subject - the user ID this token was issued for
    sub: String,
}

/// Challenge token claims for WebAuthn registration or login.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeClaims {
    /// Opaque state blob (base64-encoded registration/auth state)
    pub state: String,
    /// The user ID this challenge is for
    pub user_id: UserId,
    /// Whether this is a registration or login challenge
    pub purpose: ChallengePurpose,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ChallengePurpose {
    Register,
    Login,
}

impl From<JwtError> for HttpError {
    fn from(e: JwtError) -> Self {
        match e {
            // Server-side errors (misconfiguration or creation failure)
            JwtError::Creation(_) | JwtError::SecretNotConfigured | JwtError::SecretTooShort => {
                HttpError::for_internal_error(e.to_string())
            }
            // Client-side errors (bad/expired/wrong token)
            JwtError::Parse(_)
            | JwtError::Validation(_)
            | JwtError::InvalidPurpose
            | JwtError::MissingClaims => HttpError::for_bad_request(None, e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &str = "this-is-a-test-secret-with-enough-length";

    #[test]
    fn test_jwt_error_to_http_error_server_errors() {
        // Server-side errors should be internal errors (5xx)
        let secret_err = JwtError::SecretNotConfigured;
        let http_err: HttpError = secret_err.into();
        assert!(http_err.status_code.is_server_error());

        let short_err = JwtError::SecretTooShort;
        let http_err: HttpError = short_err.into();
        assert!(http_err.status_code.is_server_error());
    }

    #[test]
    fn test_jwt_error_to_http_error_client_errors() {
        // Client-side errors should be client errors (4xx)
        let purpose_err = JwtError::InvalidPurpose;
        let http_err: HttpError = purpose_err.into();
        assert!(http_err.status_code.is_client_error());

        let claims_err = JwtError::MissingClaims;
        let http_err: HttpError = claims_err.into();
        assert!(http_err.status_code.is_client_error());
    }

    #[test]
    fn test_jwt_config_new() {
        let config = JwtConfig::new(TEST_SECRET, 3600, 120, 60);
        // Should be able to create tokens
        let user_id = crate::types::UserId::new("test-user");
        assert!(config.create_session_token(&user_id).is_ok());
    }

    #[test]
    fn test_session_token_roundtrip() {
        let config = JwtConfig::new(TEST_SECRET, 3600, 120, 60);
        let user_id = crate::types::UserId::new("test-user-123");

        // Create a token
        let token_str = config.create_session_token(&user_id).unwrap();

        // Validate it
        let untrusted = UntrustedToken::new(&token_str).unwrap();
        let validated_user_id = config.validate_session_token(&untrusted).unwrap();

        assert_eq!(validated_user_id.as_str(), "test-user-123");
    }

    #[test]
    fn test_challenge_token_wrong_purpose() {
        let config = JwtConfig::new(TEST_SECRET, 3600, 120, 60);
        let user_id = crate::types::UserId::new("test-user");

        // Create a registration challenge token
        let token_str = config
            .create_challenge_token("state", &user_id, ChallengePurpose::Register)
            .unwrap();

        // Try to validate as login - should fail
        let untrusted = UntrustedToken::new(&token_str).unwrap();
        let result = config.validate_challenge_token(&untrusted, &ChallengePurpose::Login);
        assert!(matches!(result, Err(JwtError::InvalidPurpose)));
    }

    #[test]
    fn test_challenge_purpose_serialization() {
        // Test that purposes serialize/deserialize correctly
        let register = ChallengePurpose::Register;
        let login = ChallengePurpose::Login;

        let reg_json = serde_json::to_string(&register).unwrap();
        let login_json = serde_json::to_string(&login).unwrap();

        assert_eq!(reg_json, "\"register\"");
        assert_eq!(login_json, "\"login\"");

        let reg_back: ChallengePurpose = serde_json::from_str(&reg_json).unwrap();
        let login_back: ChallengePurpose = serde_json::from_str(&login_json).unwrap();

        assert_eq!(reg_back, ChallengePurpose::Register);
        assert_eq!(login_back, ChallengePurpose::Login);
    }

    #[test]
    fn test_secret_too_short_rejected() {
        let short_secret = "too-short"; // 9 bytes, need 32
        let result = JwtConfig::try_new(short_secret, 3600, 120, 60);
        assert!(matches!(result, Err(JwtError::SecretTooShort)));
    }

    #[test]
    fn test_secret_at_minimum_length_accepted() {
        let min_secret = "a".repeat(super::MIN_SECRET_LENGTH); // exactly 32 bytes
        let result = JwtConfig::try_new(&min_secret, 3600, 120, 60);
        assert!(result.is_ok());
    }
}
