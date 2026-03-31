//! Chronoscope API Client
//!
//! Shared types and typed HTTP client for the Chronoscope API.
//! This crate defines the API contract — both the server and web frontend
//! depend on it. WASM-safe (no sqlx, no tokio, no dropshot).

pub mod client;
pub mod entities;
pub mod ids;
pub mod pagination;
pub mod types;

pub use client::{ApiError, ChronoscopeClient, EntityFetchResult};
pub use entities::{AnnotationSummary, EntityLinkSummary, EntityResponse, EntitySummary};
pub use ids::{
    AnnotationId, Email, EntityId, EntityLinkId, MediaId, ResearchUrlId, SourceId, UserId,
};
pub use pagination::{PageToken, ResultsPage};
pub use types::{Bbox, MediaType, ResearchUrlStatus};
