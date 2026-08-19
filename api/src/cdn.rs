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

use chronoscope_integrations::DisplayableKey;
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
/// The size a browser sees at the edge; [`LocalCdn`] serves the same bytes for
/// every rendition, since dev has no resizer.
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

/// Builds the browser-facing URL for a mirrored image at a given size.
///
/// One implementation per environment, constructed by the entry point and held
/// on [`AppState`](crate::state::AppState): [`EdgeCdn`] points browsers at the
/// Cloudflare edge, which resizes per rendition against the R2 object;
/// [`LocalCdn`] points them at the dev server's own `/media` route, which serves
/// the stored bytes unresized. The read path calls [`url`](Cdn::url) the same
/// way for both — the size only shapes the URL where an edge is there to honor
/// it, so in dev every rendition of an image resolves to one URL.
///
/// URL construction is total, infallible, and does no I/O: a URL can be produced
/// for an image nobody has fetched, which is what lets the read path emit one
/// without a lookup. The one thing that can go wrong is the base, so each
/// implementation settles it once at construction.
pub trait Cdn: std::fmt::Debug + Send + Sync {
    /// The public URL for a displayable image at a given size.
    ///
    /// Takes a [`DisplayableKey`] rather than a bare
    /// [`MirrorKey`](chronoscope_integrations::MirrorKey) so the format check
    /// cannot be skipped: a PDF at a sized rendition builds a URL the edge
    /// rejects, and an SVG served untransformed puts a scriptable document
    /// inside the WebAuthn RP scope.
    fn url(&self, key: &DisplayableKey, rendition: Rendition) -> Url;
}

/// A base URL that can carry a path, so a key can be appended to it.
///
/// # Errors
/// [`CdnError::NotABaseUrl`] for a URL whose path cannot be extended — rejected
/// here rather than at each use, because by then the only choices are a panic or
/// serving every image at one broken URL.
fn checked_base(base: Url) -> Result<Url, CdnError> {
    if base.cannot_be_a_base() {
        return Err(CdnError::NotABaseUrl { url: base });
    }
    Ok(base)
}

/// Production: the Cloudflare edge resizes at request time, so each rendition is
/// its own `/cdn-cgi/image/<opts>/<key>` transform URL against the R2 object.
#[derive(Debug, Clone)]
pub struct EdgeCdn {
    base: Url,
}

impl EdgeCdn {
    /// Build over the CDN base (e.g. `https://cdn.chronoscope.io`).
    ///
    /// # Errors
    /// [`CdnError::NotABaseUrl`] for a base whose path cannot be extended.
    pub fn new(base: Url) -> Result<Self, CdnError> {
        Ok(Self {
            base: checked_base(base)?,
        })
    }
}

impl Cdn for EdgeCdn {
    fn url(&self, key: &DisplayableKey, rendition: Rendition) -> Url {
        let mut url = self.base.clone();
        // `Ok` for every `EdgeCdn` that exists: `path_segments_mut` refuses only
        // a cannot-be-a-base URL, and `new` rejects those.
        if let Ok(mut segments) = url.path_segments_mut() {
            segments
                .pop_if_empty()
                .extend(["cdn-cgi", "image", rendition.cloudflare_options()])
                .extend(key.key().as_str().split('/'));
        }
        url
    }
}

/// The dev media-store key, and `/media/{key}` path tail, for a displayable
/// image: its globally-unique hash under the `media/` prefix.
///
/// A mirror key is `{source}/{hash}`, but the source prefix only organizes keys
/// in R2; dev's media store is flat and the hash alone is unique (a SHA-256 of
/// the image identity), so dev drops the prefix and reuses the research
/// pipeline's single-segment `get_media` route. One derivation so [`LocalCdn`]
/// (which builds the URL) and the dev warm (which stores the bytes) cannot
/// disagree on the key.
#[must_use]
pub fn local_media_key(key: &DisplayableKey) -> String {
    local_media_key_for_mirror_key(key.key().as_str())
}

/// [`local_media_key`] from the raw mirror-key string a queue message carries.
///
/// The dev warm holds `MirrorRequest::key` (the `{source}/{hash}` string), not a
/// [`DisplayableKey`], so it derives the dev store key here rather than
/// re-deriving from the URL. Same `rsplit` as [`local_media_key`], kept single so
/// the URL the read path builds and the key the warm stores under stay in
/// lockstep.
#[must_use]
pub fn local_media_key_for_mirror_key(mirror_key: &str) -> String {
    let hash = mirror_key.rsplit('/').next().unwrap_or_default();
    format!("media/{hash}")
}

/// Dev/test: the `embedded-media` server serves the mirrored bytes unresized
/// from its `/media/{key}` route (via [`local_media_key`]), so the rendition
/// never shapes the URL — there is no edge to resize against.
#[derive(Debug, Clone)]
pub struct LocalCdn {
    base: Url,
}

impl LocalCdn {
    /// Build over the dev base (the same-origin `/api` front-door mount).
    ///
    /// # Errors
    /// [`CdnError::NotABaseUrl`] for a base whose path cannot be extended.
    pub fn new(base: Url) -> Result<Self, CdnError> {
        Ok(Self {
            base: checked_base(base)?,
        })
    }
}

