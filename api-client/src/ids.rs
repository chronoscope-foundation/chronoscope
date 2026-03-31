//! Strongly-typed ID newtypes for API entities.
//!
//! These are infrastructure identifiers that appear in API responses. They wrap
//! UUIDv7 strings and provide type safety to prevent mixing up different ID types.
//!
//! When the `sqlx` feature is enabled, these types derive `sqlx::Type` so the
//! database layer can use them directly in row structs without manual conversion.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Macro to define a strongly-typed ID newtype.
///
/// Each ID type wraps a String (UUIDv7) and provides:
/// - `new()` to create from any string-like value
/// - `generate()` to create a new UUIDv7
/// - `as_str()` to get the inner string reference
/// - Display, `AsRef<str>`, and derives for serialization
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

define_id!(EntityId, "Persistent entity identifier (UUIDv7).");
define_id!(
    SourceId,
    "Persistent source identifier (UUIDv7). References images, maps, and documents."
);
define_id!(EntityLinkId, "Entity external link identifier (UUIDv7).");
define_id!(AnnotationId, "Annotation identifier (UUIDv7).");
define_id!(UserId, "User account identifier (UUIDv7).");
define_id!(ResearchUrlId, "Research URL identifier (UUIDv7).");
define_id!(MediaId, "Fetched media blob identifier (UUIDv7).");

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
