//! The warm must store bytes under the dev media key `LocalCdn` builds its URLs
//! from, for every depicted image that carries a displayable source URL.
//!
//! (`warm_fact_store_media` is a temporary dev-only stand-in for the R2 mirror —
//! see its module doc in `dev/src/image_resolve.rs`; this test guards it only
//! while it exists.)
//!
//! Ingests the curated Wikidata snapshot (via `WIKIDATA_ENTITIES_JSONL`, set by
//! the api/web dev shells; skipped when unset, like
//! `ingestion/tests/real_entity_ingest.rs`) and runs `warm_fact_store_media` in
//! Placeholder mode — deterministic, no network. It then asserts the media store
//! holds a stored object under the `local_media_key` of every displayable source
//! URL of every depicted image — the exact keys the `/media/{key}` route serves.
//! This guards the enumeration / projection / key-alignment the warm and the read
//! path must agree on: an under-enumeration or a key-derivation drift would leave
//! a depicted image's key unwarmed and its tile broken.

use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::sync::Arc;

use chronoscope_core::projection::{member_lineage, project_entity, project_image};
use chronoscope_core::store::memory::MemoryFactStore;
use chronoscope_core::store::schema::EntityStream;
use chronoscope_core::store::{EntityView, FactStore};
use chronoscope_core::typed;
use chronoscope_db::media_store::{InMemoryMediaStore, MediaStore};
use chronoscope_dev::{ImageResolveMode, load_curated_fact_store, warm_fact_store_media};
use chronoscope_integrations::DisplayableKey;
use chronoscope_workers::{HttpClient, ReqwestClient};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The curated store, or `None` when `WIKIDATA_ENTITIES_JSONL` is unset.
async fn curated() -> Result<Option<MemoryFactStore>, BoxError> {
    let Ok(path) = std::env::var("WIKIDATA_ENTITIES_JSONL") else {
        eprintln!("WIKIDATA_ENTITIES_JSONL unset — skipping warm coverage test");
        return Ok(None);
    };
    let store = MemoryFactStore::new();
    load_curated_fact_store(&store, std::path::Path::new(&path)).await?;
    Ok(Some(store))
}

fn page() -> Result<NonZeroUsize, BoxError> {
    NonZeroUsize::new(1024).ok_or_else(|| "nonzero page size".into())
}

/// The dev media-store key of every displayable source URL of every depicted
/// image across all entities — the exact keys `LocalCdn` builds its URLs from,
/// and so the keys the warm must store bytes under.
async fn depicted_displayable_keys(store: &MemoryFactStore) -> Result<BTreeSet<String>, BoxError> {
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let limit = page()?;

    let mut keys = BTreeSet::new();
    let mut after = None;
    loop {
        let p = view
            .walk_entity_classes(&EntityStream::All, after, limit)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let mut last = None;
        for row in &p.rows {
            if last == Some(row.representative) {
                continue;
            }
            last = Some(row.representative);
            let Some((_class, projected)) = project_entity::<MemoryFactStore, _, _>(
                &mut view,
                row.representative,
                member_lineage,
            )
            .await
            .map_err(|e| format!("{e:?}"))?
            else {
                continue;
            };
            let depicted: Vec<_> = projected.depictions.keys().copied().collect();
            for image in depicted {
                let Some((class, img_proj)) =
                    project_image::<MemoryFactStore, _, _>(&mut view, image, member_lineage)
                        .await
                        .map_err(|e| format!("{e:?}"))?
                else {
                    continue;
                };
                let typed = typed::Image::parse(&img_proj, &class);
                for attributed in &typed.urls {
                    if let Some(key) = DisplayableKey::for_url(&attributed.value)? {
                        keys.insert(chronoscope_api::cdn::local_media_key(&key));
                    }
                }
            }
        }
        match p.next_class {
            None => break,
            Some(n) => after = Some(n),
        }
    }
    Ok(keys)
}

#[tokio::test]
async fn warms_the_media_key_of_every_depicted_displayable_image() -> Result<(), BoxError> {
    let Some(store) = curated().await? else {
        return Ok(());
    };

    let expected = depicted_displayable_keys(&store).await?;
    assert!(
        !expected.is_empty(),
        "the curated snapshot depicts images with displayable sources"
    );

    let media_store: Arc<dyn MediaStore> = Arc::new(InMemoryMediaStore::new());
    // Placeholder mode never touches the client, but the signature wants one.
    let http_client: Arc<dyn HttpClient> = Arc::new(ReqwestClient::new()?);
    let warmed = warm_fact_store_media(
        &store,
        &media_store,
        &http_client,
        ImageResolveMode::Placeholder,
    )
    .await;
    assert!(warmed > 0, "the warm stored at least one key");

    // Every key the read path can request for a depicted image is warmed, so no
    // depicted tile can 404 against the store — the enumeration/key-alignment the
    // warm and the read path must agree on.
    let mut unwarmed = Vec::new();
    for key in &expected {
        if media_store
            .get(key)
            .await
            .map_err(|e| format!("{e:?}"))?
            .is_none()
        {
            unwarmed.push(key.clone());
        }
    }
    assert!(
        unwarmed.is_empty(),
        "every depicted displayable image's media key must be warmed; unwarmed: {unwarmed:?}"
    );

    Ok(())
}
