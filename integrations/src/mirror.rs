//! The mirror queue message: the contract between the dispatcher and the
//! consumer.
//!
//! The dispatcher (Rust) walks the fact store and sends one of these per image;
//! the consumer (a Cloudflare Worker, in JS) reads it, fetches the URL, and
//! stores the bytes at the key. Everything the fetch needs travels in the
//! message, so the two fetch implementations that exist (the production Worker
//! and the dev-time Rust fetcher) stay glue and cannot drift on policy.
//!
//! Because one end is JS, the JSON shape is the interface, not this struct. The
//! golden test pins it, the way the firewall rule and its Rust twin are pinned:
//! a field renamed on one side and not the other is a silent break otherwise.

use serde::{Deserialize, Serialize};
use url::Url;

/// The message version. The consumer dead-letters a version it does not know,
/// so a shape change is a bump here plus a re-dispatch, never a migration.
pub const MIRROR_MESSAGE_VERSION: u32 = 1;

/// An instruction to fetch `url` and store it at `key`.
///
/// A wire message, so its fields are public and carry no invariant the type
/// enforces: a received message may hold any `v` (that is how the consumer
/// spots an old one) and any `key` string (the key was validated where it was
/// derived, and is opaque here). Build an outgoing one with [`MirrorRequest::new`],
/// which stamps the current version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorRequest {
    /// Message version; [`MIRROR_MESSAGE_VERSION`] on anything freshly built.
    pub v: u32,
    /// Where to store the bytes: a [`MirrorKey`](crate::MirrorKey) rendered to
    /// its string, e.g. `commons/<sha256hex>`.
    pub key: String,
    /// The upstream URL to fetch.
    pub url: Url,
    /// The User-Agent to fetch with. Per-source, because Wikimedia blocks an
    /// unidentified client, and other hosts will want their own.
    pub user_agent: String,
    /// Content types the fetched bytes may carry; the consumer refuses others.
    /// The second format gate: the extension the dispatcher filtered on was a
    /// claim, and the fetch is the first thing to see the true type.
    pub accept: Vec<String>,
    /// The largest response the consumer stores; a bigger one dead-letters,
    /// bounding what one message writes to paid storage. Archival sizing is a
    /// later phase.
    pub max_bytes: u64,
}

impl MirrorRequest {
    /// Build an outgoing message, stamped with the current version.
    #[must_use]
    pub fn new(
        key: String,
        url: Url,
        user_agent: String,
        accept: Vec<String>,
        max_bytes: u64,
    ) -> Self {
        Self {
            v: MIRROR_MESSAGE_VERSION,
            key,
            url,
            user_agent,
            accept,
            max_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn sample() -> Result<MirrorRequest, Box<dyn std::error::Error>> {
        Ok(MirrorRequest::new(
            "commons/58cbc7f0645e9dc8ec3d2af33a5a7221ed7b5707f4ea18670025f9a32e7fe0dc".to_string(),
            Url::parse("https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg")?,
            "Chronoscope/1.0 (https://chronoscope.io)".to_string(),
            vec!["image/jpeg".to_string(), "image/png".to_string()],
            104_857_600,
        ))
    }

    /// Byte-pin the wire shape the JS consumer reads. A field renamed, retyped,
    /// or reordered against the consumer is a silent break; this is the guard,
    /// the same role the firewall rule's Rust twin plays.
    #[test]
    fn the_wire_shape_is_byte_pinned() -> TestResult {
        let json = serde_json::to_string(&sample()?)?;
        assert_eq!(
            json,
            r#"{"v":1,"key":"commons/58cbc7f0645e9dc8ec3d2af33a5a7221ed7b5707f4ea18670025f9a32e7fe0dc","url":"https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg","user_agent":"Chronoscope/1.0 (https://chronoscope.io)","accept":["image/jpeg","image/png"],"max_bytes":104857600}"#
        );
        Ok(())
    }

    #[test]
    fn new_stamps_the_current_version() -> TestResult {
        assert_eq!(sample()?.v, MIRROR_MESSAGE_VERSION);
        Ok(())
    }

    #[test]
    fn a_message_round_trips() -> TestResult {
        let req = sample()?;
        let json = serde_json::to_string(&req)?;
        let back: MirrorRequest = serde_json::from_str(&json)?;
        assert_eq!(req, back);
        Ok(())
    }
}
