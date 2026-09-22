//! The vocabulary the segmenter is asked for, as a closed set.
//!
//! Its own module because the vocabulary is shared language rather than one
//! stage's detail: [`crate::scene::detect`] prompts with it,
//! [`crate::pipeline::Entity`] carries it out of the pipeline, and the handling
//! that treats a road differently from a building keys on it. A closed set is
//! what makes that handling fail to compile when a class is added.

use serde::{Deserialize, Serialize};

/// One class of structure the segmenter is prompted for, and the class a
/// detection of that prompt is labelled with.
///
/// The type is the vocabulary: [`strum::VariantArray`] supplies `VARIANTS`, the
/// list [`crate::scene::detect`] is handed. Breadth is what makes a structure
/// that is not a building findable at all: a step pyramid answers no "building"
/// prompt, and a panel whose only subject that prompt misses reads as a panel
/// with nothing in it.
///
/// [`crate::sam3::Sam3::segment_concept`] takes one prompt and feeds the
/// language encoder one row of tokens, so each variant costs one language
/// encode and one grounding decode. [`crate::scene::detect`] runs the image
/// encode once for the whole list and every concept reads it, so what a variant
/// adds is the cheap half.
///
/// The vocabulary holds no concept that names a part of another concept's
/// subject. [`crate::postprocess::postprocess_detections`] gives each pixel to
/// the highest-scored region covering it and rebuilds every survivor from only
/// the pixels it owns, which resolves partial overlap between peers. A nesting
/// concept, such as a tower on a building, a dome, or a window, is proposed as a
/// region contained inside its parent's; when the part outscores the whole, the
/// parent is rebuilt with a hole in it, so its DINOv3 embedding is pooled over
/// the mutilation and its overlay mark shows the hole to the VLM. A nesting
/// concept therefore waits on the containment pass, the parent-with-sub-features
/// grouping [`crate::postprocess`] records as missing.
///
/// Peers do overlap: a road crosses a bridge, and a monumental building answers
/// the monument prompt and the building prompt at nearly the same extent, where
/// the dedup threshold settles it. The property that holds is the absence of
/// part-whole nesting, not disjointness.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::IntoStaticStr,
    strum::VariantArray,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Concept {
    /// An occupiable structure: the common case and the vocabulary's anchor.
    Building,
    /// A span carrying a way over something.
    Bridge,
    /// A structure built to commemorate rather than to be occupied.
    Monument,
    /// A way itself, rather than what stands along it.
    Road,
}

impl Concept {
    /// The text SAM 3 is prompted with for this concept: the variant's name in
    /// `snake_case`.
    ///
    /// The serde tag is specified separately and happens to agree, so a prompt
    /// that has to stop being a bare noun to find its subject can move without
    /// changing how an analysis names the class to its readers.
    pub fn prompt(self) -> &'static str {
        self.into()
    }
}
