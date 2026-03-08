//! Main ingestion orchestration.
//!
//! Concurrent entity processing with rate-limited external fetches.

use anyhow::{Context, Result};
use chronoscope_core::{
    Annotation, AnnotationKind, Entity, EntityIdx, EntityRelation, EntityRelationType, EntityType,
    Evidence, ExternalLink, ImageSource, IngestionOutput, LinkIdx, LinkTarget, LinkType, SourceIdx,
    WikidataEntityId, WikidataPropertyId,
};
use futures::stream::{self, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::AsyncBufReadExt;

use crate::wikidata::commons::{RateLimitedClient, url_for_filename};
use crate::wikidata::handlers::PROPERTY_HANDLERS;
use crate::wikidata::lifecycle::build_lifecycles;
use crate::wikidata::parsing::{extract_names, get_claim_str, get_claims, parse_sitelink};
use crate::wikidata::usage;

// Re-export for use by handlers
pub use self::entity_accumulator::*;

mod entity_accumulator {
    use chronoscope_core::{
        AnnotationKind, Cited, Entity, EntityTransition, Evidence, ExternalLink, ImageSource,
        WikidataEntityId, WikidataPropertyId,
    };

    /// Result of processing a single entity, with local indices.
    #[derive(Clone)]
    pub struct EntityResult {
        pub entity_idx: usize,
        pub wikidata_id: String,
        pub revision_id: u64,
        pub entity: Entity,
        pub images: Vec<ImageSource>,
        pub annotations: Vec<LocalAnnotation>,
        pub links: Vec<ExternalLink>,
        pub issues: Vec<(String, String)>,
    }

    /// Annotation with local source index (remapped during merge).
    #[derive(Clone)]
    pub struct LocalAnnotation {
        pub local_source_idx: usize,
        pub kind: AnnotationKind,
    }

    /// Accumulator for building an `EntityResult` during entity processing.
    ///
    /// Holds entity-level state that persists across all property handlers.
    pub struct EntityAccumulator {
        entity_idx: usize,
        wikidata_id: String,
        revision_id: u64,
        pub entity: Entity,
        images: Vec<ImageSource>,
        annotations: Vec<LocalAnnotation>,
        links: Vec<ExternalLink>,
        issues: Vec<(String, String)>,
    }

    impl EntityAccumulator {
        /// Create a new accumulator for an entity.
        #[must_use]
        pub fn new(
            entity_idx: usize,
            wikidata_id: String,
            revision_id: u64,
            entity: Entity,
        ) -> Self {
            Self {
                entity_idx,
                wikidata_id,
                revision_id,
                entity,
                images: Vec::new(),
                annotations: Vec::new(),
                links: Vec::new(),
                issues: Vec::new(),
            }
        }

        /// Get the Wikidata entity ID.
        #[must_use]
        pub fn wikidata_id(&self) -> &str {
            &self.wikidata_id
        }

        /// Add an image with an annotation linking it to this entity.
        pub fn add_image(&mut self, image: ImageSource, kind: AnnotationKind) {
            let local_idx = self.images.len();
            self.images.push(image);
            self.annotations.push(LocalAnnotation {
                local_source_idx: local_idx,
                kind,
            });
        }

        /// Add an external link for this entity.
        pub fn add_link(&mut self, link: ExternalLink) {
            self.links.push(link);
        }

        /// Create a context for a specific property handler.
        #[must_use]
        pub fn property_context<'a>(&'a self, property: &'a str) -> PropertyContext<'a> {
            PropertyContext {
                wikidata_id: &self.wikidata_id,
                revision_id: self.revision_id,
                property,
            }
        }

        /// Merge handler output into the accumulator, tagging issues with property.
        pub fn merge_output(&mut self, property: &str, output: HandlerOutput) {
            self.entity.transitions.extend(output.transitions);
            for (img, kind) in output.images {
                self.add_image(img, kind);
            }
            self.links.extend(output.links);
            for msg in output.issues {
                self.issues.push((property.to_string(), msg));
            }
        }

        /// Record an issue for a property.
        pub fn add_issue(&mut self, property: &str, msg: impl Into<String>) {
            self.issues.push((property.to_string(), msg.into()));
        }

        /// Convert to final result.
        #[must_use]
        pub fn into_result(self) -> EntityResult {
            EntityResult {
                entity_idx: self.entity_idx,
                wikidata_id: self.wikidata_id,
                revision_id: self.revision_id,
                entity: self.entity,
                images: self.images,
                annotations: self.annotations,
                links: self.links,
                issues: self.issues,
            }
        }
    }

    /// Immutable context for a property handler.
    ///
    /// Contains everything a handler needs to create citations.
    pub struct PropertyContext<'a> {
        wikidata_id: &'a str,
        revision_id: u64,
        property: &'a str,
    }

    impl<'a> PropertyContext<'a> {
        /// Create a new `PropertyContext`.
        #[must_use]
        pub fn new(wikidata_id: &'a str, revision_id: u64, property: &'a str) -> Self {
            Self {
                wikidata_id,
                revision_id,
                property,
            }
        }

        /// Create a cited value with Wikidata evidence for this property.
        #[must_use]
        pub fn cited<T>(&self, raw: impl Into<String>, value: T) -> Cited<T> {
            Cited::new(
                value,
                vec![Evidence::Wikidata {
                    entity_id: WikidataEntityId(self.wikidata_id.to_string()),
                    property_id: WikidataPropertyId(self.property.to_string()),
                    property_value: raw.into(),
                    revision_id: self.revision_id,
                }],
            )
        }

        /// Get the current property ID.
        #[must_use]
        pub fn property(&self) -> &str {
            self.property
        }

        /// Get the Wikidata entity ID.
        #[must_use]
        pub fn wikidata_id(&self) -> &str {
            self.wikidata_id
        }
    }

    /// Output from a property handler.
    ///
    /// Handlers are pure functions that return what they want to add.
    #[derive(Default)]
    pub struct HandlerOutput {
        pub transitions: Vec<EntityTransition>,
        pub images: Vec<(ImageSource, AnnotationKind)>,
        pub links: Vec<ExternalLink>,
        pub issues: Vec<String>,
    }

    impl HandlerOutput {
        /// Create an empty output.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Add a transition.
        pub fn add_transition(&mut self, t: EntityTransition) {
            self.transitions.push(t);
        }

        /// Add an image with annotation kind.
        pub fn add_image(&mut self, image: ImageSource, kind: AnnotationKind) {
            self.images.push((image, kind));
        }

        /// Add an external link.
        pub fn add_link(&mut self, link: ExternalLink) {
            self.links.push(link);
        }

        /// Record an issue.
        pub fn issue(&mut self, msg: impl Into<String>) {
            self.issues.push(msg.into());
        }
    }
}

