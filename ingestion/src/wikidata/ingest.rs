//! Main ingestion orchestration.
//!
//! Entity processing and result merging for the Wikidata ingestion pipeline.

use anyhow::{Context, Result};
use chronoscope_core::{
    Annotation, AnnotationKind, EntityIdx, EntityRelation, EntityRelationType, EntityType,
    Evidence, ExternalLink, ImageSource, IngestionOutput, LinkIdx, LinkTarget, LinkType, SourceIdx,
    WikidataEntityId, WikidataPropertyId,
};
use chronoscope_integrations::wikidata::WikidataEntity;
use futures::stream::{self, StreamExt};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::AsyncBufReadExt;

use crate::wikidata::handlers::PROPERTY_HANDLERS;
use crate::wikidata::lifecycle::build_lifecycles;
use crate::wikidata::parsing::{extract_names, parse_sitelink};
use crate::wikidata::usage;
use chronoscope_integrations::wikidata::{CommonsFilename, WikidataId, url_for_filename};

// Re-export for use by handlers
pub use self::entity_accumulator::*;

mod entity_accumulator {
    use std::collections::BTreeSet;

    use chronoscope_core::{
        AnnotationKind, Cited, Entity, EntityName, EntityTransition, EntityType, Evidence,
        ExternalLink, ImageSource, Usage, WikidataEntityId, WikidataPropertyId,
    };
    use chronoscope_integrations::wikidata::{RevisionId, WikidataId};

    /// Result of processing a single entity, with local indices.
    #[derive(Clone)]
    pub struct EntityResult {
        pub entity_idx: usize,
        pub wikidata_id: WikidataId,
        pub revision_id: RevisionId,
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
        wikidata_id: WikidataId,
        revision_id: RevisionId,
        entity_type: EntityType,
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
            wikidata_id: WikidataId,
            revision_id: RevisionId,
            entity_type: EntityType,
        ) -> Self {
            Self {
                entity_idx,
                wikidata_id,
                revision_id,
                entity_type,
                images: Vec::new(),
                annotations: Vec::new(),
                links: Vec::new(),
                issues: Vec::new(),
            }
        }

