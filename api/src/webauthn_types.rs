//! WebAuthn Level 3 wire-format types for JSON serialization.
//!
//! These types mirror the W3C WebAuthn specification exactly:
//! <https://www.w3.org/TR/webauthn-3/>
//!
//! They exist here only because the upstream Rust crates don't yet support
//! `schemars::JsonSchema`, which we need for OpenAPI schema generation with Dropshot.
//!
//! Upstream alternatives (switch to these once they support JsonSchema):
//! - `passkey-types`: <https://crates.io/crates/passkey-types>
//! - `webauthn-rs-proto`: <https://crates.io/crates/webauthn-rs-proto>

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ==================== Enums ====================

/// WebAuthn credential type - currently only "public-key" is defined.
/// See: <https://www.w3.org/TR/webauthn-3/#enumdef-publickeycredentialtype>
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialType {
    PublicKey,
}

/// COSE algorithm identifier for public key credentials.
/// See: <https://www.iana.org/assignments/cose/cose.xhtml#algorithms>
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(into = "i64", try_from = "i64")]
pub enum CoseAlgorithm {
    /// ECDSA with SHA-256 (recommended for passkeys)
    Es256 = -7,
    /// ECDSA with SHA-384
    Es384 = -35,
    /// ECDSA with SHA-512
    Es512 = -36,
    /// RSASSA-PKCS1-v1_5 with SHA-256
    Rs256 = -257,
    /// RSASSA-PKCS1-v1_5 with SHA-384
    Rs384 = -258,
    /// RSASSA-PKCS1-v1_5 with SHA-512
    Rs512 = -259,
    /// EdDSA
    EdDsa = -8,
}

impl JsonSchema for CoseAlgorithm {
    fn schema_name() -> String {
        "CoseAlgorithm".to_string()
    }

    fn json_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "COSE algorithm identifier (integer). Common values: -7 (ES256), -257 (RS256)"
                        .to_string(),
                ),
                ..Default::default()
            })),
            instance_type: Some(schemars::schema::InstanceType::Integer.into()),
            ..Default::default()
        }
        .into()
    }
}

impl From<CoseAlgorithm> for i64 {
    fn from(alg: CoseAlgorithm) -> Self {
        alg as i64
    }
}

impl TryFrom<i64> for CoseAlgorithm {
    type Error = String;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match value {
            -7 => Ok(Self::Es256),
            -35 => Ok(Self::Es384),
            -36 => Ok(Self::Es512),
            -257 => Ok(Self::Rs256),
            -258 => Ok(Self::Rs384),
            -259 => Ok(Self::Rs512),
            -8 => Ok(Self::EdDsa),
            _ => Err(format!("Unknown COSE algorithm: {value}")),
        }
    }
}

// ==================== Registration (Credential Creation) ====================

/// Wrapper for credential creation options.
/// See: <https://www.w3.org/TR/webauthn-3/#sctn-credentialcreationoptions-extension>
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CredentialCreationOptions {
    #[serde(rename = "publicKey")]
    pub public_key: PublicKeyCredentialCreationOptions,
}

/// Options for creating a new public key credential.
/// See: <https://www.w3.org/TR/webauthn-3/#dictdef-publickeycredentialcreationoptions>
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PublicKeyCredentialCreationOptions {
    pub rp: RelyingPartyEntity,
    pub user: UserEntity,
    /// Base64URL-encoded challenge
    pub challenge: String,
    #[serde(rename = "pubKeyCredParams")]
    pub pub_key_cred_params: Vec<PubKeyCredParam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(rename = "excludeCredentials", skip_serializing_if = "Option::is_none")]
    pub exclude_credentials: Option<Vec<PublicKeyCredentialDescriptor>>,
    #[serde(
        rename = "authenticatorSelection",
        skip_serializing_if = "Option::is_none"
    )]
    pub authenticator_selection: Option<AuthenticatorSelectionCriteria>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attestation: Option<String>,
}

/// Information about the relying party (server/application).
/// See: <https://www.w3.org/TR/webauthn-3/#dictdef-publickeycredentialrpentity>
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RelyingPartyEntity {
    /// RP ID (typically the domain, e.g., "example.com")
    pub id: String,
    /// Human-readable RP name
    pub name: String,
}

/// Information about the user account.
/// See: <https://www.w3.org/TR/webauthn-3/#dictdef-publickeycredentialuserentity>
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UserEntity {
    /// Base64URL-encoded user handle (immutable identifier)
    pub id: String,
    /// Machine-readable username (used as credential identifier)
    pub name: String,
    /// Human-readable display name (shown in authenticator UI)
    #[serde(rename = "displayName")]
    pub display_name: String,
}

