//! Server-side limits for API responses.

/// Maximum page size for entity listing endpoint.
pub const ENTITY_LIST_MAX_PAGE_SIZE: u32 = 200;

/// Maximum page size for the entity images sub-resource, counting distinct
/// depicted images. A client request over this is clamped down rather than
/// rejected.
pub const ENTITY_IMAGES_MAX_PAGE_SIZE: u32 = 60;
