//! Chronoscope API Client
//!
//! Shared types and typed HTTP client for the Chronoscope API.
//! This crate defines the API contract — both the server and web frontend
//! depend on it. WASM-safe (no sqlx, no tokio, no dropshot).

pub mod auth;
pub mod client;
pub mod entities;
pub mod ids;
pub mod pagination;
pub mod types;
pub mod users;
pub mod webauthn_types;

pub use auth::{
    AuthTokenResponse, LoginFinishRequest, LoginStartRequest, LoginStartResponse,
    RegisterFinishRequest, RegisterStartRequest, RegisterStartResponse,
};
pub use client::{ApiError, AuthClient, AuthError, Client, login, paginate, register};
pub use entities::{
    ClickAction, Cursor, DetailImage, Entity, EntityDetail, EntityListPage, EntityPickerEntry,
    EntitySummary, Marker, MarkersResponse, image_caption,
};
pub use ids::{
    AnnotationId, Email, EntityId, EntityLinkId, MediaId, ResearchUrlId, SourceId, UserId,
};
pub use pagination::{PageToken, ResultsPage};
pub use types::{MediaType, ResearchUrlStatus, ZoneType};
pub use users::{UpdateUserRequest, UserResponse};
