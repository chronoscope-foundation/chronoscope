//! The chained pipeline against the corpus manifest's own ground truth.
//!
//! The stages are each checked against something outside themselves already:
//! the models against their reference fixtures, the geometry against oracles.
//! What no other test covers is the chaining, where a panel's rect reaches the
//! crop, the gate's verdict picks a route, and the segmenter's regions reach the
//! describe loop. A miswiring there reads as a plausible answer about the wrong
//! pixels, so this asserts the two counts the manifest states: how many panels
//! an image splits into, and how many regions survive within them.
//!
//! Not the descriptions. Even at a pinned decode they are brittle against a
//! model swap, and the corpus states no expected wording.
//!
//! Two entries rather than the whole corpus: running every image through three
//! models is the eval's job, and it is a Nix derivation for that reason. These
//! two cover both shapes, a single frame and a composite whose panels are
//! counted separately.
//!
//! Ignored by default: it needs the three model exports and the fetched corpus,
//! which the commit gate cannot realize.

use std::env;
use std::path::{Path, PathBuf};

use ab_glyph::FontVec;

use chronoscope_analysis::corpus::load_image;
use chronoscope_analysis::corpus::manifest::{CorpusManifest, ImageEntry};
use chronoscope_analysis::dinov3::Dinov3;
use chronoscope_analysis::onnx::Accel;
use chronoscope_analysis::pipeline::{Analysis, Content, Medium, Pipeline, Reading};
use chronoscope_analysis::qwen3::{MODEL_SHARD_ENV, Qwen3};
use chronoscope_analysis::sam3::Sam3;
use chronoscope_analysis::setofmark::font_from_env;
use chronoscope_analysis::test_support::{coreml_cache_root, describe};

/// The manifest, as JSON. `just model-test` inherits it from the analysis shell.
const MANIFEST_ENV: &str = "CORPUS_MANIFEST";

/// The fetched corpus, a directory of images named by entry id.
const IMAGES_ENV: &str = "CORPUS_IMAGES";

/// The SAM 3 export directory.
const SAM3_EXPORT_ENV: &str = "SAM3_EXPORT";

/// The DINOv3 export directory, one resolution per export.
const DINOV3_EXPORT_ENV: &str = "DINOV3_EXPORT";

/// The entries read, and why these two: a single frame whose one structure is
/// counted, and a before-and-after composite whose second panel is counted on
/// its own. Between them they exercise every route the chaining picks.
const SUBJECTS: &[&str] = &["chichen-itza-el-castillo", "art-deco-gas-station.0"];

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A required path from the environment, named so a missing handle says which,
/// and `source` saying what would have set it.
///
/// The handles come from two places, and the recipe re-enters the analysis
/// shell only when it is not already inside one, so running it from a different
/// shell leaves the corpus vars unset. Pointing every failure at the recipe
/// would misdirect exactly the reader who hit that.
fn required(var: &str, source: &str) -> Result<PathBuf, String> {
    env::var_os(var)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("{var} is unset; {source}"))
}

/// The corpus handles, exported by the analysis shell.
const FROM_SHELL: &str = "the `analysis` shell exports it, and `just model-test` \
                          inherits it from there. Enter that shell first";

/// The model handles, realized by the recipe rather than any shell.
const FROM_RECIPE: &str = "`just model-test` realizes the export and exports it";

/// How many regions a panel was read into, or why it holds none to count.
///
/// An irrelevant verdict and a map route are both failures here rather than
/// zero: the corpus is photographs of real buildings, so either answer means the
/// gate sent the panel down the wrong path.
fn regions_read(reading: &Reading) -> Result<usize, String> {
    let medium = match reading {
        Reading::Irrelevant { reason } => {
            return Err(format!("gated irrelevant: {reason}"));
        }
        Reading::Relevant { medium, .. } => medium,
    };
    let content = match medium {
        Medium::Picture { content, .. } | Medium::PictorialMap { content } => content,
        Medium::Map => return Err("read as a cartographic map".to_owned()),
        Medium::Plan => return Err("read as a plan".to_owned()),
    };
    Ok(match content {
        Content::Described { entities } => entities.len().get(),
        // The backstop ran, which is the honest count of nothing found.
        Content::Triaged { .. } => 0,
    })
}

