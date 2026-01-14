//! Database models.

use chrono::NaiveDateTime;
use sqlx::FromRow;

use crate::types::{
    Email, MediaId, MediaType, PageId, ResearchUrlId, ResearchUrlStatus, SourceType, UserId,
};

// ==================== Data Types ====================

/// GPS location with latitude, longitude, and optional altitude.
/// Designed to match future PostGIS/Spatialite POINT type.
#[derive(Debug, Clone)]
pub struct GpsLocation {
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f64>,
}

impl GpsLocation {
    /// Reconstruct GPS location from separate lat/lon/alt columns.
    /// Returns None if lat/lon are missing or only partially present.
    #[must_use]
    pub fn from_columns(lat: Option<f64>, lon: Option<f64>, alt: Option<f64>) -> Option<Self> {
        match (lat, lon) {
            (Some(latitude), Some(longitude)) => Some(GpsLocation {
                latitude,
                longitude,
                altitude: alt,
            }),
            _ => None,
        }
    }

    /// Decompose an optional GPS location into separate column values for database storage.
    #[must_use]
    pub fn to_columns(location: Option<&Self>) -> (Option<f64>, Option<f64>, Option<f64>) {
        match location {
            Some(loc) => (Some(loc.latitude), Some(loc.longitude), loc.altitude),
            None => (None, None, None),
        }
    }
}

/// A user account
#[derive(Debug, Clone, FromRow)]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub email: Email,
    pub created_at: NaiveDateTime,
}

/// A research URL (canonical, deduplicated)
/// Note: page_id and media_id are mutually exclusive (enforced by DB constraint)
#[derive(Debug, Clone, FromRow)]
pub struct ResearchUrl {
    pub id: ResearchUrlId,
    pub url: String,
    pub page_id: Option<PageId>,
    pub media_id: Option<MediaId>,
    pub status: ResearchUrlStatus,
    pub attempt_count: i32,
    pub created_at: NaiveDateTime,
}

/// A research URL that a user follows (includes follow timestamp)
#[derive(Debug, Clone, FromRow)]
pub struct FollowedUrl {
    #[sqlx(flatten)]
    pub research_url: ResearchUrl,
    pub followed_at: NaiveDateTime,
}

/// A media slot - a URL reference that may or may not be resolved to Media yet
#[derive(Debug, Clone)]
pub struct MediaSlot {
    pub url: String,
    pub resolved: Option<Media>,
}

impl MediaSlot {
    /// Create a slot for a URL that hasn't been fetched yet
    pub fn pending(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            resolved: None,
        }
    }
}

/// Core page data (used for both creation and reading)
#[derive(Debug, Clone)]
pub struct PageData {
    pub source_type: SourceType,
    pub title: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<NaiveDateTime>,
    pub content: Option<String>,
    pub fetched_at: NaiveDateTime,
    /// Media referenced by this page (in source order)
    pub media: Vec<MediaSlot>,
}

/// A page with database-generated fields
#[derive(Debug, Clone)]
pub struct Page {
    pub id: PageId,
    pub data: PageData,
    pub created_at: NaiveDateTime,
}

/// Internal row type for reading pages from DB (no media, fetched separately)
#[derive(Debug, FromRow)]
pub(crate) struct PageDbRow {
    pub(crate) id: PageId,
    pub(crate) source_type: SourceType,
    pub(crate) title: Option<String>,
    pub(crate) author: Option<String>,
    pub(crate) published_at: Option<NaiveDateTime>,
    pub(crate) content: Option<String>,
    pub(crate) fetched_at: NaiveDateTime,
    pub(crate) created_at: NaiveDateTime,
}

/// Core media data (used for both creation and reading)
#[derive(Debug, Clone)]
pub struct MediaData {
    pub exact_hash: Vec<u8>,
    pub perceptual_hash: Option<Vec<u8>>,
    pub storage_key: String,
    pub media_type: MediaType,
    pub width: i32,
    pub height: i32,
    pub duration_seconds: Option<f32>,
    pub captured_at: Option<NaiveDateTime>,
    pub location: Option<GpsLocation>,
    pub source_metadata: Option<String>, // JSON stored as text
    pub fetched_at: NaiveDateTime,
}

/// A media item with database-generated fields
#[derive(Debug, Clone)]
pub struct Media {
    pub id: MediaId,
    pub data: MediaData,
    pub created_at: NaiveDateTime,
}

/// Internal row type for sqlx (maps to flat DB columns)
#[derive(Debug, FromRow)]
pub(crate) struct MediaDbRow {
    pub(crate) id: MediaId,
    pub(crate) exact_hash: Vec<u8>,
    pub(crate) perceptual_hash: Option<Vec<u8>>,
    pub(crate) storage_key: String,
    pub(crate) media_type: MediaType,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) duration_seconds: Option<f32>,
    pub(crate) captured_at: Option<NaiveDateTime>,
    pub(crate) gps_latitude: Option<f64>,
    pub(crate) gps_longitude: Option<f64>,
    pub(crate) gps_altitude: Option<f64>,
    pub(crate) source_metadata: Option<String>,
    pub(crate) fetched_at: NaiveDateTime,
    pub(crate) created_at: NaiveDateTime,
}

impl MediaDbRow {
    pub(crate) fn into_media(self) -> Media {
        Media {
            id: self.id,
            data: MediaData {
                exact_hash: self.exact_hash,
                perceptual_hash: self.perceptual_hash,
                storage_key: self.storage_key,
                media_type: self.media_type,
                width: self.width,
                height: self.height,
                duration_seconds: self.duration_seconds,
                captured_at: self.captured_at,
                location: GpsLocation::from_columns(
                    self.gps_latitude,
                    self.gps_longitude,
                    self.gps_altitude,
                ),
                source_metadata: self.source_metadata,
                fetched_at: self.fetched_at,
            },
            created_at: self.created_at,
        }
    }
}

