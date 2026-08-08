//! CDN URL generation for media assets.
//!
//! Two schemes live here during the transition. [`full_url`] and
//! [`thumbnail_url`] serve the research-URL fetcher, which stores media under
//! content-addressed keys with a companion `_thumb.jpg` object. [`Cdn`] serves
//! mirrored fact-store images, whose key is derived from the upstream URL and
//! whose sizes are request-time transforms rather than stored objects.
//!
//! They coexist deliberately: the two pipelines converge later, and until then
//! deleting either would break the surfaces the other feeds.

use chronoscope_integrations::{DisplayableKey, MirrorKey};
use thiserror::Error;
use url::Url;

/// Suffix appended to storage keys for thumbnail variants.
const THUMBNAIL_SUFFIX: &str = "_thumb";

/// Generate a full-resolution CDN URL for a media item.
///
/// Storage keys are multi-segment (`/`-separated), so each segment is appended
/// individually — a single push would percent-encode the slashes.
#[must_use]
pub fn full_url(base: &Url, storage_key: &str) -> Url {
    let mut url = base.clone();
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.pop_if_empty().extend(storage_key.split('/'));
    }
    url
}

/// Generate a thumbnail CDN URL for a media item.
///
/// Thumbnails are always JPEG regardless of original format.
#[must_use]
pub fn thumbnail_url(base: &Url, storage_key: &str) -> Url {
    let thumb_key = match storage_key.rfind('.') {
        Some(dot) => format!("{}{THUMBNAIL_SUFFIX}.jpg", &storage_key[..dot]),
        None => format!("{storage_key}{THUMBNAIL_SUFFIX}.jpg"),
    };
    let mut url = base.clone();
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.pop_if_empty().extend(thumb_key.split('/'));
    }
    url
}

// ==================== Mirrored media ====================

/// A size a browser surface is willing to serve, named by the surface that
/// asks for it.
///
/// Closed on purpose. Every distinct size is another billable transformation
/// per image per calendar month, so an open width parameter would let any
/// caller mint unbounded cost. Naming by surface rather than by pixels also
/// makes adding a fourth a visible decision, and makes the blast radius of
/// changing a size obvious.
///
/// Every variant transforms. The untransformed object is not a rendition:
/// only analysis reads it, and it reaches that through
/// [`Cdn::original_url`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rendition {
    /// Map marker thumbnails, drawn at 96 CSS px and up to 3x device ratio.
    Marker,
    /// Detail-panel grid tiles.
    Tile,
    /// The lightbox. Bounded deliberately: a real Commons original can be
    /// 100 megapixels, and the upstream source URL remains available for
    /// anyone who wants true full resolution.
    Detail,
}

impl Rendition {
    /// The Cloudflare transform options for this rendition.
    ///
    /// One canonical string per rendition, because the options are part of
    /// what makes a transformation unique for billing, so two spellings of one
    /// size would be charged twice.
    ///
    /// These options ride in the rendition URL a browser fetches at serve time.
    /// Warming only ever stores the original and never touches renditions, so
    /// `format=auto` is always a request-time negotiation of WebP or AVIF per
    /// browser, and still counts as a single transformation. `fit=scale-down`
    /// keeps a small original from being upscaled into a large response.
    fn cloudflare_options(self) -> &'static str {
        match self {
            Self::Marker => "width=320,format=auto,fit=scale-down",
            Self::Tile => "width=640,format=auto,fit=scale-down",
            Self::Detail => "width=1600,format=auto,fit=scale-down",
        }
    }
}

/// Why a base URL cannot address media.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CdnError {
    /// A base URL that cannot carry a path, so nothing can be appended to it.
    ///
    /// `mailto:` and `data:` URLs parse, and a key appended to one goes nowhere:
    /// every image would resolve to the base itself.
    #[error("CDN base URL `{url}` cannot carry a path, so no media key can be appended to it")]
    NotABaseUrl {
        /// The URL as offered.
        url: Url,
    },
}

/// Builds public URLs for mirrored media.
///
/// URL construction is total, infallible, and does no I/O: a URL can be
/// produced for an image nobody has fetched, which is what lets the read path
/// emit one without a lookup. Not a trait, because production and local emit
/// the same URL *shape*: the dev server serves the Cloudflare transform path
/// itself rather than a parallel format, so nothing branches on environment.
///
/// The one thing that can go wrong is the base, so it is settled once at
/// construction: a `Cdn` that exists can address a key.
#[derive(Debug, Clone)]
pub struct Cdn {
    base: Url,
}