impl Cdn for LocalCdn {
    fn url(&self, key: &DisplayableKey, _rendition: Rendition) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut segments) = url.path_segments_mut() {
            segments
                .pop_if_empty()
                .extend(local_media_key(key).split('/'));
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

    fn commons_displayable_key() -> Result<DisplayableKey, Box<dyn std::error::Error>> {
        DisplayableKey::for_url(&Url::parse(COMMONS_JPEG)?)?
            .ok_or_else(|| "a Commons JPEG is displayable".into())
    }

    #[test]
    fn edge_renditions_go_through_the_transform_path() -> TestResult {
        let cdn = EdgeCdn::new(Url::parse(TEST_CDN_BASE_URL)?)?;
        let key = commons_displayable_key()?;
        assert_eq!(
            cdn.url(&key, Rendition::Marker).as_str(),
            format!(
                "{TEST_CDN_BASE_URL}/cdn-cgi/image/width=320,format=auto,fit=scale-down/{}",
                key.key()
            )
        );
        Ok(())
    }

    #[test]
    fn edge_renditions_have_distinct_urls() -> TestResult {
        let cdn = EdgeCdn::new(Url::parse(TEST_CDN_BASE_URL)?)?;
        let key = commons_displayable_key()?;
        let urls: std::collections::BTreeSet<String> =
            [Rendition::Marker, Rendition::Tile, Rendition::Detail]
                .into_iter()
                .map(|rendition| cdn.url(&key, rendition).to_string())
                .collect();
        // Two sharing a URL would silently serve one at the other's size, and
        // would make the billing story wrong.
        assert_eq!(urls.len(), 3);
        Ok(())
    }

    #[test]
    fn edge_renditions_append_under_a_front_door_api_mount() -> TestResult {
        // The transform path must land *under* a base that carries a path, not
        // replace it.
        let cdn = EdgeCdn::new(Url::parse("http://127.0.0.1:8080/api")?)?;
        let key = commons_displayable_key()?;
        assert_eq!(
            cdn.url(&key, Rendition::Tile).as_str(),
            format!(
                "http://127.0.0.1:8080/api/cdn-cgi/image/width=640,format=auto,fit=scale-down/{}",
                key.key()
            )
        );
        Ok(())
    }

    #[test]
    fn local_serves_one_media_url_for_every_size() -> TestResult {
        // The dev server has no edge to resize against, so every rendition of an
        // image resolves to the one `/media/{hash}` URL `get_media` serves.
        let cdn = LocalCdn::new(Url::parse("http://127.0.0.1:8080/api")?)?;
        let key = commons_displayable_key()?;
        let expected = format!("http://127.0.0.1:8080/api/{}", local_media_key(&key));
        for rendition in [Rendition::Marker, Rendition::Tile, Rendition::Detail] {
            assert_eq!(cdn.url(&key, rendition).as_str(), expected, "{rendition:?}");
        }
        Ok(())
    }

    #[test]
    fn local_media_key_is_the_hash_under_media_without_the_source() -> TestResult {
        // Dev serves single-segment keys through the existing `/media/{key}`
        // route, so the mirror key's source prefix is dropped and the unique hash
        // stands alone under `media/`.
        let key = commons_displayable_key()?;
        let mirror = key.key().as_str().to_string(); // `commons/<hash>`
        let hash = mirror.rsplit('/').next().unwrap_or_default();
        assert_eq!(local_media_key(&key), format!("media/{hash}"));
        assert!(!local_media_key(&key).contains("commons"));
        Ok(())
    }

    #[test]
    fn local_media_key_drops_the_source_prefix_to_a_flat_media_key() -> TestResult {
        // The read path derives the dev key from a `DisplayableKey`; the dev warm
        // derives it from the mirror-key string in the queue message. Both must
        // land on the same flat `media/{hash}` key `get_media` reconstructs, or a
        // warmed image 404s at serve time. Pin the shape to a literal so a change
        // to the prefix or the split fails here, not silently at serve time.
        assert_eq!(
            local_media_key_for_mirror_key("commons/deadbeefhash"),
            "media/deadbeefhash"
        );
        let key = commons_displayable_key()?;
        assert_eq!(
            local_media_key(&key),
            local_media_key_for_mirror_key(key.key().as_str())
        );
        Ok(())
    }

    #[test]
    fn a_base_that_cannot_carry_a_path_is_rejected() -> TestResult {
        // These parse, and appending a key to one does nothing: taking them
        // would leave every image resolving to the same base URL, with no error
        // anywhere to say which images were lost.
        for spelling in ["mailto:ops@chronoscope.io", "data:text/plain,cdn"] {
            let url = Url::parse(spelling)?;
            assert_eq!(
                EdgeCdn::new(url.clone()).err(),
                Some(CdnError::NotABaseUrl { url: url.clone() }),
                "{spelling} is no base for an edge CDN"
            );
            assert_eq!(
                LocalCdn::new(url.clone()).err(),
                Some(CdnError::NotABaseUrl { url }),
                "{spelling} is no base for a local CDN"
            );
        }
        Ok(())
    }

    #[test]
    fn the_key_survives_url_construction_unescaped() -> TestResult {
        // The key's `/` separates path segments rather than being encoded;
        // a percent-encoded slash would not match the stored object.
        let key = commons_displayable_key()?;
        let url = EdgeCdn::new(Url::parse(TEST_CDN_BASE_URL)?)?.url(&key, Rendition::Detail);
        assert!(url.as_str().ends_with(key.key().as_str()));
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