/// Resolved content - either a page with embedded media, or direct media
#[derive(Debug, Clone)]
pub enum ResolvedContent {
    Page(Page),
    Media(Media),
}

/// Full dossier data for a research URL
#[derive(Debug, Clone)]
pub struct ResearchUrlWithResolved {
    pub research_url: ResearchUrl,
    pub resolved: Option<ResolvedContent>,
}

/// Raw row from the page media query (internal use only)
#[derive(Debug, FromRow)]
pub(crate) struct PageMediaRow {
    pub(crate) source_url: String,
    // Media fields (all optional since LEFT JOIN)
    pub(crate) id: Option<MediaId>,
    pub(crate) exact_hash: Option<Vec<u8>>,
    pub(crate) perceptual_hash: Option<Vec<u8>>,
    pub(crate) storage_key: Option<String>,
    pub(crate) media_type: Option<MediaType>,
    pub(crate) width: Option<i32>,
    pub(crate) height: Option<i32>,
    pub(crate) duration_seconds: Option<f32>,
    pub(crate) captured_at: Option<NaiveDateTime>,
    pub(crate) gps_latitude: Option<f64>,
    pub(crate) gps_longitude: Option<f64>,
    pub(crate) gps_altitude: Option<f64>,
    pub(crate) source_metadata: Option<String>,
    pub(crate) fetched_at: Option<NaiveDateTime>,
    pub(crate) created_at: Option<NaiveDateTime>,
}

impl PageMediaRow {
    pub(crate) fn into_media_slot(self) -> MediaSlot {
        let resolved = match (
            self.id,
            self.exact_hash,
            self.storage_key,
            self.media_type,
            self.width,
            self.height,
            self.fetched_at,
            self.created_at,
        ) {
            (
                Some(id),
                Some(exact_hash),
                Some(storage_key),
                Some(media_type),
                Some(width),
                Some(height),
                Some(fetched_at),
                Some(created_at),
            ) => Some(Media {
                id,
                data: MediaData {
                    exact_hash,
                    perceptual_hash: self.perceptual_hash,
                    storage_key,
                    media_type,
                    width,
                    height,
                    duration_seconds: self.duration_seconds,
                    captured_at: self.captured_at,
                    location: GpsLocation::from_columns(
                        self.gps_latitude,
                        self.gps_longitude,
                        self.gps_altitude,
                    ),
                    source_metadata: self.source_metadata,
                    fetched_at,
                },
                created_at,
            }),
            _ => None,
        };

        MediaSlot {
            url: self.source_url,
            resolved,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // GpsLocation::from_columns tests
    #[test]
    fn gps_from_columns_both_present() {
        let loc = GpsLocation::from_columns(Some(41.5), Some(-87.5), None);
        assert!(loc.is_some());
        // Safe because we just asserted is_some
        if let Some(loc) = loc {
            assert!((loc.latitude - 41.5).abs() < f64::EPSILON);
            assert!((loc.longitude - -87.5).abs() < f64::EPSILON);
            assert!(loc.altitude.is_none());
        }
    }

    #[test]
    fn gps_from_columns_with_altitude() {
        let loc = GpsLocation::from_columns(Some(41.5), Some(-87.5), Some(200.0));
        assert!(loc.is_some());
        if let Some(loc) = loc {
            assert_eq!(loc.altitude, Some(200.0));
        }
    }

    #[test]
    fn gps_from_columns_lat_only_returns_none() {
        assert!(GpsLocation::from_columns(Some(41.5), None, None).is_none());
    }

    #[test]
    fn gps_from_columns_lon_only_returns_none() {
        assert!(GpsLocation::from_columns(None, Some(-87.5), None).is_none());
    }

    #[test]
    fn gps_from_columns_neither_returns_none() {
        assert!(GpsLocation::from_columns(None, None, None).is_none());
    }

    // GpsLocation::to_columns tests
    #[test]
    fn gps_to_columns_none() {
        assert_eq!(GpsLocation::to_columns(None), (None, None, None));
    }

    #[test]
    fn gps_to_columns_roundtrip() {
        let loc = GpsLocation {
            latitude: 41.5,
            longitude: -87.5,
            altitude: Some(200.0),
        };
        let (lat, lon, alt) = GpsLocation::to_columns(Some(&loc));
        let reconstructed = GpsLocation::from_columns(lat, lon, alt);
        assert!(reconstructed.is_some());
        if let Some(reconstructed) = reconstructed {
            assert!((reconstructed.latitude - loc.latitude).abs() < f64::EPSILON);
            assert!((reconstructed.longitude - loc.longitude).abs() < f64::EPSILON);
            assert_eq!(reconstructed.altitude, loc.altitude);
        }
    }

    #[test]
    fn gps_to_columns_without_altitude() {
        let loc = GpsLocation {
            latitude: 41.5,
            longitude: -87.5,
            altitude: None,
        };
        let (lat, lon, alt) = GpsLocation::to_columns(Some(&loc));
        assert_eq!(lat, Some(41.5));
        assert_eq!(lon, Some(-87.5));
        assert_eq!(alt, None);
    }

    // MediaSlot::pending test
    #[test]
    fn media_slot_pending_has_no_resolved() {
        let slot = MediaSlot::pending("https://example.com/image.jpg");
        assert_eq!(slot.url, "https://example.com/image.jpg");
        assert!(slot.resolved.is_none());
    }
}