impl Cdn {
    /// Build over a base URL, which may carry a path prefix: dev and test
    /// serve same-origin through an `/api` front-door mount.
    ///
    /// # Errors
    /// [`CdnError::NotABaseUrl`] for a URL whose path cannot be extended.
    /// Rejected here rather than at each use, because by then the only choices
    /// are a panic or dropping the key and serving every image at one broken URL.
    pub fn new(base: Url) -> Result<Self, CdnError> {
        if base.cannot_be_a_base() {
            return Err(CdnError::NotABaseUrl { url: base });
        }
        Ok(Self { base })
    }

    /// The public URL for a mirrored image at a given size.
    ///
    /// Takes a [`DisplayableKey`] rather than a bare [`MirrorKey`] so the
    /// format check cannot be skipped: a PDF at a sized rendition builds a
    /// URL the edge rejects, and an SVG served untransformed puts a
    /// scriptable document inside the WebAuthn RP scope.
    #[must_use]
    pub fn url(&self, key: &DisplayableKey, rendition: Rendition) -> Url {
        self.at(key.key(), Some(rendition.cloudflare_options()))
    }

    /// The URL of the untransformed mirrored object.
    ///
    /// Not browser-facing. The key can address an SVG, a PDF or a TIFF, and
    /// this URL is same-origin, so pointing a browser at one either hands the
    /// WebAuthn RP scope a scriptable document or serves bytes no browser can
    /// render. Analysis reads the master here, which costs no transformations;
    /// browser surfaces go through [`Cdn::url`].
    #[must_use]
    pub fn original_url(&self, key: &MirrorKey) -> Url {
        self.at(key, None)
    }