// =============================================================================
// CONFIGURATION
// =============================================================================

/// Configuration for the Wikidata ingestion process.
pub struct Config {
    pub input_path: String,
    pub output_path: String,
    pub verbose: bool,
}

// =============================================================================
// CONSTANTS
// =============================================================================

/// Buffer size for file I/O (8 MB).
const BUFFER_SIZE: usize = 8 * 1024 * 1024;

/// Concurrency for entity processing.
///
/// Limits concurrent `process_entity` tasks to bound memory usage. Each task
/// may fetch gallery images from Commons, so this also caps outbound API requests.
const ENTITY_CONCURRENCY: usize = 100;

/// How often to print progress (every N entities processed).
const PROGRESS_INTERVAL: usize = 1000;

// =============================================================================
// MAIN ENTRY POINT
// =============================================================================

/// Run the full ingestion process.
///
/// # Errors
/// Returns an error if the input file cannot be read, entity processing fails,
/// or the output file cannot be written.
pub async fn run(config: &Config) -> Result<()> {
    let client = RateLimitedClient::with_defaults()?;
    let verbose = config.verbose;

    eprintln!("Processing entities...");

    // Async stream of parsed entities from the input file.
    // Skips blank lines and unparseable JSON, counting parse errors.
    let file = tokio::fs::File::open(&config.input_path)
        .await
        .context("Failed to open input file")?;
    let reader = tokio::io::BufReader::with_capacity(BUFFER_SIZE, file);

    let parse_errors = Arc::new(AtomicU64::new(0));
    let errors_for_stream = parse_errors.clone();

    let entity_stream = stream::unfold(
        (reader.lines(), 0usize, errors_for_stream),
        |(mut lines, mut entity_idx, parse_errors)| async move {
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Value>(&line) {
                            Ok(entity) => {
                                let idx = entity_idx;
                                entity_idx += 1;
                                return Some(((idx, entity), (lines, entity_idx, parse_errors)));
                            }
                            Err(_) => {
                                parse_errors.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                        }
                    }
                    Ok(None) => return None,
                    Err(e) => {
                        eprintln!("I/O error reading input: {e}");
                        return None;
                    }
                }
            }
        },
    );

    // Process entities concurrently as they stream in.
    // `.boxed()` pins the stream since `unfold`'s async closure is `!Unpin`.
    let mut processing_stream = entity_stream
        .map(|(entity_idx, wd_entity)| {
            let client = client.clone();
            async move { process_entity(entity_idx, wd_entity, &client).await }
        })
        .buffer_unordered(ENTITY_CONCURRENCY)
        .boxed();

    let mut results: Vec<Vec<EntityResult>> = Vec::new();
    let mut processed: usize = 0;

    while let Some(result) = processing_stream.next().await {
        if !result.is_empty() {
            results.push(result);
        }
        processed += 1;
        if verbose && processed.is_multiple_of(PROGRESS_INTERVAL) {
            eprint!("\r  Processed {processed} entities...");
        }
    }

    let errors = parse_errors.load(Ordering::Relaxed);
    if verbose {
        // Clear progress line if we printed any, then print final summary
        if processed >= PROGRESS_INTERVAL {
            eprint!("\r");
        }
        eprintln!("  Processed {processed} entities ({errors} parse errors)");
        eprintln!("Merging results...");
    }

    // Merge results into IngestionOutput
    let mut output = IngestionOutput::new();
    let mut next_entity: usize = 0;
    let mut next_source: usize = 0;
    let mut next_link: usize = 0;

    // Sort by source entity_idx to maintain deterministic output order
    results.sort_by_key(|v| v.first().map(|r| r.entity_idx).unwrap_or(0));

    // Track issues by property: property -> [(wikidata_id, message)]
    let mut issues_by_property: HashMap<String, Vec<(String, String)>> = HashMap::new();

    for result_group in &results {
        // Track entity keys for this group (for creating Replaces relationships)
        let mut group_entity_keys: Vec<EntityIdx> = Vec::new();

        for result in result_group {
            let entity_key = EntityIdx::new(next_entity);
            next_entity += 1;
            group_entity_keys.push(entity_key);

            let source_base = next_source;
            let link_base = next_link;

            // Collect issues
            for (property, msg) in &result.issues {
                issues_by_property
                    .entry(property.clone())
                    .or_default()
                    .push((result.wikidata_id.clone(), msg.clone()));
            }

            output.entities.insert(entity_key, result.entity.clone());

            for image in &result.images {
                output
                    .images
                    .insert(SourceIdx::new(next_source), image.clone());
                next_source += 1;
            }

            for link in &result.links {
                output
                    .external_links
                    .insert(LinkIdx::new(next_link), link.clone());
                next_link += 1;
            }

            // Remap annotations with global keys
            for ann in &result.annotations {
                output.annotations.push(Annotation {
                    source: SourceIdx::new(source_base + ann.local_source_idx),
                    entity: entity_key,
                    kind: ann.kind.clone(),
                });
            }

            // Track entity→link mappings
            if link_base < next_link {
                let link_keys: Vec<LinkIdx> = (link_base..next_link).map(LinkIdx::new).collect();
                output.entity_links.insert(entity_key, link_keys);
            }
        }

        // Create Replaces relationships for split entities
        // Entities are ordered chronologically, so each replaces the previous
        if group_entity_keys.len() > 1 {
            let wikidata_id = &result_group[0].wikidata_id;
            let revision_id = result_group[0].revision_id;

            for window in group_entity_keys.windows(2) {
                let older_key = window[0];
                let newer_key = window[1];
                output.entity_relations.push(EntityRelation {
                    from_entity: newer_key,
                    to_entity: older_key,
                    relation_type: EntityRelationType::Replaces,
                    evidence: vec![Evidence::Wikidata {
                        entity_id: WikidataEntityId(wikidata_id.clone()),
                        property_id: WikidataPropertyId("lifecycle".to_string()),
                        property_value: "demolish→rebuild pattern".to_string(),
                        revision_id,
                    }],
                });
            }
        }
    }

    // Report issues summary
    if !issues_by_property.is_empty() {
        let total_issues: usize = issues_by_property.values().map(|v| v.len()).sum();
        eprintln!();
        eprintln!("=== Processing Issues ({} total) ===", total_issues);

        // Sort properties by issue count (descending)
        let mut sorted_properties: Vec<_> = issues_by_property.iter().collect();
        sorted_properties.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

        for (property, issues) in sorted_properties {
            eprintln!();
            eprintln!("{} ({} issues):", property, issues.len());
            // Group by message, tracking which entities had each issue
            let mut msgs_with_entities: HashMap<&str, Vec<&str>> = HashMap::new();
            for (entity_id, msg) in issues {
                msgs_with_entities
                    .entry(msg.as_str())
                    .or_default()
                    .push(entity_id.as_str());
            }
            // Sort by count descending
            let mut sorted_msgs: Vec<_> = msgs_with_entities.into_iter().collect();
            sorted_msgs.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
            for (msg, entities) in sorted_msgs.iter().take(5) {
                let entity_sample: Vec<_> = entities.iter().take(3).copied().collect();
                let more = if entities.len() > 3 {
                    format!(" +{} more", entities.len() - 3)
                } else {
                    String::new()
                };
                eprintln!(
                    "  {} (×{}) [{}{}]",
                    msg,
                    entities.len(),
                    entity_sample.join(", "),
                    more
                );
            }
            if sorted_msgs.len() > 5 {
                eprintln!("  ... and {} more unique messages", sorted_msgs.len() - 5);
            }
        }
    }

    // Validate referential integrity
    if let Err(errors) = output.validate_references() {
        for error in &errors {
            eprintln!("  {error}");
        }
        anyhow::bail!(
            "Referential integrity errors in ingestion output ({} errors)",
            errors.len()
        );
    }

    // Write output
    if verbose {
        eprintln!("Writing to {}...", config.output_path);
    }
    let out_file = File::create(&config.output_path)?;
    let mut writer = BufWriter::new(out_file);
    serde_json::to_writer(&mut writer, &output)?;
    writer.flush()?;

    if verbose {
        eprintln!();
        eprintln!("Done!");
        eprintln!("  Output entities: {}", output.entities.len());
        eprintln!("  Output images: {}", output.images.len());
        eprintln!("  Output links: {}", output.external_links.len());
        eprintln!("  Annotations: {}", output.annotations.len());
    }

    Ok(())
}

