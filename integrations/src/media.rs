//! Mirror keys: the address of an upstream image in our own store.
//!
//! Every image in the system comes from somewhere on the internet, and its key
//! is derived from what names it upstream rather than from its bytes.
//! Addressing has to work offline, in a total function, so that a URL can be
//! built without fetching anything; description (dimensions, EXIF, a content
//! digest) inherently cannot, and lives elsewhere.
//!
//! The key is `{source}/{sha256(identity)}`, where `identity` is what a
//! [`SourceId`] considers the stable name of the image.
//!
//! For Commons that is the filename. Wikidata's P18 hands us
//! `Machu Picchu, Peru (2018).jpg` and
//! [`url_for_filename`](crate::wikidata::url_for_filename) derives a URL from
//! it, so the filename is the identifier the source itself uses and the URL is
//! a function of it. Keying on the filename means `%28` and `(` are two
//! spellings of one title rather than two keys needing a canonical direction
//! chosen between them.
//!
//! Commons file URLs are `upload.wikimedia.org/wikipedia/commons/{x}/{xy}/{title}`.
//! The title is read out of that path shape, and a thumbnail
//! (`/thumb/.../{rendition}`) is rejected rather than mirrored. Any other path
//! on that host, such as a project-local upload under `/wikipedia/en/`, is not a
//! Commons file and keys on its URL like any other host.
//!
//! For a host no source claims, the URL is all there is, so identity is the
//! normalized URL with its percent-encoding left verbatim. The collapse
//! Commons enjoys is not available there: `(` is a sub-delim a conforming
//! client transmits literally, so `/a(b)` and `/a%28b%29` are genuinely two
//! request targets and only the origin server knows whether they name one
//! object. MediaWiki titles are the case where we do know.
//!
//! Sources that serve media from sharded hosts with per-request tokens
//! (Instagram's `scontent-*.cdninstagram.com`, where the same photograph comes
//! back on different hosts with different query signatures) will need
//! something narrower still, which is why identity is a per-source decision
//! rather than one global rule.
//!
//! This is a sibling of [`crate::IntegrationMeta::normalize_url`], not the
//! same thing: that canonicalizes *page* URLs for the research queue and is
//! keyed by page domain, while this names *media* and is keyed by the
//! repository the media URL addresses.

use percent_encoding::percent_decode_str;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::wikidata::commons::{
    COMMONS_REPOSITORY_PREFIX, COMMONS_SHARD_WIDTHS, COMMONS_UPLOAD_BASE, canonical_title,
};

/// The upstream a mirrored image came from.
///
/// The `as_str` value is a literal that appears in every key, so the warm
/// path and the read path must agree on it exactly; a disagreement leaves the
/// bucket half-addressable with no error anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceId {
    /// Files in the Wikimedia Commons repository, the corpus source.
    Commons,
    /// Anything no source claims, including the per-project upload
    /// repositories that share the Commons host.
    Web,
}

impl SourceId {
    /// The key prefix for this source.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commons => "commons",
            Self::Web => "web",
        }
    }

    /// Classify a URL by the repository it addresses.
    ///
    /// Commons file URLs are minted by
    /// [`url_for_filename`](crate::wikidata::url_for_filename), which puts them
    /// under `/wikipedia/commons/` on the upload host this shares with it. The
    /// host alone is not enough: the same host serves each project's local
    /// uploads, where a title names a different file, so those key on their
    /// URL like any other unclaimed source.
    #[must_use]
    pub fn classify(url: &Url) -> Self {
        if commons_repository_path(url).is_some() {
            Self::Commons
        } else {
            Self::Web
        }
    }
}

/// Normalize a URL into the string a key is derived from.
///
/// Defined rather than best-effort, because two spellings of one URL would
/// otherwise mirror twice and serve two keys for one image:
///
/// - scheme and host lowercased, and a default port dropped (both already
///   done by the `url` crate, before this sees the URL)
/// - the fragment dropped, since it never reaches the server
/// - an empty query dropped, since `?` alone and no `?` fetch the same bytes
/// - userinfo dropped, since a username or password names the requester rather
///   than the resource
/// - percent-escapes in the *path* uppercased, since RFC 3986 §2.1 makes `%2e`
///   and `%2E` the same octet, so their case carries no identity, and §6.2.2.1
///   picks uppercase
/// - everything else kept verbatim: path characters, their case, query order,
///   and which octets are escaped
///
/// Folding an escape's case is not decoding it: `%28` never becomes `(`,
/// because only the origin server knows whether a reserved octet carries
/// delimiting meaning inside a component. RFC 3986 §6.2.2.2 decodes only
/// *unreserved* octets for exactly that reason, so `/a(b)` and `/a%28b%29` stay
/// two request targets until some source-specific knowledge says otherwise. The
/// query is kept exactly as given, escape case included: a source that signs
/// its query would be broken by re-spelling any part of it.
#[must_use]
fn normalize(url: &Url) -> String {
    let mut normalized = url.clone();
    normalized.set_fragment(None);
    if normalized.query() == Some("") {
        normalized.set_query(None);
    }
    // Userinfo names the requester, not the image. Both setters no-op on a
    // base-less URL, which carries no userinfo to begin with.
    let _ = normalized.set_password(None);
    let _ = normalized.set_username("");
    let recased = uppercase_percent_escapes(normalized.path());
    normalized.set_path(&recased);
    normalized.to_string()
}

