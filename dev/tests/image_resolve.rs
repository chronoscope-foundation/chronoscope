//! The resolver must resolve every fact-store image that carries a source URL.
//!
//! (`resolve_fact_store_images` is a temporary bridge — see its module doc in
//! `dev/src/image_resolve.rs`; this test guards it only while it exists.)
//!
//! Ingests the curated Wikidata snapshot (via `WIKIDATA_ENTITIES_JSONL`, set by
//! the api/web dev shells; skipped when unset, like
//! `ingestion/tests/real_entity_ingest.rs`) and runs `resolve_fact_store_images`
//! in Placeholder mode — deterministic, no network. It then asserts the resolved
//! map covers exactly the images that carry a source URL, and in particular every
//! depicted image the read path will look up. This guards the enumeration /
//! projection / key-alignment that `resolve_fact_store_images` and the read path
//! must agree on: an under-enumeration or representative-key drift would drop the
//! count below the url-bearing total.

use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::sync::Arc;

use chronoscope_core::projection::{member_lineage, project_entity, project_image};
use chronoscope_core::store::memory::{MemoryFactStore, MemoryImageId};
use chronoscope_core::store::schema::{EntityStream, ImageStream};
use chronoscope_core::store::{EntityView, FactStore, ImageView};
use chronoscope_core::typed;
use chronoscope_db::media_store::{InMemoryMediaStore, MediaStore};
use chronoscope_dev::{ImageResolveMode, load_curated_fact_store, resolve_fact_store_images};
use chronoscope_workers::{HttpClient, ReqwestClient};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The curated store, or `None` when `WIKIDATA_ENTITIES_JSONL` is unset.
async fn curated() -> Result<Option<MemoryFactStore>, BoxError> {
    let Ok(path) = std::env::var("WIKIDATA_ENTITIES_JSONL") else {
        eprintln!("WIKIDATA_ENTITIES_JSONL unset — skipping resolver coverage test");
        return Ok(None);
    };
    let store = MemoryFactStore::new();
    load_curated_fact_store(&store, std::path::Path::new(&path)).await?;
    Ok(Some(store))
}

fn page() -> Result<NonZeroUsize, BoxError> {
    NonZeroUsize::new(1024).ok_or_else(|| "nonzero page size".into())
}

/// Every image representative in the store that projects to at least one source
/// URL — the set the resolver is expected to serve.
async fn image_reps_with_url(store: &MemoryFactStore) -> Result<BTreeSet<MemoryImageId>, BoxError> {
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let limit = page()?;

    let mut reps = BTreeSet::new();
    let mut after = None;
    loop {
        let p = view
            .walk_image_classes(&ImageStream::All, after, limit)
            .await
            .map_err(|e| format!("{e:?}"))?;
        for row in &p.rows {
            reps.insert(row.representative);
        }
        match p.next_class {
            None => break,
            Some(n) => after = Some(n),
        }
    }

    let mut with_url = BTreeSet::new();
    for rep in reps {
        let Some((class, projected)) =
            project_image::<MemoryFactStore, _, _>(&mut view, rep, member_lineage)
                .await
                .map_err(|e| format!("{e:?}"))?
        else {
            continue;
        };
        if !typed::Image::parse(&projected, &class).urls.is_empty() {
            with_url.insert(rep);
        }
    }
    Ok(with_url)
}

/// The `SameArtifact` representative of every depicted image across all
/// entities — the exact keys the marker/detail read path looks images up by.
async fn depicted_image_reps(store: &MemoryFactStore) -> Result<BTreeSet<MemoryImageId>, BoxError> {
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let limit = page()?;

    let mut reps = BTreeSet::new();
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
            for (image, _entry) in projected.depictions.iter() {
                let rep = view
                    .image_representative(image)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                reps.insert(rep);
            }
        }
        match p.next_class {
            None => break,
            Some(n) => after = Some(n),
        }
    }
    Ok(reps)
}

#[tokio::test]
async fn resolves_every_image_with_a_source_url() -> Result<(), BoxError> {
    let Some(store) = curated().await? else {
        return Ok(());
    };

    let expected = image_reps_with_url(&store).await?;
    assert!(
        !expected.is_empty(),
        "the curated snapshot carries source-bearing images"
    );

    let media_store: Arc<dyn MediaStore> = Arc::new(InMemoryMediaStore::new());
    // Placeholder mode never touches the client, but the signature wants one.
    let http_client: Arc<dyn HttpClient> = Arc::new(ReqwestClient::new()?);
    let resolved = resolve_fact_store_images(
        &store,
        &media_store,
        &http_client,
        ImageResolveMode::Placeholder,
    )
    .await;

    let resolved_keys: BTreeSet<MemoryImageId> = resolved.keys().copied().collect();
    assert_eq!(
        resolved_keys, expected,
        "resolver must cover exactly the images that carry a source URL"
    );

    // The user-facing guarantee: every image the read path can look up resolves.
    let depicted = depicted_image_reps(&store).await?;
    assert!(
        !depicted.is_empty(),
        "the curated snapshot depicts images on its entities"
    );
    let unresolved: Vec<_> = depicted.difference(&resolved_keys).collect();
    assert!(
        unresolved.is_empty(),
        "every depicted image with a source URL must resolve; unresolved: {unresolved:?}"
    );

    Ok(())
}
