//! Media storage abstraction for workers.
//!
//! This module provides a `MediaStore` trait for storing and retrieving media files,
//! with an in-memory implementation for development and testing.

use std::collections::HashMap;
use std::io::Cursor;
use std::pin::Pin;
use std::sync::RwLock;

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Error type for media store operations.
#[derive(Debug, thiserror::Error)]
pub enum MediaStoreError {
    /// I/O error during media operations.
    #[error("media store I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Lock was poisoned (a thread panicked while holding it).
    #[error("lock poisoned: {0}")]
    LockPoisoned(String),
}

/// Metadata about stored media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaMetadata {
    pub content_type: String,
    pub size: u64,
}

/// Media content with metadata, returned by `get`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaWithMetadata {
    pub data: Bytes,
    pub metadata: MediaMetadata,
}

/// Trait for media storage, allowing different backends (memory, filesystem, S3).
///
/// This trait is object-safe (dyn-compatible), so you can use `Arc<dyn MediaStore>`.
#[async_trait::async_trait]
pub trait MediaStore: Send + Sync {
    /// Store media from an async reader.
    ///
    /// Reads all data from the reader and stores it under the given key.
    /// If a value already exists for the key, it is overwritten.
    async fn put_stream(
        &self,
        key: &str,
        reader: Pin<Box<dyn AsyncRead + Send>>,
        content_type: &str,
    ) -> Result<(), MediaStoreError>;

    /// Retrieve media as an async reader.
    ///
    /// Returns `None` if the key does not exist.
    async fn get_stream(
        &self,
        key: &str,
    ) -> Result<Option<Pin<Box<dyn AsyncRead + Send>>>, MediaStoreError>;

    /// Get metadata about stored media without retrieving the content.
    ///
    /// Returns `None` if the key does not exist.
    async fn head(&self, key: &str) -> Result<Option<MediaMetadata>, MediaStoreError>;

    // ==================== Convenience Methods ====================

    /// Store media from bytes (convenience wrapper around `put_stream`).
    async fn put(&self, key: &str, data: Bytes, content_type: &str) -> Result<(), MediaStoreError> {
        self.put_stream(key, Box::pin(Cursor::new(data)), content_type)
            .await
    }

    /// Retrieve media with metadata (convenience wrapper around `get_stream` + `head`).
    ///
    /// Returns `None` if the key does not exist.
    async fn get(&self, key: &str) -> Result<Option<MediaWithMetadata>, MediaStoreError> {
        let Some(metadata) = self.head(key).await? else {
            return Ok(None);
        };
        match self.get_stream(key).await? {
            Some(mut reader) => {
                let mut buf = Vec::new();
                reader.read_to_end(&mut buf).await?;
                Ok(Some(MediaWithMetadata {
                    data: Bytes::from(buf),
                    metadata,
                }))
            }
            None => Ok(None),
        }
    }

    /// Check if a key exists.
    async fn exists(&self, key: &str) -> Result<bool, MediaStoreError> {
        Ok(self.head(key).await?.is_some())
    }
}

// ==================== InMemoryMediaStore ====================

/// In-memory media store for development and testing.
///
/// Uses a simple `RwLock<HashMap>` for storage. Not suitable for production
/// but works well for dev servers and tests where simplicity trumps performance.
pub struct InMemoryMediaStore {
    storage: RwLock<HashMap<String, StoredMedia>>,
}

/// Media stored in memory.
struct StoredMedia {
    data: Bytes,
    content_type: String,
}

impl InMemoryMediaStore {
    /// Create a new empty in-memory store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            storage: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryMediaStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl MediaStore for InMemoryMediaStore {
    async fn put_stream(
        &self,
        key: &str,
        mut reader: Pin<Box<dyn AsyncRead + Send>>,
        content_type: &str,
    ) -> Result<(), MediaStoreError> {
        // Read all data from the stream
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await?;

        // Store in memory
        let stored = StoredMedia {
            data: Bytes::from(buf),
            content_type: content_type.to_string(),
        };

        self.storage
            .write()
            .map_err(|e| MediaStoreError::LockPoisoned(e.to_string()))?
            .insert(key.to_string(), stored);

        Ok(())
    }