/// Uppercase the two hex digits of every percent-escape in `s`.
///
/// RFC 3986 §2.1 makes `%2e` and `%2E` the same octet and §6.2.2.1 picks
/// uppercase, so folding here collapses two spellings of one escape to one key.
/// A `%` that is not followed by two hex digits is left as it stands, and every
/// other character is copied through, so path case (significant for Web
/// identity) survives.
fn uppercase_percent_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut escape = chars.clone();
        match (escape.next(), escape.next()) {
            (Some(hi), Some(lo)) if hi.is_ascii_hexdigit() && lo.is_ascii_hexdigit() => {
                out.push('%');
                out.push(hi.to_ascii_uppercase());
                out.push(lo.to_ascii_uppercase());
                chars = escape;
            }
            _ => out.push('%'),
        }
    }
    out
}

/// The path segments under [`COMMONS_REPOSITORY_PREFIX`], for a URL that
/// addresses something in the repository.
///
/// One place decides what "on Commons" means, so [`SourceId::classify`] and
/// [`Resolved::of`] cannot drift into disagreeing about which URLs the title
/// rule applies to: a URL that classified as Commons but had no title would
/// key on neither rule.
///
/// Consumes the prefix from the segment iterator and collects only the
/// remainder, so a matching URL costs one allocation rather than one for the
/// whole path and another for the tail.
fn commons_repository_path(url: &Url) -> Option<Vec<&str>> {
    if url.host_str() != COMMONS_UPLOAD_BASE.host_str() {
        return None;
    }
    let mut segments = url.path_segments()?;
    for expected in COMMONS_REPOSITORY_PREFIX {
        if segments.next()? != *expected {
            return None;
        }
    }
    Some(segments.collect())
}

/// A shard directory: `width` characters of the title's MD5.
///
/// Shape only. Recomputing the MD5 to check the digits would couple this to
/// the derivation [`url_for_filename`](crate::wikidata::url_for_filename)
/// owns, and buy nothing: the shard says nothing about which file it is that
/// the title does not already say.
fn is_shard(segment: &str, width: usize) -> bool {
    segment.len() == width && segment.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The Commons title named by a repository sub-path (already stripped of
/// [`COMMONS_REPOSITORY_PREFIX`]).
///
/// Read from the path's shape rather than off its last segment, because the
/// two shapes put the title in different places: a master ends in it, and a
/// thumbnail carries it as a *directory* above the rendition
/// (`…/thumb/c/ca/Title.jpg/960px-Title.jpg`). Taking the last segment of a
/// thumbnail yields a title no file has, splitting one photograph across two
/// keys.
///
/// Everything around the title falls away: the shard directories are a
/// function of it, and the scheme and query say how to fetch the file rather
/// than which file it is.
///
/// Takes the already-resolved sub-path so [`Resolved::of`], which parses it
/// once to classify, reads the title without re-walking the host and prefix.
///
/// # Errors
/// [`MirrorKeyError::CommonsThumbnail`] for a rendition, and
/// [`MirrorKeyError::NotACommonsFile`] for a path matching neither shape.
fn commons_title_from_path(path: &[&str], url: &Url) -> Result<String, MirrorKeyError> {
    let [shard_width, subshard_width] = COMMONS_SHARD_WIDTHS;
    match path {
        [shard, subshard, title]
            if is_shard(shard, shard_width) && is_shard(subshard, subshard_width) =>
        {
            title_from_segment(title, url)
        }
        ["thumb", shard, subshard, title, rendition]
            if is_shard(shard, shard_width)
                && is_shard(subshard, subshard_width)
                && !rendition.is_empty() =>
        {
            Err(MirrorKeyError::CommonsThumbnail {
                url: url.clone(),
                title: title_from_segment(title, url)?,
            })
        }
        _ => Err(MirrorKeyError::NotACommonsFile { url: url.clone() }),
    }
}

/// Decode a path segment into the title it spells.
///
/// Space and underscore are one character to MediaWiki, and the URL form
/// always carries the underscore, so that is the spelling identity keeps. A
/// title cannot be empty, and bytes that are no UTF-8 spell no title at all.
fn title_from_segment(segment: &str, url: &Url) -> Result<String, MirrorKeyError> {
    let decoded = percent_decode_str(segment)
        .decode_utf8()
        .map_err(|_| MirrorKeyError::NotACommonsFile { url: url.clone() })?;
    let title = canonical_title(&decoded);
    if title.is_empty() {
        return Err(MirrorKeyError::NotACommonsFile { url: url.clone() });
    }
    Ok(title)
}

/// A URL resolved to the source that claims it and the name that source keys
/// on, with the Commons path parsed at most once.
///
/// [`MirrorKey::for_url`] and [`DisplayableKey::for_url`] both need the
/// classification, and on Commons both need the decoded title; resolving once
/// lets the key and the display decision come from a single parse instead of
/// each re-deriving it.
enum Resolved {
    /// A Commons master, keyed on and displayed by its decoded title.
    Commons { title: String },
    /// A URL no source claims, keyed on its normalized form.
    Web { identity: String },
}

impl Resolved {
    /// Classify a fetchable URL and read the name its source keys on.
    ///
    /// The scheme is checked here because a key names something we intend to
    /// fetch, so both entry points reject a non-http(s) URL before classifying.
    ///
    /// # Errors
    /// [`MirrorKeyError::UnsupportedScheme`] for a non-http(s) scheme, and
    /// [`MirrorKeyError::CommonsThumbnail`] or
    /// [`MirrorKeyError::NotACommonsFile`] for a Commons-repository URL that
    /// names no master file.
    fn of(url: &Url) -> Result<Self, MirrorKeyError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(MirrorKeyError::UnsupportedScheme { url: url.clone() });
        }
        match commons_repository_path(url) {
            Some(path) => Ok(Self::Commons {
                title: commons_title_from_path(&path, url)?,
            }),
            None => Ok(Self::Web {
                identity: normalize(url),
            }),
        }
    }

    /// The store key for this resolution.
    fn key(&self) -> MirrorKey {
        let (source, identity) = match self {
            Self::Commons { title } => (SourceId::Commons, title.as_str()),
            Self::Web { identity } => (SourceId::Web, identity.as_str()),
        };
        let digest = Sha256::digest(identity.as_bytes());
        MirrorKey(format!("{}/{}", source.as_str(), hex::encode(digest)))
    }

    /// Whether a browser-renderable rendition can be derived, from the name
    /// already resolved: the decoded title on Commons, the raw last path
    /// segment elsewhere.
    fn is_displayable(&self, url: &Url) -> bool {
        match self {
            Self::Commons { title } => renderable_extension(title),
            Self::Web { .. } => web_url_displays(url),
        }
    }
}

