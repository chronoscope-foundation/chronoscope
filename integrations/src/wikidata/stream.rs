//! Async streaming utilities for Wikidata dumps.
//!
//! Provides composable stream components for processing Wikidata JSON dumps:
//! - Async decompression (gzip, bzip2)
//! - JSON entity parsing (handles Wikidata's array-wrapped format)
//! - Filtering predicates

use crate::wikidata::WikidataId;
use anyhow::{Context, Result};
use async_compression::tokio::bufread::{BzDecoder, GzipDecoder};
use futures_util::stream::{self, Stream};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};

/// Get a Q-ID reference from a raw claim JSON's mainsnak (for wikibase-entityid type).
fn claim_qid(claim: &Value) -> Option<&str> {
    claim
        .pointer("/mainsnak/datavalue/value/id")
        .and_then(|i| i.as_str())
}

/// A Wikidata dump record is an item (not a property, lexeme, etc.).
fn is_item(entity_type: Option<&str>) -> bool {
    entity_type == Some("item")
}

/// Buffer size for async I/O (8 MB).
const BUFFER_SIZE: usize = 8 * 1024 * 1024;

/// Open a file with automatic decompression based on extension.
pub async fn open_compressed(path: &Path) -> Result<Box<dyn AsyncBufRead + Send + Unpin>> {
    let file = File::open(path)
        .await
        .with_context(|| format!("Failed to open {}", path.display()))?;
    let reader = BufReader::with_capacity(BUFFER_SIZE, file);

    match path.extension().and_then(|e| e.to_str()) {
        Some("gz") => {
            // Wikidata dumps use concatenated gzip streams
            let mut decoder = GzipDecoder::new(reader);
            decoder.multiple_members(true);
            Ok(Box::new(BufReader::with_capacity(BUFFER_SIZE, decoder)))
        }
        Some("bz2") => {
            let decoder = BzDecoder::new(reader);
            Ok(Box::new(BufReader::with_capacity(BUFFER_SIZE, decoder)))
        }
        _ => Ok(Box::new(reader)),
    }
}

/// Yield the raw JSON text of each entity in a Wikidata dump line stream.
///
/// Array brackets, blank lines, and trailing commas are stripped; each yielded
/// String is one entity object ready to deserialize. A read/decode error is
/// fatal — surfaced once, then the stream ends (state carries `None` in place
/// of the reader); a per-line JSON parse error is the consumer's to handle.
pub(crate) fn wikidata_lines<R>(reader: R) -> impl Stream<Item = Result<String>>
where
    R: AsyncBufRead + Unpin,
{
    stream::unfold(
        (Some(reader), String::new()),
        |(reader, mut line_buf)| async move {
            let mut reader = reader?; // None => a read error already ended the stream
            loop {
                line_buf.clear();
                match reader.read_line(&mut line_buf).await {
                    Ok(0) => return None, // EOF
                    Ok(_) => {
                        let trimmed = line_buf.trim();

                        // Skip array brackets and empty lines
                        if trimmed.is_empty() || trimmed == "[" || trimmed == "]" {
                            continue;
                        }

                        // Strip trailing comma
                        let json_str = trimmed.trim_end_matches(',');

                        return Some((Ok(json_str.to_owned()), (Some(reader), line_buf)));
                    }
                    Err(e) => {
                        return Some((Err(anyhow::anyhow!("Read error: {}", e)), (None, line_buf)));
                    }
                }
            }
        },
    )
}

/// Slim view of a dump entity for the P279 (subclass-of) pass: only the fields
/// the subclass-graph build reads. Every other field — labels, descriptions,
/// sitelinks, other claims — is structurally skipped during deserialization.
#[derive(serde::Deserialize)]
pub(crate) struct SubclassView {
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "type")]
    entity_type: Option<String>,
    #[serde(default)]
    claims: SubclassClaims,
}

#[derive(serde::Deserialize, Default)]
struct SubclassClaims {
    #[serde(default, rename = "P279")]
    p279: Vec<Value>,
}

/// Slim view of a dump entity for the P31 (instance-of) pass: only the fields
/// the match predicate reads. Every other field is structurally skipped.
#[derive(serde::Deserialize)]
pub(crate) struct InstanceView {
    #[serde(default, rename = "type")]
    entity_type: Option<String>,
    #[serde(default)]
    claims: InstanceClaims,
}

#[derive(serde::Deserialize, Default)]
struct InstanceClaims {
    #[serde(default, rename = "P31")]
    p31: Vec<Value>,
}

impl SubclassView {
    /// P279 (subclass-of) parent Q-IDs. A P279 value `B` on this entity means
    /// "this is a subclass of B"; each yielded Q-ID is such a `B`. A non-item
    /// yields nothing.
    pub(crate) fn subclass_parents(&self) -> impl Iterator<Item = &str> {
        is_item(self.entity_type.as_deref())
            .then_some(&self.claims.p279)
            .into_iter()
            .flatten()
            .filter_map(claim_qid)
    }

