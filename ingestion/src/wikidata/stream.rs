//! Async streaming utilities for Wikidata dumps.
//!
//! Provides composable stream components for processing Wikidata JSON dumps:
//! - Async decompression (gzip, bzip2)
//! - JSON entity parsing (handles Wikidata's array-wrapped format)
//! - Filtering predicates

use anyhow::{Context, Result};
use async_compression::tokio::bufread::{BzDecoder, GzipDecoder};
use chronoscope_integrations::wikidata::WikidataId;
use futures::stream::{self, Stream};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};

/// Get a Q-ID reference from a raw claim JSON's mainsnak (for wikibase-entityid type).
fn get_claim_qid(claim: &Value) -> Option<&str> {
    claim
        .pointer("/mainsnak/datavalue/value/id")
        .and_then(|i| i.as_str())
}

/// Get claims array for a property from a raw entity JSON.
fn get_claims<'a>(wd: &'a Value, property: &str) -> Option<&'a Vec<Value>> {
    wd.get("claims")
        .and_then(|c| c.get(property))
        .and_then(|p| p.as_array())
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

/// Parse Wikidata JSON dump lines into a stream of entities.
///
/// Wikidata dumps are JSON arrays: `[ {entity}, {entity}, ... ]`
/// This handles the array brackets and trailing commas, yielding parsed entities.
///
/// A JSON parse error yields the error and resumes on the next line — one bad
/// record shouldn't abort a multi-gigabyte dump. A read/decode error is fatal:
/// a latched decompressor error would repeat forever, so the error surfaces
/// once and the stream ends (state carries `None` in place of the reader).
pub fn wikidata_entities<R>(reader: R) -> impl Stream<Item = Result<Value>>
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

                        match serde_json::from_str(json_str) {
                            Ok(value) => return Some((Ok(value), (Some(reader), line_buf))),
                            Err(e) => {
                                return Some((
                                    Err(anyhow::anyhow!("JSON parse error: {}", e)),
                                    (Some(reader), line_buf),
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        return Some((Err(anyhow::anyhow!("Read error: {}", e)), (None, line_buf)));
                    }
                }
            }
        },
    )
}

/// Check if an entity is a Wikidata item (not a property, lexeme, etc.).
pub fn is_item(entity: &Value) -> bool {
    entity.get("type").and_then(|t| t.as_str()) == Some("item")
}

/// Check if an entity is an instance of any target type (via P31).
pub fn is_instance_of(entity: &Value, target_types: &HashSet<WikidataId>) -> bool {
    if !is_item(entity) {
        return false;
    }

    let Some(claims) = get_claims(entity, "P31") else {
        return false;
    };

    claims
        .iter()
        .filter_map(|c| get_claim_qid(c))
        .any(|qid| target_types.contains(qid))
}

/// P279 (subclass-of) parent Q-IDs of an item entity.
///
/// A P279 value `B` on entity `A` means "A is a subclass of B"; each yielded
/// Q-ID is such a `B`. Non-items and items without P279 yield nothing.
pub fn subclass_parents(entity: &Value) -> impl Iterator<Item = &str> {
    let claims = is_item(entity)
        .then(|| get_claims(entity, "P279"))
        .flatten();
    claims.into_iter().flatten().filter_map(get_claim_qid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_is_item() {
        assert!(is_item(&json!({"type": "item"})));
        assert!(!is_item(&json!({"type": "property"})));
        assert!(!is_item(&json!({})));
    }

    #[test]
    fn test_is_instance_of() -> Result<(), Box<dyn std::error::Error>> {
        let targets: HashSet<WikidataId> = [WikidataId::try_from("Q41176".to_string())?]
            .into_iter()
            .collect();

        let building = json!({
            "type": "item",
            "claims": {
                "P31": [{
                    "mainsnak": {
                        "datavalue": {
                            "value": {"id": "Q41176"}
                        }
                    }
                }]
            }
        });
        assert!(is_instance_of(&building, &targets));

        let other = json!({
            "type": "item",
            "claims": {
                "P31": [{
                    "mainsnak": {
                        "datavalue": {
                            "value": {"id": "Q12345"}
                        }
                    }
                }]
            }
        });
        assert!(!is_instance_of(&other, &targets));

        // Not an item
        let property = json!({"type": "property"});
        assert!(!is_instance_of(&property, &targets));

        Ok(())
    }

    #[test]
    fn subclass_parents_reads_p279_qids_of_items_only() {
        let subclass = json!({
            "type": "item",
            "claims": {
                "P279": [
                    { "mainsnak": { "datavalue": { "value": { "id": "Q811979" } } } },
                    { "mainsnak": { "datavalue": { "value": { "id": "Q41176" } } } }
                ]
            }
        });
        let parents: Vec<&str> = subclass_parents(&subclass).collect();
        assert_eq!(parents, vec!["Q811979", "Q41176"]);

        // No P279 claims, and a non-item both yield nothing.
        assert_eq!(subclass_parents(&json!({"type": "item"})).count(), 0);
        let property = json!({
            "type": "property",
            "claims": { "P279": [{ "mainsnak": { "datavalue": { "value": { "id": "Q1" } } } }] }
        });
        assert_eq!(subclass_parents(&property).count(), 0);
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
        use futures::StreamExt;

        let reader = BufReader::new(FailingReader);
        let mut stream = std::pin::pin!(wikidata_entities(reader));

        assert!(
            matches!(stream.next().await, Some(Err(_))),
            "the read error surfaces once"
        );
        assert!(
            stream.next().await.is_none(),
            "the stream ends after a read error rather than re-reading the failed reader"
        );
    }
}