/// Why a URL has no mirror key.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MirrorKeyError {
    /// A scheme the mirror cannot fetch from: `file:`, `data:`, anything that
    /// is not http(s).
    #[error("cannot mirror `{url}`: scheme `{}` is neither http nor https", .url.scheme())]
    UnsupportedScheme {
        /// The URL as offered.
        url: Url,
    },
    /// A Commons rendition rather than the file it was derived from.
    ///
    /// We mirror masters: a rendition is smaller than the file it came from,
    /// so keying one would both split the photograph across two keys and let a
    /// large rendition be built by upscaling a small one. A fact citing a
    /// thumbnail is a data problem, and this is what reports it.
    #[error(
        "cannot mirror `{url}`: it is a rendition of the Commons file `{title}`; mirror that file's master URL instead"
    )]
    CommonsThumbnail {
        /// The URL as offered.
        url: Url,
        /// The title the rendition was derived from, which is what to mirror.
        title: String,
    },
    /// A URL in the Commons repository whose path is neither a master nor a
    /// thumbnail: a truncated path, a trailing slash, a shard that is no hex,
    /// or a segment that does not decode to a title.
    ///
    /// An error rather than a fall back to URL identity, because a fallback
    /// would let two code paths key one object two ways with nothing
    /// reporting it.
    #[error(
        "cannot mirror `{url}`: path `{}` is no Commons file (expected `/wikipedia/commons/{{x}}/{{xy}}/{{title}}`)",
        .url.path()
    )]
    NotACommonsFile {
        /// The URL as offered.
        url: Url,
    },
}

/// Where an upstream image lives in our store.
///
/// Computable from a URL alone, with no I/O, so a caller can build a CDN URL
/// for an image it has never fetched. Two URLs its source considers one image
/// land on one key, which is what keeps the store from mirroring the same
/// image twice and serving it at two addresses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MirrorKey(String);

impl MirrorKey {
    /// Derive the key for an upstream URL.
    ///
    /// A key is the address of something we intend to fetch, and only http(s)
    /// is fetchable, so the scheme is checked during resolution rather than at
    /// each use: every `MirrorKey` in the system then names a reachable
    /// upstream.
    ///
    /// # Errors
    /// [`MirrorKeyError::UnsupportedScheme`] for any other scheme, and
    /// [`MirrorKeyError::CommonsThumbnail`] or
    /// [`MirrorKeyError::NotACommonsFile`] for a URL in the Commons repository
    /// that names no master file.
    pub fn for_url(url: &Url) -> Result<Self, MirrorKeyError> {
        Ok(Resolved::of(url)?.key())
    }

    /// The key as it appears in the store and in a CDN path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MirrorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A [`MirrorKey`] whose upstream a browser can render.
///
/// Obtainable only through [`DisplayableKey::for_url`], which applies
/// [`displayable`]. Browser-facing URL construction asks for this type, so the
/// check happens once at derivation instead of at each call site that could
/// forget it: an SVG key served untransformed would put `image/svg+xml` inside
/// the WebAuthn RP scope, and a PDF key at a sized rendition would build a URL
/// the edge rejects.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisplayableKey(MirrorKey);

impl DisplayableKey {
    /// Derive the browser-facing key for an upstream URL.
    ///
    /// `Ok(None)` when the URL mirrors fine but nothing in a browser can show
    /// it: a TIFF or a PDF still belongs in the store for analysis, it just
    /// has no renderable URL until something produces a proxy for it.
    ///
    /// # Errors
    /// Whatever [`MirrorKey::for_url`] rejects the URL for.
    pub fn for_url(url: &Url) -> Result<Option<Self>, MirrorKeyError> {
        let resolved = Resolved::of(url)?;
        Ok(resolved.is_displayable(url).then(|| Self(resolved.key())))
    }

    /// The underlying key, which addresses the same stored object.
    #[must_use]
    pub fn key(&self) -> &MirrorKey {
        &self.0
    }
}

impl std::fmt::Display for DisplayableKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The formats a browser can render, each as the extensions that name it and
/// the content type its bytes carry. `displayable()` gates on the extensions;
/// the mirror consumer re-checks the fetched bytes against the content type. One
/// source for both encodings so they cannot drift: a format added here widens
/// the display gate and the consumer's accept list in one edit.
///
/// The set is the intersection of two constraints, which is why it is an
/// allowlist rather than a list of exclusions. Cloudflare accepts PNG, JPEG,
/// GIF, WebP, SVG, HEIC, and AVIF as transform input, with AVIF restricted to
/// Enterprise plans. The `image` crate is built here with `jpeg`, `png`, `gif`,
/// and `webp` only, so anything else would transform in production and fail in
/// the dev route.
///
/// SVG is excluded on both counts: transforms deliver it unresized, so a
/// thumbnail URL would serve a full-size SVG, and a direct original URL
/// bypasses Cloudflare's sanitization entirely, putting `image/svg+xml` inside
/// the WebAuthn RP scope.
const DISPLAYABLE_FORMATS: &[(&[&str], &str)] = &[
    (&["jpg", "jpeg"], "image/jpeg"),
    (&["png"], "image/png"),
    (&["gif"], "image/gif"),
    (&["webp"], "image/webp"),
];

/// The content types the displayable formats carry, for the mirror consumer's
/// fetch-time re-check of the bytes. Drawn from the same `DISPLAYABLE_FORMATS`
/// the extension gate reads, so an accept list a dispatcher stamps cannot omit a
/// format the gate admits.
pub fn displayable_content_types() -> impl Iterator<Item = &'static str> {
    DISPLAYABLE_FORMATS
        .iter()
        .map(|(_, content_type)| *content_type)
}