/// Describes a supported public key algorithm.
/// See: <https://www.w3.org/TR/webauthn-3/#dictdef-publickeycredentialparameters>
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PubKeyCredParam {
    #[serde(rename = "type")]
    pub cred_type: CredentialType,
    pub alg: CoseAlgorithm,
}

/// Describes an existing credential to exclude or allow.
/// See: <https://www.w3.org/TR/webauthn-3/#dictdef-publickeycredentialdescriptor>
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PublicKeyCredentialDescriptor {
    #[serde(rename = "type")]
    pub cred_type: CredentialType,
    /// Base64URL-encoded credential ID
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transports: Option<Vec<String>>,
}

/// Criteria for selecting an authenticator.
/// See: <https://www.w3.org/TR/webauthn-3/#dictdef-authenticatorselectioncriteria>
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AuthenticatorSelectionCriteria {
    #[serde(
        rename = "authenticatorAttachment",
        skip_serializing_if = "Option::is_none"
    )]
    pub authenticator_attachment: Option<String>,
    #[serde(rename = "residentKey", skip_serializing_if = "Option::is_none")]
    pub resident_key: Option<String>,
    #[serde(rename = "requireResidentKey", skip_serializing_if = "Option::is_none")]
    pub require_resident_key: Option<bool>,
    #[serde(rename = "userVerification", skip_serializing_if = "Option::is_none")]
    pub user_verification: Option<String>,
}

// ==================== Authentication (Credential Request) ====================

/// Wrapper for credential request options.
/// See: <https://www.w3.org/TR/webauthn-3/#sctn-credentialrequestoptions-extension>
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CredentialRequestOptions {
    #[serde(rename = "publicKey")]
    pub public_key: PublicKeyCredentialRequestOptions,
}

/// Options for requesting an assertion from an existing credential.
/// See: <https://www.w3.org/TR/webauthn-3/#dictdef-publickeycredentialrequestoptions>
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PublicKeyCredentialRequestOptions {
    /// Base64URL-encoded challenge
    pub challenge: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(rename = "rpId", skip_serializing_if = "Option::is_none")]
    pub rp_id: Option<String>,
    #[serde(rename = "allowCredentials", skip_serializing_if = "Option::is_none")]
    pub allow_credentials: Option<Vec<PublicKeyCredentialDescriptor>>,
    #[serde(rename = "userVerification", skip_serializing_if = "Option::is_none")]
    pub user_verification: Option<String>,
}

// ==================== Credential Responses ====================

/// Attestation response from credential creation (registration).
/// See: <https://www.w3.org/TR/webauthn-3/#iface-authenticatorattestationresponse>
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PublicKeyCredentialAttestation {
    /// Base64URL-encoded credential ID
    pub id: String,
    /// Base64URL-encoded raw credential ID
    #[serde(rename = "rawId")]
    pub raw_id: String,
    #[serde(rename = "type")]
    pub cred_type: CredentialType,
    pub response: AuthenticatorAttestationResponse,
}

/// The attestation response data.
/// See: <https://www.w3.org/TR/webauthn-3/#iface-authenticatorattestationresponse>
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AuthenticatorAttestationResponse {
    /// Base64URL-encoded client data JSON
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    /// Base64URL-encoded attestation object
    #[serde(rename = "attestationObject")]
    pub attestation_object: String,
}

/// Assertion response from credential authentication (login).
/// See: <https://www.w3.org/TR/webauthn-3/#iface-authenticatorassertionresponse>
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PublicKeyCredentialAssertion {
    /// Base64URL-encoded credential ID
    pub id: String,
    /// Base64URL-encoded raw credential ID
    #[serde(rename = "rawId")]
    pub raw_id: String,
    #[serde(rename = "type")]
    pub cred_type: CredentialType,
    pub response: AuthenticatorAssertionResponse,
}

/// The assertion response data.
/// See: <https://www.w3.org/TR/webauthn-3/#iface-authenticatorassertionresponse>
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AuthenticatorAssertionResponse {
    /// Base64URL-encoded client data JSON
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    /// Base64URL-encoded authenticator data
    #[serde(rename = "authenticatorData")]
    pub authenticator_data: String,
    /// Base64URL-encoded signature
    pub signature: String,
    /// Base64URL-encoded user handle
    #[serde(rename = "userHandle", skip_serializing_if = "Option::is_none")]
    pub user_handle: Option<String>,
}
