//! Wikimedia Commons integration.
//!
//! URL generation and gallery fetching for Commons media files.

use std::sync::LazyLock;

use md5::{Digest, Md5};
use url::Url;
use urlencoding::encode as urlencode;

use super::ApiTimestamp;
use super::WikidataError;
use super::entity::{CommonsFilename, PageId, RevisionId};
use crate::http::{HttpClient, HttpRequest};

/// Base URL for Wikimedia Commons file uploads.
#[allow(clippy::expect_used)]
static COMMONS_UPLOAD_BASE: LazyLock<Url> =
    LazyLock::new(|| Url::parse("https://upload.wikimedia.org").expect("hardcoded URL is valid"));

/// Maximum response size for API requests (1 MB).
const MAX_RESPONSE_SIZE: usize = 1024 * 1024;

/// Media file extensions accepted from Commons galleries.
const MEDIA_EXTENSIONS: &[&str] = &[
    // Images
    "jpg", "jpeg", "png", "gif", "svg", "tif", "tiff", "webp", "avif", // Video
    "mp4", "webm", "ogv", // Documents
    "pdf",
];

/// Resolve a Commons gallery page's revision at a specific timestamp.
///
/// Returns `Some((page_id, revision_id))` for the gallery page as it existed
/// at or before the given timestamp. Returns `None` if the page does not exist.
///
/// # Errors
/// Returns an error if the API request fails or the response cannot be parsed.
pub(super) async fn resolve_gallery_revision<H: HttpClient>(
    http: &H,
    gallery: &str,
    timestamp: &ApiTimestamp,
) -> Result<Option<(PageId, RevisionId)>, WikidataError> {
    let gallery_title = gallery.replace(' ', "_");
    let encoded_title = urlencode(&gallery_title);

    let rev_url_str = format!(
        "https://commons.wikimedia.org/w/api.php?\
         action=query&titles={encoded_title}&prop=revisions\
         &rvprop=ids&rvstart={}&rvdir=older&rvlimit=1&format=json",
        timestamp.as_str()
    );
    let rev_url = Url::parse(&rev_url_str)?;

    let rev_request = HttpRequest::get(rev_url);
    let rev_response = http.execute(rev_request).await?;
    let rev_json: serde_json::Value = serde_json::from_slice(&rev_response.body)?;

    let pages = rev_json
        .pointer("/query/pages")
        .and_then(|p| p.as_object())
        .ok_or_else(|| WikidataError::Api {
            message: format!("no pages in revision response for gallery '{gallery}'"),
        })?;

    let (page_id_str, page_obj) = pages.iter().next().ok_or_else(|| WikidataError::Api {
        message: format!("empty pages object for gallery '{gallery}'"),
    })?;

    // Missing page has page ID "-1"
    if page_id_str == "-1" {
        return Ok(None);
    }

    let rev_id = page_obj
        .pointer("/revisions/0/revid")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| WikidataError::Api {
            message: format!("no revision found for gallery '{gallery}' at {timestamp}"),
        })?;

    let page_id = page_id_str.parse::<u64>().map_err(|_| WikidataError::Api {
        message: format!("invalid page ID '{page_id_str}' for gallery '{gallery}'"),
    })?;

    Ok(Some((PageId(page_id), RevisionId(rev_id))))
}

/// Fetch media from a Commons gallery page at a specific revision.
///
/// Returns filenames of media files in the gallery (images, video, PDFs),
/// filtered by known media extensions.
///
/// Takes a `revision_id` directly — use [`resolve_gallery_revision`] to obtain
/// this from a gallery title and timestamp.
///
/// # Errors
/// Returns an error if the API request fails or the response cannot be parsed.
pub(super) async fn fetch_gallery_media_at_revision<H: HttpClient>(
    http: &H,
    rev_id: RevisionId,
) -> Result<Vec<CommonsFilename>, WikidataError> {
    // Use oldid alone — pageid and oldid cannot be combined in action=parse
    let parse_url_str = format!(
        "https://commons.wikimedia.org/w/api.php?\
         action=parse&oldid={}&prop=images&format=json",
        rev_id.0
    );
    let parse_url = Url::parse(&parse_url_str)?;

    let parse_request = HttpRequest::get(parse_url);
    let parse_response = http.execute(parse_request).await?;

    if parse_response.body.len() > MAX_RESPONSE_SIZE {
        return Err(WikidataError::ResponseTooLarge(parse_response.body.len()));
    }

    let json: serde_json::Value = serde_json::from_slice(&parse_response.body)?;
    Ok(extract_gallery_media(&json))
}

/// Extract media filenames from a Commons API parse response.
fn extract_gallery_media(json: &serde_json::Value) -> Vec<CommonsFilename> {
    json.get("parse")
        .and_then(|p| p.get("images"))
        .and_then(|i| i.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|img| img.as_str())
                .filter(|s| {
                    let lower = s.to_lowercase();
                    matches!(lower.rsplit('.').next(), Some(ext) if MEDIA_EXTENSIONS.contains(&ext))
                })
                .map(|s| CommonsFilename(s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Generate Wikimedia Commons URL from a [`CommonsFilename`].
///
/// Commons URLs use an MD5 hash of the filename to determine the path.
/// The base URL is hardcoded and the filename is percent-encoded, so
/// this function is infallible.
#[must_use]
pub fn url_for_filename(filename: &CommonsFilename) -> Url {
    let filename_underscored = filename.as_str().replace(' ', "_");
    let mut hasher = Md5::new();
    hasher.update(filename_underscored.as_bytes());
    let hash = hasher.finalize();
    let hash_hex = format!("{hash:x}");

    let mut url = COMMONS_UPLOAD_BASE.clone();
    url.set_path(&format!(
        "/wikipedia/commons/{}/{}/{}",
        &hash_hex[0..1],
        &hash_hex[0..2],
        urlencode(&filename_underscored)
    ));
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_spaces_become_underscores() {
        let url = url_for_filename(&CommonsFilename("My Photo.jpg".to_string()));
        let path = url.path();
        assert!(
            path.contains("My_Photo.jpg"),
            "spaces should become underscores in the path: {path}"
        );
        assert!(!url.as_str().contains(' '));
    }

    #[test]
    fn test_extract_gallery_media_filters_correctly() {
        let json = serde_json::json!({
            "parse": {
                "images": [
                    "Photo.jpg",
                    "Plan.svg",
                    "Video.mp4",
                    "Interior.png",
                    "Doc.pdf",
                    "Night.tiff",
                    "Data.csv"
                ]
            }
        });
        let media = extract_gallery_media(&json);
        let names: Vec<&str> = media.iter().map(|f| f.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Photo.jpg",
                "Plan.svg",
                "Video.mp4",
                "Interior.png",
                "Doc.pdf",
                "Night.tiff"
            ]
        );
    }

    #[test]
    fn test_extract_gallery_media_empty_response() {
        let json = serde_json::json!({});
        let media = extract_gallery_media(&json);
        assert!(media.is_empty());
    }

    #[test]
    fn prop_url_no_spaces() {
        // Spaces in filenames must be converted, never left as raw spaces or %20
        for filename in &["Hello World.jpg", "Two  Spaces.png", " Leading.jpg"] {
            let url = url_for_filename(&CommonsFilename(filename.to_string()));
            assert!(
                !url.as_str().contains(' '),
                "URL should not contain raw spaces: {url}"
            );
            assert!(
                !url.as_str().contains("%20"),
                "URL should not contain %20 (spaces become underscores): {url}"
            );
        }
    }
}
