//! Conversion functions from database models to API response types.
//!
//! The response types themselves live in `chronoscope_api_client::entities`.
//! This module provides the server-side logic for constructing them from
//! database rows (orphan rule prevents `impl From<DbType> for ClientType`).

pub use chronoscope_api_client::{
    AnnotationSummary, EntityLinkSummary, EntityResponse, EntitySummary, MediaSummary,
    ThumbnailInfo,
};
use chronoscope_db::{Annotation, EntityId, EntityLink, EntityMedia, EntityThumbnail};

use crate::cdn;

/// Convert a database `EntityLink` to an API `EntityLinkSummary`.
pub fn entity_link_summary(link: EntityLink) -> EntityLinkSummary {
    EntityLinkSummary {
        id: link.id,
        link_type: link.link_type,
        target: link.target,
    }
}

/// Convert a database `Annotation` to an API `AnnotationSummary`.
pub fn annotation_summary(ann: Annotation) -> AnnotationSummary {
    AnnotationSummary {
        id: ann.id,
        kind: ann.kind,
        created_at: ann.created_at,
    }
}

/// Convert a database `EntityMedia` to an API `MediaSummary`.
pub fn media_summary(media: EntityMedia, cdn_base_url: &str) -> MediaSummary {
    MediaSummary {
        id: media.id,
        url: cdn::full_url(cdn_base_url, &media.storage_key),
        source_url: media.source_url,
        width: media.width,
        height: media.height,
        captured: media.captured,
        annotation_kind: media.annotation_kind,
    }
}

/// Convert a database `EntityThumbnail` to an `(EntityId, ThumbnailInfo)` pair.
pub fn thumbnail_entry(thumb: EntityThumbnail, cdn_base_url: &str) -> (EntityId, ThumbnailInfo) {
    let info = ThumbnailInfo {
        url: cdn::thumbnail_url(cdn_base_url, &thumb.storage_key),
        width: thumb.width,
        height: thumb.height,
    };
    (thumb.entity_id, info)
}
