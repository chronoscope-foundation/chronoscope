//! Strongly-typed domain types to prevent mixing up IDs and other values.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A user ID (UUIDv7 string).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct UserId(String);

impl UserId {
    /// Create a new UserId from a string.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Generate a new random UserId using UUIDv7.
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }

    /// Get the inner string reference.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for UserId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A research URL ID (UUIDv7 string).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct ResearchUrlId(String);

impl ResearchUrlId {
    /// Create a new ResearchUrlId from a string.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Generate a new random ResearchUrlId using UUIDv7.
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }

    /// Get the inner string reference.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ResearchUrlId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for ResearchUrlId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// An email address.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct Email(String);

impl Email {
    /// Create a new Email from a string.
    #[must_use]
    pub fn new(email: impl Into<String>) -> Self {
        Self(email.into())
    }

    /// Get the inner string reference.
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