    pub(crate) fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

impl InstanceView {
    /// True when this item is an instance (P31) of any target type.
    pub(crate) fn is_instance_of(&self, target_types: &HashSet<WikidataId>) -> bool {
        is_item(self.entity_type.as_deref())
            && self
                .claims
                .p31
                .iter()
                .filter_map(claim_qid)
                .any(|q| target_types.contains(q))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_instance_of_matches_item_p31_against_targets() -> Result<(), Box<dyn std::error::Error>> {
        let targets: HashSet<WikidataId> = [WikidataId::try_from("Q41176".to_string())?]
            .into_iter()
            .collect();

        let building: InstanceView = serde_json::from_str(
            r#"{"type":"item","claims":{"P31":[{"mainsnak":{"datavalue":{"value":{"id":"Q41176"}}}}]}}"#,
        )?;
        assert!(building.is_instance_of(&targets), "P31 names a target type");

        let other: InstanceView = serde_json::from_str(
            r#"{"type":"item","claims":{"P31":[{"mainsnak":{"datavalue":{"value":{"id":"Q12345"}}}}]}}"#,
        )?;
        assert!(!other.is_instance_of(&targets), "P31 misses every target");

        Ok(())
    }

    #[test]
    fn is_instance_of_is_false_for_non_items() -> Result<(), Box<dyn std::error::Error>> {
        let targets: HashSet<WikidataId> = [WikidataId::try_from("Q41176".to_string())?]
            .into_iter()
            .collect();

        // Only item entities qualify, whatever their P31.
        let property: InstanceView = serde_json::from_str(
            r#"{"type":"property","claims":{"P31":[{"mainsnak":{"datavalue":{"value":{"id":"Q41176"}}}}]}}"#,
        )?;
        assert!(!property.is_instance_of(&targets));

        Ok(())
    }

    #[test]
    fn subclass_parents_reads_p279_qids_of_items_only() -> Result<(), Box<dyn std::error::Error>> {
        let subclass: SubclassView = serde_json::from_str(
            r#"{"type":"item","claims":{"P279":[
                {"mainsnak":{"datavalue":{"value":{"id":"Q811979"}}}},
                {"mainsnak":{"datavalue":{"value":{"id":"Q41176"}}}}
            ]}}"#,
        )?;
        let parents: Vec<&str> = subclass.subclass_parents().collect();
        assert_eq!(parents, vec!["Q811979", "Q41176"]);

        let no_p279: SubclassView = serde_json::from_str(r#"{"type":"item"}"#)?;
        assert_eq!(
            no_p279.subclass_parents().count(),
            0,
            "an item without P279 yields nothing"
        );

        let property: SubclassView = serde_json::from_str(
            r#"{"type":"property","claims":{"P279":[{"mainsnak":{"datavalue":{"value":{"id":"Q1"}}}}]}}"#,
        )?;
        assert_eq!(
            property.subclass_parents().count(),
            0,
            "non-items yield nothing"
        );

        Ok(())
    }

    /// A reader whose first read fails, modeling a latched decompressor error.
    struct FailingReader;

    impl tokio::io::AsyncRead for FailingReader {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Err(std::io::Error::other("simulated decode failure")))
        }
    }

    #[tokio::test]
    async fn read_error_ends_stream_instead_of_re_reading() {
        use futures_util::StreamExt;

        let reader = BufReader::new(FailingReader);
        let mut stream = std::pin::pin!(wikidata_lines(reader));

        assert!(
            matches!(stream.next().await, Some(Err(_))),
            "the read error surfaces once"
        );
        assert!(
            stream.next().await.is_none(),
            "the stream ends after a read error rather than re-reading the failed reader"
        );
    }

    #[tokio::test]
    async fn wikidata_lines_strips_brackets_and_trailing_commas()
    -> Result<(), Box<dyn std::error::Error>> {
        use futures_util::StreamExt;

        let data = b"[\n{\"id\":\"Q1\"},\n{\"id\":\"Q2\"}\n]\n";
        let reader = BufReader::new(&data[..]);
        let mut stream = std::pin::pin!(wikidata_lines(reader));

        let first = stream
            .next()
            .await
            .ok_or_else(|| "expected first entity".to_string())?
            .map_err(|e| e.to_string())?;
        assert_eq!(first, r#"{"id":"Q1"}"#);

        let second = stream
            .next()
            .await
            .ok_or_else(|| "expected second entity".to_string())?
            .map_err(|e| e.to_string())?;
        assert_eq!(second, r#"{"id":"Q2"}"#);

        assert!(
            stream.next().await.is_none(),
            "the stream ends after the closing bracket"
        );
        Ok(())
    }
}
