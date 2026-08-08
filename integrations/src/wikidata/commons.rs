//! Wikimedia Commons integration.
//!
//! URL generation and gallery fetching for Commons media files.

use std::sync::LazyLock;

use md5::{Digest, Md5};
use url::Url;
use urlencoding::encode as urlencode;

use super::entity::{CommonsFilename, PageId, RevisionId};
use super::{ApiTimestamp, WikidataClient, WikidataError};
use crate::http::{HttpClient, HttpRequest};

/// Base URL for Wikimedia Commons file uploads.
///
/// Shared with [`crate::media`], which classifies the URLs this mints: the
/// host that names a Commons file and the host that mints one are the same
/// fact, and two spellings of it would drift into keying Commons images as
/// unclaimed web URLs.
#[allow(clippy::expect_used)]
pub(crate) static COMMONS_UPLOAD_BASE: LazyLock<Url> =
    LazyLock::new(|| Url::parse("https://upload.wikimedia.org").expect("hardcoded URL is valid"));

/// Path segments that locate the Commons repository on the upload host.
///
/// The upload host also serves each project's local uploads under sibling
/// prefixes (`/wikipedia/en/`, …), so this prefix is what says "Commons file".
/// [`url_for_filename`] writes it into every upload URL and [`crate::media`]
/// matches it to recognize one, so a single definition keeps the mint and read
/// sides from drifting.
pub(crate) const COMMONS_REPOSITORY_PREFIX: &[&str] = &["wikipedia", "commons"];

/// Widths of the two shard directories in a Commons upload path.
///
/// MediaWiki shards an upload by the first hex digit of its filename's MD5 and
/// then by the first two, so the file lands at `/{h0}/{h0..2}/{title}`.
/// [`url_for_filename`] slices the hash to these widths; [`crate::media`] checks
/// that the two shard segments have them. One definition keeps the shape agreed.
pub(crate) const COMMONS_SHARD_WIDTHS: [usize; 2] = [1, 2];

/// Canonicalize a Commons title's whitespace.
///
/// MediaWiki treats a space and an underscore as one character in a title and
/// stores the underscore form, so both the mint side ([`url_for_filename`]) and
/// the read side ([`crate::media`]) fold spaces to underscores before anything
/// downstream compares titles. Fuller MediaWiki title normalization (collapsing
/// runs of whitespace, trimming, first-letter capitalization) would extend this
/// one function.
pub(crate) fn canonical_title(title: &str) -> String {
    title.replace(' ', "_")
}

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
    client: &WikidataClient<H>,
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

    let rev_json = client.get_json(rev_url).await?;

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
    client: &WikidataClient<H>,
    rev_id: RevisionId,
) -> Result<Vec<CommonsFilename>, WikidataError> {
    // Use oldid alone — pageid and oldid cannot be combined in action=parse
    let parse_url_str = format!(
        "https://commons.wikimedia.org/w/api.php?\
         action=parse&oldid={}&prop=images&format=json",
        rev_id.0
    );
    let parse_url = Url::parse(&parse_url_str)?;

    let parse_request = HttpRequest::get(parse_url.clone());
    let parse_response = client.execute_checked(parse_request).await?;

    if parse_response.body.len() > MAX_RESPONSE_SIZE {
        return Err(WikidataError::ResponseTooLarge(parse_response.body.len()));
    }

    let json = super::parse_json_body(&parse_url, &parse_response.body)?;
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
    let filename_underscored = canonical_title(filename.as_str());
    let mut hasher = Md5::new();
    hasher.update(filename_underscored.as_bytes());
    let hash = hasher.finalize();
    let hash_hex = format!("{hash:x}");

    let [shard_width, subshard_width] = COMMONS_SHARD_WIDTHS;
    let prefix = COMMONS_REPOSITORY_PREFIX.join("/");
    let mut url = COMMONS_UPLOAD_BASE.clone();
    url.set_path(&format!(
        "/{prefix}/{}/{}/{}",
        &hash_hex[0..shard_width],
        &hash_hex[0..subshard_width],
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
