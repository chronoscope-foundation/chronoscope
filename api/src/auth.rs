use std::sync::Arc;

use base64::prelude::*;
use chronoscope_db::{Email, UserId};
use dropshot::{
    ClientErrorStatusCode, HttpError, HttpResponseOk, RequestContext, TypedBody, endpoint,
};
use jwt_compact::UntrustedToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use slog::warn;
use webauthn_rs::prelude::*;

use crate::jwt::ChallengePurpose;
use crate::state::AppState;
use crate::validation::{db_err, is_unique_violation, validate_email, validate_username};
use crate::webauthn_types::{
    CredentialCreationOptions, CredentialRequestOptions, PublicKeyCredentialAssertion,
    PublicKeyCredentialAttestation,
};

// ==================== Request/Response Types ====================

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RegisterStartRequest {
    pub username: String,
    pub email: Email,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RegisterStartResponse {
    pub challenge_token: String,
    pub options: CredentialCreationOptions,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RegisterFinishRequest {
    pub challenge_token: String,
    pub credential: PublicKeyCredentialAttestation,
    /// Username for the new account (may differ from `register_start` if there was a conflict)
    pub username: String,
    /// Email for the new account (may differ from `register_start` if there was a conflict)
    pub email: Email,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LoginStartRequest {
    /// Username or email
    pub identifier: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LoginStartResponse {
    pub challenge_token: String,
    pub options: CredentialRequestOptions,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LoginFinishRequest {
    pub challenge_token: String,
    pub credential: PublicKeyCredentialAssertion,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AuthTokenResponse {
    pub token: String,
}

/// Internal state stored in registration challenge token.
/// Only contains the webauthn state - username/email come from the finish request.
#[derive(Debug, Serialize, Deserialize)]
struct RegistrationState {
    webauthn: String,
}

// ==================== Helper Functions ====================

/// Extract Bearer token from Authorization header
fn extract_bearer_token(request: &RequestContext<Arc<AppState>>) -> Option<String> {
    request
        .request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(ToString::to_string)
}

/// Validate session token and return user ID
///
/// # Errors
/// Returns `HttpError` if the Authorization header is missing or the token is invalid.
pub fn validate_session(ctx: &RequestContext<Arc<AppState>>) -> Result<UserId, HttpError> {
    let token_str = extract_bearer_token(ctx).ok_or_else(|| {
        HttpError::for_client_error(
            None,
            ClientErrorStatusCode::UNAUTHORIZED,
            "Missing or invalid Authorization header".to_string(),
        )
    })?;

    let token = UntrustedToken::new(&token_str).map_err(|_| {
        HttpError::for_client_error(
            None,
            ClientErrorStatusCode::UNAUTHORIZED,
            "Invalid token format".to_string(),
        )
    })?;

    let user_id = ctx
        .context()
        .jwt
        .validate_session_token(&token)
        .map_err(|_| {
            HttpError::for_client_error(
                None,
                ClientErrorStatusCode::UNAUTHORIZED,
                "Invalid or expired session token".to_string(),
            )
        })?;

    Ok(user_id)
}

// ==================== Endpoints ====================

/// Start passkey registration flow
#[endpoint {
    method = POST,
    path = "/auth/register/start",
}]
pub async fn register_start(
    ctx: RequestContext<Arc<AppState>>,
    body: TypedBody<RegisterStartRequest>,
) -> Result<HttpResponseOk<RegisterStartResponse>, HttpError> {
    let state = ctx.context();
    let req = body.into_inner();

    // Validate username and email format
    validate_username(&req.username)?;
    validate_email(req.email.as_str())?;

    // Early check for username/email availability (non-authoritative, just for fast feedback).
    // The real uniqueness check happens at INSERT time in register_finish.
    if state
        .db
        .find_user_by_identifier(&req.username)
        .await
        .map_err(db_err)?
        .is_some()
    {
        return Err(HttpError::for_bad_request(
            None,
            "Username already taken".to_string(),
        ));
    }
    if state
        .db
        .find_user_by_identifier(req.email.as_str())
        .await
        .map_err(db_err)?
        .is_some()
    {
        return Err(HttpError::for_bad_request(
            None,
            "Email already registered".to_string(),
        ));
    }

    // Generate a new user ID for this registration
    let user_id = UserId::generate();
    // WebAuthn needs a UUID for the user handle
    let user_uuid = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, user_id.to_string().as_bytes());

    // name = immutable user_id (machine-readable identifier for the passkey)
    // display_name = username (human-friendly, shown in authenticator UI)
    let (ccr, reg_state) = state
        .webauthn
        .start_passkey_registration(user_uuid, user_id.as_ref(), &req.username, None)
        .map_err(|e| HttpError::for_internal_error(format!("Failed to start registration: {e}")))?;

    // Serialize webauthn state (username/email come from register_finish request)
    let webauthn_state_json = serde_json::to_string(&reg_state)
        .map_err(|e| HttpError::for_internal_error(format!("Failed to serialize state: {e}")))?;

    let registration_state = RegistrationState {
        webauthn: webauthn_state_json,
    };
    let state_json = serde_json::to_string(&registration_state)
        .map_err(|e| HttpError::for_internal_error(format!("Failed to serialize state: {e}")))?;
    let state_b64 = BASE64_STANDARD.encode(state_json);

    let challenge_token =
        state
            .jwt
            .create_challenge_token(&state_b64, &user_id, ChallengePurpose::Register)?;

    // Convert webauthn-rs type to our typed struct via JSON
    let options: CredentialCreationOptions =
        serde_json::from_value(serde_json::to_value(&ccr).map_err(|e| {
            HttpError::for_internal_error(format!("Failed to serialize options: {e}"))
        })?)
        .map_err(|e| HttpError::for_internal_error(format!("Failed to convert options: {e}")))?;

    Ok(HttpResponseOk(RegisterStartResponse {
        challenge_token,
        options,
    }))
}

/// Complete passkey registration
#[endpoint {
    method = POST,
    path = "/auth/register/finish",
}]
pub async fn register_finish(
    ctx: RequestContext<Arc<AppState>>,
    body: TypedBody<RegisterFinishRequest>,
) -> Result<HttpResponseOk<AuthTokenResponse>, HttpError> {
    let state = ctx.context();
    let req = body.into_inner();

    // Validate username and email format (may differ from register_start if user had to pick new ones)
    validate_username(&req.username)?;
    validate_email(req.email.as_str())?;

    // Validate challenge token
    let token = match UntrustedToken::new(&req.challenge_token) {
        Ok(t) => t,
        Err(e) => {
            warn!(ctx.log, "register_invalid_challenge_token";
                "event" => "security",
                "action" => "register_finish",
                "result" => "invalid_token_format",
                "error" => %e,
            );
            return Err(crate::jwt::JwtError::from(e).into());
        }
    };
    let claims = match state
        .jwt
        .validate_challenge_token(&token, &ChallengePurpose::Register)
    {
        Ok(c) => c,
        Err(e) => {
            warn!(ctx.log, "register_challenge_validation_failed";
                "event" => "security",
                "action" => "register_finish",
                "result" => "challenge_validation_failed",
                "error" => %e,
            );
            return Err(e.into());
        }
    };

    // Deserialize registration state (contains only webauthn state)
    let state_json = BASE64_STANDARD
        .decode(&claims.state)
        .map_err(|e| HttpError::for_bad_request(None, format!("Invalid state encoding: {e}")))?;
    let registration_state: RegistrationState = serde_json::from_slice(&state_json)
        .map_err(|e| HttpError::for_bad_request(None, format!("Invalid state format: {e}")))?;

    // Extract webauthn state
    let reg_state: PasskeyRegistration = serde_json::from_str(&registration_state.webauthn)
        .map_err(|e| HttpError::for_bad_request(None, format!("Invalid webauthn state: {e}")))?;

    // Convert our typed struct to webauthn-rs type via JSON
    let credential: RegisterPublicKeyCredential =
        serde_json::from_value(serde_json::to_value(&req.credential).map_err(|e| {
            HttpError::for_bad_request(None, format!("Failed to serialize credential: {e}"))
        })?)
        .map_err(|e| HttpError::for_bad_request(None, format!("Invalid credential format: {e}")))?;

    // Complete registration
    let passkey = match state
        .webauthn
        .finish_passkey_registration(&credential, &reg_state)
    {
        Ok(p) => p,
        Err(e) => {
            warn!(ctx.log, "register_webauthn_verification_failed";
                "event" => "security",
                "action" => "register_finish",
                "result" => "webauthn_failed",
                "user_id" => claims.user_id.as_str(),
                "error" => %e,
            );
            return Err(HttpError::for_bad_request(
                None,
                format!("Registration failed: {e}"),
            ));
        }
    };

    // Create user - handle UNIQUE constraint violations gracefully
    if let Err(e) = state
        .db
        .create_user(&claims.user_id, &req.username, &req.email)
        .await
    {
        if is_unique_violation(&e) {
            // Query to determine which field(s) caused the conflict
            let username_taken = state
                .db
                .find_user_by_identifier(&req.username)
                .await
                .map_err(db_err)?
                .is_some();
            let email_taken = state
                .db
                .find_user_by_identifier(req.email.as_str())
                .await
                .map_err(db_err)?
                .is_some();

            let msg = match (username_taken, email_taken) {
                (true, true) => "Username and email are no longer available",
                (true, false) => "Username is no longer available",
                (false, true) => "Email is no longer available",
                (false, false) => "Username or email is no longer available",
            };

            return Err(HttpError::for_client_error(
                None,
                ClientErrorStatusCode::CONFLICT,
                msg.to_string(),
            ));
        }
        return Err(db_err(e));
    }

    // Store the passkey credential
    let credential_id = BASE64_URL_SAFE_NO_PAD.encode(passkey.cred_id().as_ref());
    let passkey_json = serde_json::to_string(&passkey)
        .map_err(|e| HttpError::for_internal_error(format!("Failed to serialize passkey: {e}")))?;
    state
        .db
        .add_credential(&claims.user_id, &credential_id, &passkey_json)
        .await
        .map_err(db_err)?;

    // Create session token
    let token = state.jwt.create_session_token(&claims.user_id)?;

    Ok(HttpResponseOk(AuthTokenResponse { token }))
}

/// Start passkey login flow
#[endpoint {
    method = POST,
    path = "/auth/login/start",
}]
pub async fn login_start(
    ctx: RequestContext<Arc<AppState>>,
    body: TypedBody<LoginStartRequest>,
) -> Result<HttpResponseOk<LoginStartResponse>, HttpError> {
    let state = ctx.context();
    let req = body.into_inner();

    // Find user by username or email
    // Note: We use a generic error message to prevent user enumeration
    let Some(user_id) = state
        .db
        .find_user_by_identifier(&req.identifier)
        .await
        .map_err(db_err)?
    else {
        warn!(ctx.log, "login_attempt_unknown_user";
            "event" => "security",
            "action" => "login_start",
            "result" => "user_not_found",
        );
        return Err(HttpError::for_bad_request(
            None,
            "Login not available for this account".to_string(),
        ));
    };

    // Get credentials for this user only
    let credential_jsons = state.db.get_credentials(&user_id).await.map_err(db_err)?;
    let credentials: Vec<Passkey> = credential_jsons
        .iter()
        .map(|json| serde_json::from_str(json))
        .collect::<Result<_, _>>()
        .map_err(|e| HttpError::for_internal_error(format!("Invalid stored credential: {e}")))?;

    if credentials.is_empty() {
        // Same error message as above to prevent enumeration
        warn!(ctx.log, "login_attempt_no_credentials";
            "event" => "security",
            "action" => "login_start",
            "result" => "no_credentials",
            "user_id" => user_id.as_str(),
        );
        return Err(HttpError::for_bad_request(
            None,
            "Login not available for this account".to_string(),
        ));
    }

    let (rcr, auth_state) = state
        .webauthn
        .start_passkey_authentication(&credentials)
        .map_err(|e| {
            HttpError::for_internal_error(format!("Failed to start authentication: {e}"))
        })?;

    // Serialize authentication state
    let state_json = serde_json::to_string(&auth_state)
        .map_err(|e| HttpError::for_internal_error(format!("Failed to serialize state: {e}")))?;
    let state_b64 = BASE64_STANDARD.encode(state_json);

    let challenge_token =
        state
            .jwt
            .create_challenge_token(&state_b64, &user_id, ChallengePurpose::Login)?;

    // Convert webauthn-rs type to our typed struct via JSON
    let options: CredentialRequestOptions =
        serde_json::from_value(serde_json::to_value(&rcr).map_err(|e| {
            HttpError::for_internal_error(format!("Failed to serialize options: {e}"))
        })?)
        .map_err(|e| HttpError::for_internal_error(format!("Failed to convert options: {e}")))?;

    Ok(HttpResponseOk(LoginStartResponse {
        challenge_token,
        options,
    }))
}