/// Checks one image's analysis against its manifest entry, collecting every
/// mismatch so a run reports all of them rather than the first.
fn mismatches(id: &str, entry: &ImageEntry, analysis: &Analysis) -> Vec<String> {
    let mut found = Vec::new();
    let panels = &analysis.panels;

    match entry {
        ImageEntry::Single {
            regions,
            known_issue,
            ..
        } => {
            if let Some(issue) = known_issue {
                println!("  {id}: skipped, known issue: {issue}");
                return found;
            }
            if panels.len().get() != 1 {
                found.push(format!(
                    "{id}: a single image read as {} panels",
                    panels.len()
                ));
                return found;
            }
            if let Some(expected) = regions {
                match regions_read(&panels.first().reading) {
                    Ok(count) if count == *expected as usize => {}
                    Ok(count) => found.push(format!(
                        "{id}: read {count} regions, the manifest states {expected}"
                    )),
                    Err(reason) => found.push(format!("{id}: {reason}")),
                }
            }
        }
        ImageEntry::Composite {
            count,
            subimages,
            known_issue,
            ..
        } => {
            if let Some(issue) = known_issue {
                println!("  {id}: skipped, known issue: {issue}");
                return found;
            }
            let expected_panels = count.get() as usize;
            if panels.len().get() != expected_panels {
                found.push(format!(
                    "{id}: read {} panels, the manifest states {expected_panels}",
                    panels.len()
                ));
                return found;
            }
            for (index, assertions) in subimages {
                let Some(expected) = assertions.regions else {
                    continue;
                };
                let Some(panel) = panels.as_slice().get(*index) else {
                    found.push(format!("{id}: no panel {index} to count regions in"));
                    continue;
                };
                match regions_read(&panel.reading) {
                    Ok(read) if read == expected as usize => {}
                    Ok(read) => found.push(format!(
                        "{id}: panel {index} read {read} regions, the manifest states {expected}"
                    )),
                    Err(reason) => found.push(format!("{id}: panel {index} {reason}")),
                }
            }
        }
    }
    found
}

/// Opens the three models and the marker font the reads need.
async fn pipeline(cache_root: Option<&Path>) -> Result<Pipeline, Box<dyn std::error::Error>> {
    let accel = cache_root.map_or(Accel::Cpu, |root| Accel::CoreML { cache_root: root });
    let qwen = Qwen3::open(&required(MODEL_SHARD_ENV, FROM_RECIPE)?)
        .await
        .map_err(|source| format!("could not load Qwen: {}", describe(&source)))?;
    let sam3 = Sam3::open(&required(SAM3_EXPORT_ENV, FROM_RECIPE)?, accel)
        .map_err(|source| format!("could not load SAM 3: {}", describe(&source)))?;
    let dinov3 = Dinov3::open(&required(DINOV3_EXPORT_ENV, FROM_RECIPE)?, accel)
        .map_err(|source| format!("could not load DINOv3: {}", describe(&source)))?;
    let font: FontVec = font_from_env()?;
    Ok(Pipeline::new(qwen, sam3, dinov3, font))
}

#[tokio::test]
#[ignore = "needs the model exports and the fetched corpus; run `just model-test`"]
async fn panel_and_region_counts_match_the_corpus_manifest() -> TestResult {
    let manifest = CorpusManifest::load(&required(MANIFEST_ENV, FROM_SHELL)?)?;
    let images = required(IMAGES_ENV, FROM_SHELL)?;
    let cache_root = coreml_cache_root();
    let pipeline = pipeline(cache_root.as_deref()).await?;

    let mut found = Vec::new();
    for id in SUBJECTS {
        let entry = manifest
            .images
            .get(*id)
            .ok_or_else(|| format!("the manifest has no entry `{id}`"))?;
        let image = load_image(&images, id)
            .map_err(|source| format!("corpus image `{id}`: {}", describe(&source)))?;
        let analysis = pipeline
            .analyze_image(&image)
            .await
            .map_err(|source| format!("{id}: {}", describe(&source)))?;
        found.extend(mismatches(id, entry, &analysis));
    }

    assert!(
        found.is_empty(),
        "the chained pipeline disagreed with the corpus manifest:\n  {}",
        found.join("\n  ")
    );
    Ok(())
}