        /// Get the Wikidata entity ID.
        #[must_use]
        pub fn wikidata_id(&self) -> &WikidataId {
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
                wikidata_id: self.wikidata_id.as_str(),
                revision_id: self.revision_id.0,
                property,
            }
        }

        /// Merge handler output into the accumulator, tagging issues with property.
        pub fn merge_output(&mut self, property: &str, output: HandlerOutput) {
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

        /// Convert to final result (single entity, no splitting).
        #[must_use]
        pub fn into_result(self, names: Vec<Cited<EntityName>>) -> EntityResult {
            EntityResult {
                entity_idx: self.entity_idx,
                wikidata_id: self.wikidata_id,
                revision_id: self.revision_id,
                entity: Entity {
                    entity_type: self.entity_type,
                    names,
                    transitions: Vec::new(),
                },
                images: self.images,
                annotations: self.annotations,
                links: self.links,
                issues: self.issues,
            }
        }

        /// Split into multiple results, one per lifecycle.
        ///
        /// Each lifecycle gets its own `Entity` with the given transitions.
        /// Shared state (images, annotations, links, issues, names) is cloned
        /// into each result.
        pub fn split_into_results(
            self,
            names: Vec<Cited<EntityName>>,
            mut lifecycles: Vec<Vec<EntityTransition>>,
            inferred_usages: &BTreeSet<Usage>,
        ) -> Vec<EntityResult> {
            if lifecycles.is_empty() {
                lifecycles.push(Vec::new());
            }

            let mut results = Vec::with_capacity(lifecycles.len());

            for transitions in lifecycles {
                let mut entity = Entity {
                    entity_type: self.entity_type,
                    names: names.clone(),
                    transitions,
                };
                crate::wikidata::usage::replace_unknown(&mut entity, inferred_usages);

                results.push(EntityResult {
                    entity_idx: self.entity_idx,
                    wikidata_id: self.wikidata_id.clone(),
                    revision_id: self.revision_id,
                    entity,
                    images: self.images.clone(),
                    annotations: self.annotations.clone(),
                    links: self.links.clone(),
                    issues: self.issues.clone(),
                });
            }

            results
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

/// How often to print progress (every N entities processed).
const PROGRESS_INTERVAL: usize = 1000;

// =============================================================================
// MAIN ENTRY POINT
// =============================================================================

/// Run the full ingestion process.
///
/// `gallery_media` provides pre-resolved Commons gallery filenames keyed by
/// gallery name. Pass an empty map if galleries are not available (e.g., when
/// processing dump-filtered JSONL without a prior resolve step).
///
/// # Errors
/// Returns an error if the input file cannot be read, entity processing fails,
/// or the output file cannot be written.
pub async fn run(
    config: &Config,
    gallery_media: &HashMap<String, Vec<CommonsFilename>>,
) -> Result<()> {
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
                        match serde_json::from_str::<WikidataEntity>(&line) {
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

    // Process entities as they stream in. Entity processing is now a pure
    // transform (no network calls) since gallery media is pre-resolved.
    let mut processing_stream = entity_stream
        .map(|(entity_idx, wd_entity)| process_entity(entity_idx, wd_entity, gallery_media))
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

    // Report issues summary (before merge, which only needs sorted order)
    report_issues(&results);

    // Sort by source entity_idx for deterministic output, then merge
    results.sort_by_key(|v| v.first().map(|r| r.entity_idx).unwrap_or(0));
    let output = merge_results(&results);

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
// RESULT MERGING
// =============================================================================

/// Merge per-entity results into a single `IngestionOutput`.
///
/// Results must be sorted by `entity_idx` before calling (for deterministic output).
fn merge_results(results: &[Vec<EntityResult>]) -> IngestionOutput {
    let mut output = IngestionOutput::new();
    let mut next_entity: usize = 0;
    let mut next_source: usize = 0;
    let mut next_link: usize = 0;

    for result_group in results {
        // Track entity keys for this group (for creating Replaces relationships)
        let mut group_entity_keys: Vec<EntityIdx> = Vec::new();

        for result in result_group {
            let entity_key = EntityIdx::new(next_entity);
            next_entity += 1;
            group_entity_keys.push(entity_key);

            let source_base = next_source;
            let link_base = next_link;

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

            // Track entity->link mappings
            if link_base < next_link {
                let link_keys: Vec<LinkIdx> = (link_base..next_link).map(LinkIdx::new).collect();
                output.entity_links.insert(entity_key, link_keys);
            }
        }

        // Create Replaces relationships for split entities
        // Entities are ordered chronologically, so each replaces the previous
        if group_entity_keys.len() > 1 {
            let wikidata_id = &result_group[0].wikidata_id;
            let revision_id = result_group[0].revision_id.0;

            for window in group_entity_keys.windows(2) {
                let older_key = window[0];
                let newer_key = window[1];
                output.entity_relations.push(EntityRelation {
                    from_entity: newer_key,
                    to_entity: older_key,
                    relation_type: EntityRelationType::Replaces,
                    evidence: vec![Evidence::Wikidata {
                        entity_id: WikidataEntityId(wikidata_id.to_string()),
                        property_id: WikidataPropertyId("lifecycle".to_string()),
                        property_value: "demolish->rebuild pattern".to_string(),
                        revision_id,
                    }],
                });
            }
        }
    }

    output
}

/// Print a summary of issues from processing.
fn report_issues(results: &[Vec<EntityResult>]) {
    let mut issues_by_property: HashMap<String, Vec<(WikidataId, String)>> = HashMap::new();

    for result_group in results {
        for result in result_group {
            for (property, msg) in &result.issues {
                issues_by_property
                    .entry(property.clone())
                    .or_default()
                    .push((result.wikidata_id.clone(), msg.clone()));
            }
        }
    }

    if issues_by_property.is_empty() {
        return;
    }

    let total_issues: usize = issues_by_property.values().map(|v| v.len()).sum();
    eprintln!();
    eprintln!("=== Processing Issues ({total_issues} total) ===");

    // Sort properties by issue count (descending)
    let mut sorted_properties: Vec<_> = issues_by_property.iter().collect();
    sorted_properties.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    for (property, issues) in sorted_properties {
        eprintln!();
        eprintln!("{} ({} issues):", property, issues.len());
        let mut msgs_with_entities: HashMap<&str, Vec<&str>> = HashMap::new();
        for (entity_id, msg) in issues {
            msgs_with_entities
                .entry(msg.as_str())
                .or_default()
                .push(entity_id.as_str());
        }
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
                "  {} (x{}) [{}{}]",
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

// =============================================================================
// ENTITY PROCESSING
// =============================================================================

/// Process a single Wikidata entity into Chronoscope domain types.
///
/// `gallery_media` provides pre-resolved Commons gallery filenames (from the
/// CLI resolve step or `get_entities_at_timestamp`). Entities whose galleries
/// are not in this map simply skip gallery images.
///
/// Returns a Vec because a single Wikidata entity may produce multiple
/// Chronoscope entities when demolish->rebuild patterns are detected.
fn process_entity(
    entity_idx: usize,
    wd_entity: WikidataEntity,
    gallery_media: &HashMap<String, Vec<CommonsFilename>>,
) -> Vec<EntityResult> {
    let wikidata_id = &wd_entity.id;
    let revision_id = wd_entity.lastrevid;

    let names = extract_names(&wd_entity, wikidata_id.as_str(), revision_id.0);

    let mut acc = EntityAccumulator::new(
        entity_idx,
        wikidata_id.clone(),
        revision_id,
        EntityType::Building,
    );

    // Build lifecycle transitions
    let lifecycle_ctx = PropertyContext::new(wikidata_id.as_str(), revision_id.0, "lifecycle");
    let (entity_lifecycles, lifecycle_warnings) =
        build_lifecycles(&wd_entity.claims, &lifecycle_ctx);

    for warning in lifecycle_warnings {
        acc.add_issue("lifecycle", warning);
    }

    // Run property handlers for non-lifecycle claims
    for (property, claims) in &wd_entity.claims {
        if let Some(handler) = PROPERTY_HANDLERS.get(property.as_str()) {
            let ctx = PropertyContext::new(wikidata_id.as_str(), revision_id.0, property.as_str());
            match handler(claims, &ctx) {
                Ok(output) => acc.merge_output(property.as_str(), output),
                Err(e) => acc.add_issue(property.as_str(), format!("handler failed: {e}")),
            }
        }
    }

    // P935 (Commons gallery): use pre-resolved gallery media if available.
    // Gallery data is resolved by the CLI's resolve step (or get_entities_at_timestamp)
    // and passed in via the gallery_media map. Entities without pre-resolved galleries
    // (e.g., from dump filtering) simply skip gallery images.
    if let Some(gallery_claims) = wd_entity.claims.get("P935") {
        for claim in gallery_claims {
            if let Some(gallery_name) = claim.mainsnak.string_value()
                && let Some(filenames) = gallery_media.get(gallery_name)
            {
                for filename in filenames {
                    let url = url_for_filename(filename);
                    acc.add_image(
                        ImageSource {
                            url,
                            date: None,
                            location: None,
                        },
                        AnnotationKind::ExteriorView { region: None },
                    );
                }
            }
        }
    }

    // Add Wikidata link
    acc.add_link(ExternalLink {
        target: LinkTarget::Wikidata {
            entity_id: WikidataEntityId(wikidata_id.to_string()),
        },
        link_type: LinkType::SameAs,
    });

    // Add sitelinks (Wikipedia, Commons)
    for (site, sitelink) in &wd_entity.sitelinks {
        if let Some(link) = parse_sitelink(site.as_str(), sitelink.title.as_str()) {
            acc.add_link(link);
        }
    }

    // Infer usages and split into one EntityResult per lifecycle
    let inferred_usages = usage::infer(&wd_entity);
    acc.split_into_results(names, entity_lifecycles, &inferred_usages)
}
