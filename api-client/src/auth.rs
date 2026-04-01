//! Auth request/response types shared between client and server.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::Email;
use crate::webauthn_types::{
    CredentialCreationOptions, CredentialRequestOptions, PublicKeyCredentialAssertion,
    PublicKeyCredentialAttestation,
};

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

/// Response containing a bearer token.
///
/// `Debug` redacts the token to prevent accidental logging. The token is a plain
/// `String` because serde's JSON parser buffers it in memory during deserialization
/// anyway — wrapping in `SecretString` here would add ceremony without real protection.
/// The token gets wrapped in `SecretString` when constructing `AuthClient`.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct AuthTokenResponse {
    pub token: String,
}

impl std::fmt::Debug for AuthTokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthTokenResponse")
            .field("token", &"[redacted]")
            .finish()
    }
}
