//! Strongly-typed domain types to prevent mixing up IDs and other values.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Macro to define a strongly-typed ID newtype.
///
/// Each ID type wraps a String (UUIDv7) and provides:
/// - `new()` to create from any string-like value
/// - `generate()` to create a new UUIDv7
/// - `as_str()` to get the inner string reference
/// - Display, AsRef<str>, and derives for serialization/database
macro_rules! define_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, sqlx::Type,
        )]
        #[serde(transparent)]
        #[sqlx(transparent)]
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

define_id!(UserId);
define_id!(ResearchUrlId);
define_id!(PageId);
define_id!(MediaId);

/// Status of a research URL in the processing pipeline.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, sqlx::Type,
)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ResearchUrlStatus {
    /// Submitted, waiting to be fetched
    Pending,
    /// Fetched successfully, analysis in progress
    Analyzing,
    /// All automated processing complete
    Complete,
    /// Processing failed (see error_message)
    Failed,
}

impl ResearchUrlStatus {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Analyzing => "analyzing",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for ResearchUrlStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Source type for a page (where it was scraped from).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, sqlx::Type,
)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    Instagram,
    Reddit,
    Twitter,
    Flickr,
    /// Library of Congress
    Loc,
    Generic,
}

impl SourceType {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Instagram => "instagram",
            Self::Reddit => "reddit",
            Self::Twitter => "twitter",
            Self::Flickr => "flickr",
            Self::Loc => "loc",
            Self::Generic => "generic",
        }
    }
}

impl fmt::Display for SourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Type of media content.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, sqlx::Type,
)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    Image,
    Video,
}

impl MediaType {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
        }
    }
}

impl fmt::Display for MediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// An email address.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
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

#[cfg(test)]
mod tests {
    use super::*;

    // ID type tests (using UserId as representative)
    #[test]
    fn user_id_new_as_str_roundtrip() {
        let id = UserId::new("test-123");
        assert_eq!(id.as_str(), "test-123");
    }

    #[test]
    fn user_id_generate_is_valid_uuid() {
        let id = UserId::generate();
        // UUIDv7 format: 8-4-4-4-12 hex chars with dashes = 36 chars
        assert_eq!(id.as_str().len(), 36);
        assert!(uuid::Uuid::parse_str(id.as_str()).is_ok());
    }

    #[test]
    fn user_id_display() {
        let id = UserId::new("abc-123");
        assert_eq!(format!("{id}"), "abc-123");
    }

    #[test]
    fn user_id_as_ref() {
        let id = UserId::new("ref-test");
        let s: &str = id.as_ref();
        assert_eq!(s, "ref-test");
    }

    // Enum serialization tests
    #[test]
    fn research_url_status_json_roundtrip() -> Result<(), serde_json::Error> {
        let status = ResearchUrlStatus::Analyzing;
        let json = serde_json::to_string(&status)?;
        assert_eq!(json, "\"analyzing\"");
        let back: ResearchUrlStatus = serde_json::from_str(&json)?;
        assert_eq!(back, status);
        Ok(())
    }

    #[test]
    fn research_url_status_as_str_matches_serde() -> Result<(), serde_json::Error> {
        for status in [
            ResearchUrlStatus::Pending,
            ResearchUrlStatus::Analyzing,
            ResearchUrlStatus::Complete,
            ResearchUrlStatus::Failed,
        ] {
            let json = serde_json::to_string(&status)?;
            assert_eq!(json, format!("\"{}\"", status.as_str()));
        }
        Ok(())
    }

    #[test]
    fn source_type_json_roundtrip() -> Result<(), serde_json::Error> {
        let source = SourceType::Loc;
        let json = serde_json::to_string(&source)?;
        assert_eq!(json, "\"loc\"");
        let back: SourceType = serde_json::from_str(&json)?;
        assert_eq!(back, source);
        Ok(())
    }

    #[test]
    fn media_type_json_roundtrip() -> Result<(), serde_json::Error> {
        let mt = MediaType::Video;
        let json = serde_json::to_string(&mt)?;
        assert_eq!(json, "\"video\"");
        let back: MediaType = serde_json::from_str(&json)?;
        assert_eq!(back, mt);
        Ok(())
    }

    #[test]
    fn email_new_as_str_roundtrip() {
        let email = Email::new("test@example.com");
        assert_eq!(email.as_str(), "test@example.com");
    }

    #[test]
    fn email_display() {
        let email = Email::new("display@example.com");
        assert_eq!(format!("{email}"), "display@example.com");
    }
}