    async fn get_stream(
        &self,
        key: &str,
    ) -> Result<Option<Pin<Box<dyn AsyncRead + Send>>>, MediaStoreError> {
        let guard = self
            .storage
            .read()
            .map_err(|e| MediaStoreError::LockPoisoned(e.to_string()))?;

        match guard.get(key) {
            Some(stored) => {
                // Cursor<T> implements AsyncRead when T: AsRef<[u8]> + Unpin
                let cursor = Cursor::new(stored.data.clone());
                Ok(Some(Box::pin(cursor)))
            }
            None => Ok(None),
        }
    }

    async fn head(&self, key: &str) -> Result<Option<MediaMetadata>, MediaStoreError> {
        let guard = self
            .storage
            .read()
            .map_err(|e| MediaStoreError::LockPoisoned(e.to_string()))?;

        Ok(guard.get(key).map(|stored| MediaMetadata {
            content_type: stored.content_type.clone(),
            size: stored.data.len() as u64,
        }))
    }

    async fn get(&self, key: &str) -> Result<Option<MediaWithMetadata>, MediaStoreError> {
        let guard = self
            .storage
            .read()
            .map_err(|e| MediaStoreError::LockPoisoned(e.to_string()))?;

        Ok(guard.get(key).map(|stored| MediaWithMetadata {
            data: stored.data.clone(),
            metadata: MediaMetadata {
                content_type: stored.content_type.clone(),
                size: stored.data.len() as u64,
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[tokio::test]
    async fn test_put_and_get_bytes() -> TestResult {
        let store = InMemoryMediaStore::new();

        store
            .put("test.jpg", Bytes::from("image data"), "image/jpeg")
            .await?;

        let retrieved = store.get("test.jpg").await?.ok_or("should exist")?;
        assert_eq!(retrieved.data, Bytes::from("image data"));
        assert_eq!(retrieved.metadata.content_type, "image/jpeg");
        Ok(())
    }

    #[tokio::test]
    async fn test_get_nonexistent_returns_none() -> TestResult {
        let store = InMemoryMediaStore::new();

        let result = store.get("nonexistent").await?;
        assert!(result.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_head_returns_metadata() -> TestResult {
        let store = InMemoryMediaStore::new();
        let data = Bytes::from("test content here");

        store.put("test.txt", data.clone(), "text/plain").await?;

        let meta = store.head("test.txt").await?.ok_or("should exist")?;

        assert_eq!(meta.content_type, "text/plain");
        assert_eq!(meta.size, data.len() as u64);
        Ok(())
    }

    #[tokio::test]
    async fn test_head_nonexistent_returns_none() -> TestResult {
        let store = InMemoryMediaStore::new();

        let result = store.head("nonexistent").await?;
        assert!(result.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_overwrite_existing() -> TestResult {
        let store = InMemoryMediaStore::new();

        store
            .put("test.jpg", Bytes::from("original"), "image/jpeg")
            .await?;

        store
            .put("test.jpg", Bytes::from("updated"), "image/png")
            .await?;

        let result = store.get("test.jpg").await?.ok_or("should exist")?;
        assert_eq!(result.data, Bytes::from("updated"));
        assert_eq!(result.metadata.content_type, "image/png");
        Ok(())
    }

    #[tokio::test]
    async fn test_streaming_read() -> TestResult {
        let store = InMemoryMediaStore::new();
        let data = Bytes::from("streaming test data");

        store
            .put("stream.bin", data.clone(), "application/octet-stream")
            .await?;

        let mut reader = store
            .get_stream("stream.bin")
            .await?
            .ok_or("should exist")?;

        // Read in small chunks to test streaming
        let mut buf = [0u8; 4];
        let mut result = Vec::new();

        loop {
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            result.extend_from_slice(&buf[..n]);
        }

        assert_eq!(result, data.as_ref());
        Ok(())
    }
}
