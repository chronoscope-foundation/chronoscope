//! Pagination types matching Dropshot's wire format.
//!
//! The server uses `dropshot::ResultsPage` for construction/serialization.
//! This module provides a deserialization-compatible type for clients.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Opaque page token for cursor-based pagination.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct PageToken(String);

impl PageToken {
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PageToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A page of results from a paginated API endpoint.
///
/// Matches Dropshot's `ResultsPage` wire format so clients can deserialize
/// responses from the server.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResultsPage<T> {
    pub items: Vec<T>,
    pub next_page: Option<PageToken>,
}