    fn at(&self, key: &MirrorKey, options: Option<&str>) -> Url {
        let mut url = self.base.clone();
        // `Ok` for every `Cdn` that exists: `path_segments_mut` refuses only a
        // cannot-be-a-base URL, and `Cdn::new` rejects those.
        if let Ok(mut segments) = url.path_segments_mut() {
            let mut segments = segments.pop_if_empty();
            if let Some(options) = options {
                segments = segments.extend(["cdn-cgi", "image", options]);
            }
            segments.extend(key.as_str().split('/'));
        }
        url
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub const TEST_CDN_BASE_URL: &str = "https://cdn.test.chronoscope.io";

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn test_thumbnail_url_with_extension() -> TestResult {
        let base = Url::parse(TEST_CDN_BASE_URL)?;
        assert_eq!(
            thumbnail_url(&base, "abc123/image.jpg").as_str(),
            format!("{TEST_CDN_BASE_URL}/abc123/image_thumb.jpg")
        );
        Ok(())
    }

    #[test]
    fn test_thumbnail_url_png_becomes_jpg() -> TestResult {
        let base = Url::parse(TEST_CDN_BASE_URL)?;
        assert_eq!(
            thumbnail_url(&base, "abc123/image.png").as_str(),
            format!("{TEST_CDN_BASE_URL}/abc123/image_thumb.jpg")
        );
        Ok(())
    }

    #[test]
    fn test_thumbnail_url_without_extension() -> TestResult {
        let base = Url::parse(TEST_CDN_BASE_URL)?;
        assert_eq!(
            thumbnail_url(&base, "abc123/image").as_str(),
            format!("{TEST_CDN_BASE_URL}/abc123/image_thumb.jpg")
        );
        Ok(())
    }

    #[test]
    fn test_thumbnail_url_multiple_dots() -> TestResult {
        let base = Url::parse(TEST_CDN_BASE_URL)?;
        assert_eq!(
            thumbnail_url(&base, "2024/01/photo.2024.01.15.png").as_str(),
            format!("{TEST_CDN_BASE_URL}/2024/01/photo.2024.01.15_thumb.jpg")
        );
        Ok(())
    }

    // ==================== Mirrored media ====================

    const COMMONS_JPEG: &str = "https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg";

    fn commons_key() -> Result<MirrorKey, Box<dyn std::error::Error>> {
        Ok(MirrorKey::for_url(&Url::parse(COMMONS_JPEG)?)?)
    }

    fn commons_displayable_key() -> Result<DisplayableKey, Box<dyn std::error::Error>> {
        DisplayableKey::for_url(&Url::parse(COMMONS_JPEG)?)?
            .ok_or_else(|| "a Commons JPEG is displayable".into())
    }

    #[test]
    fn the_original_serves_the_bare_key() -> TestResult {
        let cdn = Cdn::new(Url::parse(TEST_CDN_BASE_URL)?)?;
        let key = commons_key()?;
        assert_eq!(
            cdn.original_url(&key).as_str(),
            format!("{TEST_CDN_BASE_URL}/{key}")
        );
        Ok(())
    }

    #[test]
    fn renditions_go_through_the_transform_path() -> TestResult {
        let cdn = Cdn::new(Url::parse(TEST_CDN_BASE_URL)?)?;
        let key = commons_displayable_key()?;
        assert_eq!(
            cdn.url(&key, Rendition::Marker).as_str(),
            format!("{TEST_CDN_BASE_URL}/cdn-cgi/image/width=320,format=auto,fit=scale-down/{key}")
        );
        Ok(())
    }

    #[test]
    fn each_rendition_and_the_original_have_distinct_urls() -> TestResult {
        let cdn = Cdn::new(Url::parse(TEST_CDN_BASE_URL)?)?;
        let key = commons_displayable_key()?;
        let mut urls: std::collections::BTreeSet<String> =
            [Rendition::Marker, Rendition::Tile, Rendition::Detail]
                .into_iter()
                .map(|rendition| cdn.url(&key, rendition).to_string())
                .collect();
        urls.insert(cdn.original_url(key.key()).to_string());
        // Two of these sharing a URL would silently serve one at the other's
        // size, and would also make the billing story wrong.
        assert_eq!(urls.len(), 4);
        Ok(())
    }

    #[test]
    fn renditions_append_under_a_front_door_api_mount() -> TestResult {
        // Dev and test serve same-origin through an `/api` proxy, so the
        // transform path must land *under* the mount, not replace it.
        let cdn = Cdn::new(Url::parse("http://127.0.0.1:8080/api")?)?;
        let key = commons_displayable_key()?;
        assert_eq!(
            cdn.url(&key, Rendition::Tile).as_str(),
            format!(
                "http://127.0.0.1:8080/api/cdn-cgi/image/width=640,format=auto,fit=scale-down/{key}"
            )
        );
        assert_eq!(
            cdn.original_url(key.key()).as_str(),
            format!("http://127.0.0.1:8080/api/{key}")
        );
        Ok(())
    }

    #[test]
    fn a_base_that_cannot_carry_a_path_is_rejected() -> TestResult {
        // These parse, and appending a key to one does nothing: taking them
        // would leave every image resolving to the same base URL, with no
        // error anywhere to say which images were lost.
        for spelling in ["mailto:ops@chronoscope.io", "data:text/plain,cdn"] {
            let url = Url::parse(spelling)?;
            let error = Cdn::new(url.clone())
                .err()
                .ok_or_else(|| format!("{spelling} is no base for a CDN"))?;
            assert_eq!(error, CdnError::NotABaseUrl { url });
        }
        Ok(())
    }

    #[test]
    fn the_key_survives_url_construction_unescaped() -> TestResult {
        // The key's `/` separates path segments rather than being encoded;
        // a percent-encoded slash would not match the stored object.
        let cdn = Cdn::new(Url::parse(TEST_CDN_BASE_URL)?)?;
        let key = commons_key()?;
        let url = cdn.original_url(&key);
        assert!(url.as_str().ends_with(key.as_str()));
        assert!(!url.as_str().contains("%2F"));
        Ok(())
    }

    #[test]
    fn test_full_url_appends_under_a_front_door_api_mount() -> TestResult {
        // Dev and test serve thumbnails same-origin through a `/api` front-door
        // proxy, so the base carries an `/api` path segment. The storage key must
        // land *under* it (`/api/media/...`), not replace the segment.
        let base = Url::parse("http://127.0.0.1:8080/api")?;
        assert_eq!(
            full_url(&base, "media/abc123.jpg").as_str(),
            "http://127.0.0.1:8080/api/media/abc123.jpg"
        );
        Ok(())
    }
}
