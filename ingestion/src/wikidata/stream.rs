//! Async streaming utilities for Wikidata dumps.
//!
//! Provides composable stream components for processing Wikidata JSON dumps:
//! - Async decompression (gzip, bzip2)
//! - JSON entity parsing (handles Wikidata's array-wrapped format)
//! - Filtering predicates

use anyhow::{Context, Result};
use async_compression::tokio::bufread::{BzDecoder, GzipDecoder};
use futures::stream::{self, Stream};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};

use crate::wikidata::parsing::{get_claim_qid, get_claims};

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
pub fn wikidata_entities<R>(reader: R) -> impl Stream<Item = Result<Value>>
where
    R: AsyncBufRead + Unpin,
{
    stream::unfold(
        (reader, String::new()),
        |(mut reader, mut line_buf)| async move {
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
                            Ok(value) => return Some((Ok(value), (reader, line_buf))),
                            Err(e) => {
                                return Some((
                                    Err(anyhow::anyhow!("JSON parse error: {}", e)),
                                    (reader, line_buf),
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        return Some((
                            Err(anyhow::anyhow!("Read error: {}", e)),
                            (reader, line_buf),
                        ));
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
pub fn is_instance_of(entity: &Value, target_types: &HashSet<String>) -> bool {
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
    fn test_is_instance_of() {
        let targets: HashSet<String> = ["Q41176".to_string()].into_iter().collect();

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
    }
}