/// Whether a browser-renderable rendition can be derived from this URL.
///
/// Gates *display*, not mirroring: everything gets mirrored, because analysis
/// wants the master whatever a browser can do with it. The complement is the
/// set that will need a locally-produced renderable proxy, and since it is a
/// pure function of the URL there is nothing to track.
///
/// The rule lives once, in `Resolved::is_displayable`. This is its keyless
/// entry point, for the warm path that decides what to fetch before there is a
/// key to talk about; [`DisplayableKey`] reaches the same rule through a key,
/// so the two paths cannot disagree about whether an image renders. A URL that
/// cannot be mirrored at all (a bad scheme, a Commons thumbnail, a non-file)
/// resolves to nothing, and so does not display.
#[must_use]
pub fn displayable(url: &Url) -> bool {
    Resolved::of(url)
        .map(|resolved| resolved.is_displayable(url))
        .unwrap_or(false)
}

/// Whether an unclaimed URL's last path segment names a renderable file.
///
/// Reads the raw segment rather than a decoded title: off Commons two spellings
/// are two keys, so decoding here would let one key render through the other's
/// name.
fn web_url_displays(url: &Url) -> bool {
    url.path_segments()
        .and_then(Iterator::last)
        .is_some_and(renderable_extension)
}

/// Whether a filename ends in an extension of one of [`DISPLAYABLE_FORMATS`].
///
/// Case-insensitive on the extension: Commons carries `.JPG`, and neither
/// source folds a name's case when it keys, so the fold has to happen here.
fn renderable_extension(name: &str) -> bool {
    let Some((_, extension)) = name.rsplit_once('.') else {
        return false;
    };
    let extension = extension.to_ascii_lowercase();
    DISPLAYABLE_FORMATS
        .iter()
        .any(|(extensions, _)| extensions.contains(&extension.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn key_for(url: &str) -> Result<MirrorKey, Box<dyn std::error::Error>> {
        Ok(MirrorKey::for_url(&Url::parse(url)?)?)
    }

    /// The name the funnel keys on, before it is hashed into a key.
    ///
    /// The byte-pinned tests assert this string beside the digest so the digest
    /// can be checked against an independent hash of it. Reaching it through
    /// [`Resolved`] keeps the assertion on what production classifies, rather
    /// than a source the test hands in.
    fn resolved_identity(url: &Url) -> Result<String, MirrorKeyError> {
        Ok(match Resolved::of(url)? {
            Resolved::Commons { title } => title,
            Resolved::Web { identity } => identity,
        })
    }

    #[test]
    fn commons_uploads_classify_as_commons() -> TestResult {
        let url = Url::parse("https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg")?;
        assert_eq!(SourceId::classify(&url), SourceId::Commons);
        Ok(())
    }

    #[test]
    fn other_hosts_classify_as_web() -> TestResult {
        let url = Url::parse("https://example.com/photo.jpg")?;
        assert_eq!(SourceId::classify(&url), SourceId::Web);
        Ok(())
    }

    #[test]
    fn a_project_local_upload_classifies_as_web() -> TestResult {
        // The upload host serves one repository per project beside Commons.
        // English Wikipedia's local uploads are not Commons files, and neither
        // is the host's own root.
        for spelling in [
            "https://upload.wikimedia.org/wikipedia/en/b/ba/Foo.jpg",
            "https://upload.wikimedia.org/wikipedia/de/1/12/Foo.jpg",
            "https://upload.wikimedia.org",
        ] {
            let url = Url::parse(spelling)?;
            assert_eq!(SourceId::classify(&url), SourceId::Web, "{spelling}");
        }
        Ok(())
    }

    #[test]
    fn a_project_local_upload_does_not_collide_with_a_commons_title() -> TestResult {
        // en-wiki's `Facade.jpg` and the Commons file of that name are two
        // different photographs. Keying both on the bare title would put them
        // at one address, and whichever mirrored first would win silently.
        assert_ne!(
            key_for("https://upload.wikimedia.org/wikipedia/en/b/ba/Facade.jpg")?,
            key_for("https://upload.wikimedia.org/wikipedia/commons/a/ab/Facade.jpg")?
        );
        Ok(())
    }

    #[test]
    fn fragment_does_not_affect_the_key() -> TestResult {
        assert_eq!(
            key_for("https://example.com/photo.jpg")?,
            key_for("https://example.com/photo.jpg#section")?
        );
        Ok(())
    }

    #[test]
    fn an_empty_query_keys_like_no_query() -> TestResult {
        // `?` with nothing after it reaches the server as no query at all.
        assert_eq!(
            key_for("https://example.com/photo.jpg")?,
            key_for("https://example.com/photo.jpg?")?
        );
        Ok(())
    }

    #[test]
    fn filename_case_is_significant() -> TestResult {
        // MediaWiki capitalizes a title's first letter and leaves the rest
        // alone, so these are two Commons files.
        assert_ne!(
            key_for("https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo_bar.jpg")?,
            key_for("https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo_Bar.jpg")?
        );
        Ok(())
    }

    #[test]
    fn a_commons_key_is_the_title_and_nothing_else() -> TestResult {
        // How to fetch the file is not which file it is: the scheme and any
        // query belong to the request, and the shard directories are a
        // function of the title, so none of them add information. The shards
        // are checked for shape and then dropped rather than recomputed, so a
        // shard that is not this title's MD5 still keys on the title.
        let canonical = key_for("https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg")?;
        assert_eq!(
            key_for("http://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg")?,
            canonical
        );
        assert_eq!(
            key_for("https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg?download")?,
            canonical
        );
        assert_eq!(
            key_for("https://upload.wikimedia.org/wikipedia/commons/9/9f/Foo.jpg")?,
            canonical
        );
        Ok(())
    }

    #[test]
    fn a_space_and_an_underscore_are_one_commons_title() -> TestResult {
        // MediaWiki treats them as the same character in a title, and the URL
        // form always carries the underscore.
        let underscored = key_for("https://upload.wikimedia.org/wikipedia/commons/a/ab/A_B.jpg")?;
        assert_eq!(
            key_for("https://upload.wikimedia.org/wikipedia/commons/a/ab/A%20B.jpg")?,
            underscored
        );
        // A literal space is percent-encoded by `Url::parse` before it gets
        // here, so this is the same case arriving by a different route.
        assert_eq!(
            key_for("https://upload.wikimedia.org/wikipedia/commons/a/ab/A B.jpg")?,
            underscored
        );
        Ok(())
    }

    #[test]
    fn query_order_is_significant() -> TestResult {
        // Preserved verbatim rather than sorted: a source that signs its query
        // would be broken by reordering it.
        assert_ne!(
            key_for("https://example.com/photo.jpg?a=1&b=2")?,
            key_for("https://example.com/photo.jpg?b=2&a=1")?
        );
        Ok(())
    }

    #[test]
    fn escaped_and_literal_spellings_of_a_commons_title_key_the_same() -> TestResult {
        // `url_for_filename` mints the escaped form, and so does the Commons
        // API: an `upload.wikimedia.org` URL arrives percent-encoded. The
        // literal spelling is what the `commons.wikimedia.org/wiki/File:`
        // page URL carries for the same title, on another host. Both decode
        // to one title, which is what keying on the filename buys.
        assert_eq!(
            key_for(
                "https://upload.wikimedia.org/wikipedia/commons/a/ab/Chrysler_Building_%281930%29.jpg"
            )?,
            key_for(
                "https://upload.wikimedia.org/wikipedia/commons/a/ab/Chrysler_Building_(1930).jpg"
            )?
        );
        assert_eq!(
            key_for(
                "https://upload.wikimedia.org/wikipedia/commons/a/ab/Machu_Picchu%2C_Peru.jpg"
            )?,
            key_for("https://upload.wikimedia.org/wikipedia/commons/a/ab/Machu_Picchu,_Peru.jpg")?
        );
        Ok(())
    }

    #[test]
    fn an_unclaimed_host_keys_its_encoding_verbatim() -> TestResult {
        // Deliberately not the Commons rule, and the reason that rule is
        // per-source. `(` and `,` are sub-delims a conforming client transmits
        // literally, so these are two request targets, and only the origin
        // server knows whether they name one object.
        assert_ne!(
            key_for("https://example.com/a%28b%29.jpg")?,
            key_for("https://example.com/a(b).jpg")?
        );
        assert_ne!(
            key_for("https://example.com/a%2Cb.jpg")?,
            key_for("https://example.com/a,b.jpg")?
        );
        Ok(())
    }

    #[test]
    fn a_web_percent_escape_folds_its_hex_case() -> TestResult {
        // RFC 3986 §2.1: `%2E` and `%2e` are one octet, so two spellings of one
        // escape name one resource. Without folding, `Foo%2Ejpg` and
        // `Foo%2ejpg` would mint two keys for one image.
        assert_eq!(
            key_for("https://example.com/Foo%2Ejpg")?,
            key_for("https://example.com/Foo%2ejpg")?
        );
        Ok(())
    }

    #[test]
    fn a_web_query_escape_keeps_its_hex_case() -> TestResult {
        // Only the path folds. The query is kept verbatim because a source may
        // sign it, and re-casing an escape there could break the signature, so
        // these stay two request targets.
        assert_ne!(
            key_for("https://example.com/p.jpg?sig=%2e")?,
            key_for("https://example.com/p.jpg?sig=%2E")?
        );
        Ok(())
    }

    #[test]
    fn web_userinfo_does_not_affect_the_key() -> TestResult {
        // Username and password address the requester, not the resource, so
        // `user@host/p`, `user:pass@host/p`, and `host/p` are one image.
        let bare = key_for("https://example.com/p.jpg")?;
        assert_eq!(key_for("https://user@example.com/p.jpg")?, bare);
        assert_eq!(key_for("https://user:pass@example.com/p.jpg")?, bare);
        Ok(())
    }

    #[test]
    fn an_encoded_slash_does_not_cross_a_segment_boundary() -> TestResult {
        // On an unclaimed host nothing is decoded, so these stay the two
        // distinct request targets they are.
        assert_ne!(
            key_for("https://example.com/a%2Fb.jpg")?,
            key_for("https://example.com/a/b.jpg")?
        );
        // Commons decodes, but only inside the segment the shape says is the
        // title, so `%2F` becomes a character in one title rather than a
        // separator that moves the object being addressed.
        let encoded = Url::parse("https://upload.wikimedia.org/wikipedia/commons/a/ab/a%2Fb.jpg")?;
        assert_eq!(resolved_identity(&encoded)?, "a/b.jpg");
        // Spelled literally it is one segment too long for a master, and the
        // shape parse rejects it rather than reading `b.jpg` out of the
        // directory position the title would then occupy.
        let literal = Url::parse("https://upload.wikimedia.org/wikipedia/commons/a/ab/a/b.jpg")?;
        assert_eq!(
            MirrorKey::for_url(&literal),
            Err(MirrorKeyError::NotACommonsFile { url: literal })
        );
        Ok(())
    }

    #[test]
    fn encoded_dot_segments_resolve_before_they_reach_us() -> TestResult {
        // The traversal-shaped half of the case above: a segment decoding to
        // `..` would, if it survived into the identity, name a different
        // upstream object than the URL we were handed.
        //
        // We are safe because `Url::parse` resolves `%2e` and `%2e%2e` as dot
        // segments per WHATWG, so a parsed `Url` never carries an encoded `..`
        // into either identity rule. That is an external guarantee rather than
        // one this module provides, which is why it is pinned here: it would
        // disappear silently if `url` changed, or if anything ever derived an
        // identity from a URL string that had not come through `Url::parse`.
        //
        // The blast radius is bounded either way, since the identity is hashed
        // into a fixed-length key and cannot escape our namespace. What would
        // change is which upstream URL a fetcher is pointed at.
        assert_eq!(
            key_for("https://example.com/a/%2E%2E/secret.jpg")?,
            key_for("https://example.com/secret.jpg")?
        );
        Ok(())
    }

    #[test]
    fn normalize_is_idempotent() -> TestResult {
        // A Web identity is a URL string, so it has to survive a round trip
        // through `Url::parse` unchanged; anything else would make the key
        // depend on how many times a URL had been re-parsed.
        for spelling in [
            "https://example.com/a%2Fb.jpg",
            "https://example.com/a(b).jpg",
            "https://example.com/100%25_cotton.jpg",
            "https://example.com/Caf%C3%A9.jpg",
            "https://example.com/photo.jpg?q=%2B1#frag",
            // A folded escape and a stripped userinfo must both be fixed points.
            "https://example.com/Foo%2ejpg",
            "https://user:pass@example.com/photo.jpg",
        ] {
            let once = normalize(&Url::parse(spelling)?);
            let twice = normalize(&Url::parse(&once)?);
            assert_eq!(once, twice, "normalize is not idempotent on {spelling}");
        }
        Ok(())
    }

    #[test]
    fn key_carries_its_source_prefix() -> TestResult {
        assert!(
            key_for("https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg")?
                .as_str()
                .starts_with("commons/")
        );
        assert!(
            key_for("https://example.com/photo.jpg")?
                .as_str()
                .starts_with("web/")
        );
        Ok(())
    }

    #[test]
    fn key_digest_is_hex_sha256() -> TestResult {
        let key = key_for("https://example.com/photo.jpg")?;
        let digest = key
            .as_str()
            .rsplit_once('/')
            .map(|(_, digest)| digest)
            .unwrap_or_default();
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
        Ok(())
    }

    // ==================== Byte-pinned keys ====================
    //
    // Every other test here is relational: it asserts that two URLs key the
    // same or differently, which stays true under any edit to `normalize`
    // that moves both keys together. A rewrite of the identity string is
    // exactly that kind of edit, and it rekeys the whole bucket: every
    // already-mirrored object becomes unaddressable while the suite still
    // passes. These pin the bytes, so such a change has to be deliberate.
    //
    // Each one pins the identity string beside its digest, so the digest can
    // be checked against something other than the code that produced it:
    //
    //     printf '%s' 'Foo.jpg' | shasum -a 256
    //
    // To move a key on purpose: recompute the digest that way, not by copying
    // what the failing test printed, and plan the re-mirror in the same
    // change. `core/src/wire_goldens.rs` runs the same discipline for the
    // fact grammar.

    #[test]
    fn the_commons_key_is_byte_pinned() -> TestResult {
        let url = Url::parse("https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg")?;
        assert_eq!(resolved_identity(&url)?, "Foo.jpg");
        assert_eq!(
            MirrorKey::for_url(&url)?.as_str(),
            "commons/58cbc7f0645e9dc8ec3d2af33a5a7221ed7b5707f4ea18670025f9a32e7fe0dc"
        );
        Ok(())
    }

    #[test]
    fn the_web_key_is_byte_pinned() -> TestResult {
        let url = Url::parse("https://example.com/photo.jpg")?;
        assert_eq!(resolved_identity(&url)?, "https://example.com/photo.jpg");
        assert_eq!(
            MirrorKey::for_url(&url)?.as_str(),
            "web/5cd76d96bc2f2aecf99356e54cb349c5efb270c1cd0d030b78511575174af695"
        );
        Ok(())
    }

    #[test]
    fn a_project_local_upload_is_byte_pinned_to_its_url() -> TestResult {
        // The whole URL, not the title: en-wiki's `Foo.jpg` shares the Commons
        // host and nothing else, so pinning this pins that the repository
        // prefix is what separates them.
        let url = Url::parse("https://upload.wikimedia.org/wikipedia/en/b/ba/Foo.jpg")?;
        assert_eq!(
            resolved_identity(&url)?,
            "https://upload.wikimedia.org/wikipedia/en/b/ba/Foo.jpg"
        );
        assert_eq!(
            MirrorKey::for_url(&url)?.as_str(),
            "web/dedf8c7d724b4f8364b9c48c4fd734a073cbaf9b41351980f02b9bdb7d3a7954"
        );
        Ok(())
    }

    #[test]
    fn a_web_lowercase_escape_is_byte_pinned_to_its_uppercase_form() -> TestResult {
        // The lowercase spelling folds to the uppercase escape before it keys,
        // so the identity is the uppercased path. Recompute independently:
        //   printf '%s' 'https://example.com/Foo%2Ejpg' | shasum -a 256
        let lower = Url::parse("https://example.com/Foo%2ejpg")?;
        assert_eq!(resolved_identity(&lower)?, "https://example.com/Foo%2Ejpg");
        assert_eq!(
            MirrorKey::for_url(&lower)?.as_str(),
            "web/fad1c1e6018ffd9a2d6a3154aa41774fc13dc3b84cac90ace352247521631601"
        );
        Ok(())
    }

    #[test]
    fn a_web_url_with_userinfo_is_byte_pinned_without_it() -> TestResult {
        // Stripping userinfo lands on the bare-host identity exactly: the digest
        // is the existing no-userinfo `photo.jpg` golden, not merely equal to
        // some other userinfo'd spelling.
        let url = Url::parse("https://user:pass@example.com/photo.jpg")?;
        assert_eq!(resolved_identity(&url)?, "https://example.com/photo.jpg");
        assert_eq!(
            MirrorKey::for_url(&url)?.as_str(),
            "web/5cd76d96bc2f2aecf99356e54cb349c5efb270c1cd0d030b78511575174af695"
        );
        Ok(())
    }

    #[test]
    fn an_escaped_commons_title_is_byte_pinned_to_its_decoded_form() -> TestResult {
        // Pins the direction as well as the collapse: both spellings must land
        // on the decoded title, not merely on each other.
        let escaped = Url::parse(
            "https://upload.wikimedia.org/wikipedia/commons/a/ab/Chrysler_Building_%281930%29.jpg",
        )?;
        let literal = Url::parse(
            "https://upload.wikimedia.org/wikipedia/commons/a/ab/Chrysler_Building_(1930).jpg",
        )?;
        assert_eq!(resolved_identity(&escaped)?, "Chrysler_Building_(1930).jpg");
        assert_eq!(
            MirrorKey::for_url(&escaped)?.as_str(),
            "commons/98ae1e1638c1d775dd82b620f9c8aa780ea113b05d6fee3b03aefb657e48cfde"
        );
        assert_eq!(MirrorKey::for_url(&literal)?, MirrorKey::for_url(&escaped)?);
        Ok(())
    }

    // ==================== Scheme and filename gates ====================

    #[test]
    fn non_http_schemes_have_no_mirror_key() -> TestResult {
        for spelling in [
            "file:///etc/passwd",
            "data:image/png;base64,iVBORw0KGgo=",
            "ftp://example.com/photo.jpg",
        ] {
            let url = Url::parse(spelling)?;
            assert_eq!(
                MirrorKey::for_url(&url),
                Err(MirrorKeyError::UnsupportedScheme { url: url.clone() }),
                "{spelling} should have no key"
            );
        }
        Ok(())
    }

    #[test]
    fn a_commons_path_matching_neither_shape_has_no_key() -> TestResult {
        // Inside the Commons repository the shape is the whole contract, and
        // anything else is guesswork: falling back to URL identity would key
        // one object two ways depending on which spelling reached which code
        // path.
        for spelling in [
            // Stops at a directory, so there is no title segment.
            "https://upload.wikimedia.org/wikipedia/commons/a/ab/",
            // Bytes no title could contain.
            "https://upload.wikimedia.org/wikipedia/commons/a/ab/%FF.jpg",
            // No shards at all: the repository root is not a file.
            "https://upload.wikimedia.org/wikipedia/commons/Foo.jpg",
            // Shards that are no hex, so this is some other directory layout
            // and the last segment is not known to be a title.
            "https://upload.wikimedia.org/wikipedia/commons/x/xy/Foo.jpg",
            // A thumbnail path with the rendition segment missing, which is
            // neither shape.
            "https://upload.wikimedia.org/wikipedia/commons/thumb/a/ab/Foo.jpg",
            // A derivative that is not a thumbnail, so we cannot name the
            // master it came from.
            "https://upload.wikimedia.org/wikipedia/commons/transcoded/a/ab/Foo.ogv/Foo.ogv.webm",
        ] {
            let url = Url::parse(spelling)?;
            assert_eq!(
                MirrorKey::for_url(&url),
                Err(MirrorKeyError::NotACommonsFile { url: url.clone() }),
                "{spelling} names no Commons file"
            );
        }
        // The same shapes key fine outside the repository, where the URL
        // itself is the identity and there is no title to be missing.
        assert!(key_for("https://example.com/photos/").is_ok());
        assert!(key_for("https://upload.wikimedia.org/wikipedia/en/x/xy/Foo.jpg").is_ok());
        Ok(())
    }

    #[test]
    fn a_commons_thumbnail_has_no_key() -> TestResult {
        // A thumbnail carries the title in a directory position, so its last
        // segment is a rendition and no file by that name exists. Keying it
        // would split one photograph across two keys and let `Detail` be
        // built by upscaling a 960px derivative.
        let url = Url::parse(
            "https://upload.wikimedia.org/wikipedia/commons/thumb/c/ca/\
             Machu_Picchu%2C_Peru_%282018%29.jpg/960px-Machu_Picchu%2C_Peru_%282018%29.jpg",
        )?;
        assert_eq!(SourceId::classify(&url), SourceId::Commons);
        assert_eq!(
            MirrorKey::for_url(&url),
            Err(MirrorKeyError::CommonsThumbnail {
                url: url.clone(),
                title: "Machu_Picchu,_Peru_(2018).jpg".to_string(),
            })
        );
        // The master keys, and on the title the rejected URL named.
        let master = Url::parse(
            "https://upload.wikimedia.org/wikipedia/commons/c/ca/Machu_Picchu%2C_Peru_%282018%29.jpg",
        )?;
        assert_eq!(resolved_identity(&master)?, "Machu_Picchu,_Peru_(2018).jpg");
        // Nothing renders from a rendition either, since there is no key to
        // render.
        assert!(!displayable(&url));
        Ok(())
    }

    #[test]
    fn a_thumbnail_rejection_names_the_file_to_mirror() -> TestResult {
        // The message is what an operator sees when a fact cites a thumbnail,
        // and the fix is to cite the master, so the title has to be in it.
        let url = Url::parse(
            "https://upload.wikimedia.org/wikipedia/commons/thumb/c/ca/Facade.jpg/64px-Facade.jpg",
        )?;
        let error = MirrorKey::for_url(&url)
            .err()
            .ok_or("a thumbnail must be rejected")?;
        let message = error.to_string();
        assert!(message.contains("Facade.jpg"), "{message}");
        assert!(message.contains("master"), "{message}");
        Ok(())
    }

    #[test]
    fn plain_http_keys_differently_from_https_on_an_unclaimed_host() -> TestResult {
        // There the whole URL is the identity, so the scheme is part of it.
        assert_ne!(
            key_for("http://example.com/photo.jpg")?,
            key_for("https://example.com/photo.jpg")?
        );
        Ok(())
    }

    // ==================== Displayable gate ====================

    #[test]
    fn displayable_accepts_the_intersection() -> TestResult {
        for path in ["a.jpg", "a.jpeg", "a.png", "a.gif", "a.webp"] {
            let url = Url::parse(&format!("https://example.com/{path}"))?;
            assert!(displayable(&url), "{path} should be displayable");
        }
        Ok(())
    }

    #[test]
    fn displayable_rejects_formats_neither_side_handles() -> TestResult {
        // svg and avif: Cloudflare-side. tif/tiff/heic: local-decoder-side.
        // pdf and video: neither.
        for path in [
            "a.svg", "a.avif", "a.tif", "a.tiff", "a.heic", "a.pdf", "a.mp4", "a.webm", "a.ogv",
        ] {
            let url = Url::parse(&format!("https://example.com/{path}"))?;
            assert!(!displayable(&url), "{path} should not be displayable");
        }
        Ok(())
    }

    #[test]
    fn displayable_ignores_extension_case() -> TestResult {
        // Commons has `.JPG`, and normalization preserves path case.
        for path in ["a.JPG", "a.Jpeg", "a.PNG"] {
            let url = Url::parse(&format!("https://example.com/{path}"))?;
            assert!(displayable(&url), "{path} should be displayable");
        }
        Ok(())
    }

    #[test]
    fn displayable_rejects_a_url_with_no_extension() -> TestResult {
        let url = Url::parse("https://example.com/photo")?;
        assert!(!displayable(&url));
        Ok(())
    }

    #[test]
    fn a_commons_title_decides_display_after_decoding() -> TestResult {
        // `%2E` spells the `.` of an extension. Identity decodes it, so these
        // are one stored object; reading the raw segment here would find no
        // extension in one of them and the same object would render or not
        // depending on which spelling the read path was handed.
        let escaped = Url::parse("https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo%2Ejpg")?;
        let literal = Url::parse("https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg")?;
        assert_eq!(MirrorKey::for_url(&escaped)?, MirrorKey::for_url(&literal)?);
        assert!(displayable(&escaped));
        assert_eq!(
            DisplayableKey::for_url(&escaped)?,
            DisplayableKey::for_url(&literal)?
        );
        // The gate still gates: an SVG title is opaque under either spelling.
        let plan = Url::parse("https://upload.wikimedia.org/wikipedia/commons/a/ab/Plan%2Esvg")?;
        assert!(!displayable(&plan));
        Ok(())
    }

    #[test]
    fn an_unclaimed_host_decides_display_on_its_raw_segment() -> TestResult {
        // The counterpart of the rule above, and the reason it is per-source:
        // off Commons the two spellings are two keys, so decoding here would
        // make one key renderable through the other's name.
        let escaped = Url::parse("https://example.com/Foo%2Ejpg")?;
        assert_ne!(
            MirrorKey::for_url(&escaped)?,
            MirrorKey::for_url(&Url::parse("https://example.com/Foo.jpg")?)?
        );
        assert!(!displayable(&escaped));
        Ok(())
    }

    #[test]
    fn a_displayable_key_exists_only_for_renderable_urls() -> TestResult {
        let renderable = Url::parse("https://example.com/photo.jpg")?;
        let opaque = Url::parse("https://example.com/plan.svg")?;
        assert!(DisplayableKey::for_url(&renderable)?.is_some());
        assert_eq!(DisplayableKey::for_url(&opaque)?, None);
        Ok(())
    }

    #[test]
    fn a_displayable_key_addresses_the_same_object_as_its_mirror_key() -> TestResult {
        // The gate changes what may be *built* from a key, never where the
        // bytes live; a warm path and a read path must agree on the address.
        let url = Url::parse("https://example.com/photo.jpg")?;
        let displayable =
            DisplayableKey::for_url(&url)?.ok_or("photo.jpg should be displayable")?;
        assert_eq!(displayable.key(), &MirrorKey::for_url(&url)?);
        Ok(())
    }

    #[test]
    fn an_unfetchable_url_is_not_displayable() -> TestResult {
        // A local JPEG looks renderable by its extension but has no upstream to
        // mirror. `displayable` resolves the URL the same way a key does, so the
        // unfetchable scheme settles it before the filename is consulted, and
        // building a key rejects it too.
        let url = Url::parse("file:///photos/a.jpg")?;
        assert!(!displayable(&url));
        assert!(DisplayableKey::for_url(&url).is_err());
        Ok(())
    }
}