// =============================================================================
// ENTITY PROCESSING
// =============================================================================

/// Process a single Wikidata entity, fetching any external resources.
///
/// Returns a Vec because a single Wikidata entity may produce multiple
/// Chronoscope entities when demolish→rebuild patterns are detected.
async fn process_entity(
    entity_idx: usize,
    wd_entity: Value,
    client: &RateLimitedClient,
) -> Vec<EntityResult> {
    let Some(wikidata_id) = wd_entity.get("id").and_then(|v| v.as_str()) else {
        return vec![];
    };
    let revision_id = wd_entity
        .get("lastrevid")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let mut names = extract_names(&wd_entity, wikidata_id, revision_id);

    // Collect shared data (images, links, issues) that applies to all split entities
    let mut images: Vec<ImageSource> = Vec::new();
    let mut annotations: Vec<LocalAnnotation> = Vec::new();
    let mut links: Vec<ExternalLink> = Vec::new();
    let mut issues: Vec<(String, String)> = Vec::new();

    // Helper to add an image with annotation
    let mut add_image = |image: ImageSource, kind: AnnotationKind| {
        let local_idx = images.len();
        images.push(image);
        annotations.push(LocalAnnotation {
            local_source_idx: local_idx,
            kind,
        });
    };

    // Build lifecycle transitions
    let mut entity_lifecycles: Vec<Vec<chronoscope_core::EntityTransition>> = Vec::new();

    if let Some(claims_obj) = wd_entity.get("claims").and_then(|c| c.as_object()) {
        let lifecycle_ctx = PropertyContext::new(wikidata_id, revision_id, "lifecycle");
        let (lifecycles, lifecycle_warnings) = build_lifecycles(claims_obj, &lifecycle_ctx);
        entity_lifecycles = lifecycles;

        for warning in lifecycle_warnings {
            issues.push(("lifecycle".to_string(), warning));
        }

        // Run property handlers for non-lifecycle claims
        for (property, claims) in claims_obj {
            if let Some(handler) = PROPERTY_HANDLERS.get(property.as_str())
                && let Some(claims_arr) = claims.as_array()
            {
                let ctx = PropertyContext::new(wikidata_id, revision_id, property);
                match handler(claims_arr, &ctx) {
                    Ok(output) => {
                        // Images and links are shared across all split entities
                        for (img, kind) in output.images {
                            add_image(img, kind);
                        }
                        links.extend(output.links);
                        for msg in output.issues {
                            issues.push((property.to_string(), msg));
                        }
                        // Note: transitions from handlers are currently not used
                        // (lifecycle handles all transitions)
                    }
                    Err(e) => {
                        issues.push((property.to_string(), format!("handler failed: {e}")));
                    }
                }
            }
        }
    }

    // Fetch gallery images (P935) - this is the async part
    if let Some(claims) = get_claims(&wd_entity, "P935") {
        for claim in claims {
            if let Some(gallery_name) = get_claim_str(claim) {
                match client.fetch_gallery(gallery_name).await {
                    Ok(filenames) => {
                        for filename in filenames {
                            match url_for_filename(&filename) {
                                Ok(url) => {
                                    add_image(
                                        ImageSource {
                                            url,
                                            date: None,
                                            location: None,
                                        },
                                        AnnotationKind::ExteriorView { region: None },
                                    );
                                }
                                Err(e) => {
                                    issues.push((
                                        "P935".to_string(),
                                        format!("invalid URL for '{filename}': {e}"),
                                    ));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        issues.push((
                            "P935".to_string(),
                            format!("gallery fetch failed for '{gallery_name}': {e}"),
                        ));
                    }
                }
            }
        }
    }

    // Add Wikidata link
    links.push(ExternalLink {
        target: LinkTarget::Wikidata {
            entity_id: WikidataEntityId(wikidata_id.to_string()),
        },
        link_type: LinkType::SameAs,
    });

    // Add sitelinks (Wikipedia, Commons)
    if let Some(sitelinks) = wd_entity.get("sitelinks").and_then(|s| s.as_object()) {
        for (site, link_obj) in sitelinks {
            if let Some(title) = link_obj.get("title").and_then(|t| t.as_str())
                && let Some(link) = parse_sitelink(site, title)
            {
                links.push(link);
            }
        }
    }

    // Infer usages for post-processing
    let inferred_usages = usage::infer(&wd_entity);

    // If no lifecycles, create one empty entity
    if entity_lifecycles.is_empty() {
        entity_lifecycles.push(Vec::new());
    }

    // Create an EntityResult for each lifecycle.
    // Clone shared data for all but the last, then move into the final one.
    let lifecycle_count = entity_lifecycles.len();
    let mut results = Vec::with_capacity(lifecycle_count);

    for (i, transitions) in entity_lifecycles.into_iter().enumerate() {
        let last = i + 1 == lifecycle_count;

        let mut entity = Entity {
            entity_type: EntityType::Building,
            names: if last {
                std::mem::take(&mut names)
            } else {
                names.clone()
            },
            transitions,
        };
        usage::replace_unknown(&mut entity, &inferred_usages);

        results.push(EntityResult {
            entity_idx,
            wikidata_id: wikidata_id.to_string(),
            revision_id,
            entity,
            images: if last {
                std::mem::take(&mut images)
            } else {
                images.clone()
            },
            annotations: if last {
                std::mem::take(&mut annotations)
            } else {
                annotations.clone()
            },
            links: if last {
                std::mem::take(&mut links)
            } else {
                links.clone()
            },
            issues: if last {
                std::mem::take(&mut issues)
            } else {
                issues.clone()
            },
        });
    }

    results
}