/// Complete passkey login
#[endpoint {
    method = POST,
    path = "/auth/login/finish",
}]
pub async fn login_finish(
    ctx: RequestContext<Arc<AppState>>,
    body: TypedBody<LoginFinishRequest>,
) -> Result<HttpResponseOk<AuthTokenResponse>, HttpError> {
    let state = ctx.context();
    let req = body.into_inner();

    // Validate challenge token
    let token = match UntrustedToken::new(&req.challenge_token) {
        Ok(t) => t,
        Err(e) => {
            warn!(ctx.log, "login_invalid_challenge_token";
                "event" => "security",
                "action" => "login_finish",
                "result" => "invalid_token_format",
                "error" => %e,
            );
            return Err(crate::jwt::JwtError::from(e).into());
        }
    };
    let claims = match state
        .jwt
        .validate_challenge_token(&token, &ChallengePurpose::Login)
    {
        Ok(c) => c,
        Err(e) => {
            warn!(ctx.log, "login_challenge_validation_failed";
                "event" => "security",
                "action" => "login_finish",
                "result" => "challenge_validation_failed",
                "error" => %e,
            );
            return Err(e.into());
        }
    };

    // Deserialize authentication state
    let state_json = BASE64_STANDARD
        .decode(&claims.state)
        .map_err(|e| HttpError::for_bad_request(None, format!("Invalid state encoding: {e}")))?;
    let auth_state: PasskeyAuthentication = serde_json::from_slice(&state_json)
        .map_err(|e| HttpError::for_bad_request(None, format!("Invalid state format: {e}")))?;

    // Convert our typed struct to webauthn-rs type via JSON
    let credential: webauthn_rs::prelude::PublicKeyCredential =
        serde_json::from_value(serde_json::to_value(&req.credential).map_err(|e| {
            HttpError::for_bad_request(None, format!("Failed to serialize credential: {e}"))
        })?)
        .map_err(|e| HttpError::for_bad_request(None, format!("Invalid credential format: {e}")))?;

    // Complete authentication (verifies the passkey signature)
    let auth_result = match state
        .webauthn
        .finish_passkey_authentication(&credential, &auth_state)
    {
        Ok(r) => r,
        Err(e) => {
            warn!(ctx.log, "login_webauthn_verification_failed";
                "event" => "security",
                "action" => "login_finish",
                "result" => "webauthn_failed",
                "user_id" => claims.user_id.as_str(),
                "error" => %e,
            );
            return Err(HttpError::for_bad_request(
                None,
                format!("Authentication failed: {e}"),
            ));
        }
    };

    // User ID comes from the challenge token (set during login_start)
    let user_id = &claims.user_id;

    // Update credential counter if needed
    if auth_result.needs_update() {
        let credential_jsons = state.db.get_credentials(user_id).await.map_err(db_err)?;
        let credentials: Vec<Passkey> = credential_jsons
            .iter()
            .map(|json| serde_json::from_str(json))
            .collect::<Result<_, _>>()
            .map_err(|e| {
                HttpError::for_internal_error(format!("Invalid stored credential: {e}"))
            })?;

        for passkey in credentials {
            if passkey.cred_id() == auth_result.cred_id() {
                let mut updated = passkey.clone();
                updated.update_credential(&auth_result);

                let credential_id = BASE64_URL_SAFE_NO_PAD.encode(updated.cred_id().as_ref());
                let passkey_json = serde_json::to_string(&updated).map_err(|e| {
                    HttpError::for_internal_error(format!("Failed to serialize passkey: {e}"))
                })?;
                state
                    .db
                    .update_credential(user_id, &credential_id, &passkey_json)
                    .await
                    .map_err(db_err)?;
                break;
            }
        }
    }

    // Create session token
    let token = state.jwt.create_session_token(user_id)?;

    Ok(HttpResponseOk(AuthTokenResponse { token }))
}
