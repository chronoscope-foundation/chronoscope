//! Strongly-typed ID newtypes for the API contract.
//!
//! Two families:
//!
//! - Infrastructure ids ([`UserId`], [`ResearchUrlId`], [`MediaId`], [`Email`])
//!   wrap UUIDv7 strings the database mints. Under the `sqlx` feature they derive
//!   `sqlx::Type` so row structs use them directly.
//! - Opaque wire ids ([`EntityId`], [`ImageId`], [`EventId`]) are backend-agnostic
//!   identifiers the client reads out of fact-store read responses. They carry no
//!   format assumption — whatever string the backend serialized round-trips
//!   verbatim — so the client never names a backend's concrete id type.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Define a UUIDv7-backed infrastructure id newtype wrapping a `String`:
/// - `new()` from any string-like value
/// - `generate()` for a fresh UUIDv7
/// - `as_str()`, `Display`, `AsRef<str>`, and the serialization derives
/// - `sqlx::Type` under the `sqlx` feature
macro_rules! define_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
        )]
        #[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
        #[serde(transparent)]
        #[cfg_attr(feature = "sqlx", sqlx(transparent))]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            #[must_use]
            pub fn generate() -> Self {
                Self(uuid::Uuid::now_v7().to_string())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

define_id!(UserId, "User account identifier (UUIDv7).");
define_id!(ResearchUrlId, "Research URL identifier (UUIDv7).");
define_id!(MediaId, "Fetched media blob identifier (UUIDv7).");

/// Define an opaque, backend-agnostic wire id newtype wrapping a `String`.
///
/// Unlike [`define_id!`] these carry no format assumption and no `generate()` —
/// the value is whatever string the backend serialized, threaded back verbatim.
/// The `#[serde(transparent)]` wire form is the bare string, so the client reads
/// a backend's id without naming the backend's concrete id type.
macro_rules! wire_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

wire_id!(EntityId, "Opaque wire identifier for an entity.");
wire_id!(ImageId, "Opaque wire identifier for an image.");
wire_id!(EventId, "Opaque wire identifier for a lifetime event.");

/// An email address.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[serde(transparent)]
#[cfg_attr(feature = "sqlx", sqlx(transparent))]
pub struct Email(String);

impl Email {
    #[must_use]
    pub fn new(email: impl Into<String>) -> Self {
        Self(email.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Email {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for Email {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
