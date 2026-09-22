//! The passes composed: an image in, what each of its panels reads as out.
//!
//! [`passes`] holds them and each is tested on its own; this module is the one
//! place that runs them in order, which is where the decisions between them
//! live. Pass 1 splits the frame into panels, and each panel is cropped, gated,
//! and read according to its medium: a photograph or a pictorial map is
//! segmented, described mark by mark and pooled into per-region embeddings,
//! while a map or a plan stops at its whole-panel embedding.
//!
//! [`Analysis`] is plain data: masks, text and embeddings, with no notion of
//! which image it came from. The caller pairs that identity back on, because the
//! fact that records it is the downstream depiction, not this stage.

pub mod passes;

use std::sync::Arc;

use ab_glyph::FontVec;
use chronoscope_core::grammar::composites::SubimageRegion;
use chronoscope_core::grammar::depiction::Perspective;
use chronoscope_core::grammar::geometry::{ProportionalRect, Region as Mask};
use chronoscope_core::grammar::text::Text;
use chronoscope_core::nonempty::NonEmptyVec;
use image::DynamicImage;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::ask::{DecodeError, ImageOutcome, RelevantMedium, TriageOutcome};
use crate::dinov3::{
    DegenerateEmbedding, Dinov3, EmbedError as Dinov3EmbedError, Embedding, ForwardError,
};
use crate::pipeline::passes::{
    DegenerateCrop, Marks, PanelError, ReadError, crop, detect_composite, gate, read_content,
    require_complete, subimages,
};
use crate::postprocess::{PostprocessError, postprocess_regions};
use crate::qwen3::{AskError, Qwen3};
use crate::sam3::{ConceptError, EncodeError, GraphError, Sam3};
use crate::scene::{DetectError, EmbedError, Features, Scene, detect, embed, with_scene};

/// The concept the segmenter is prompted with.
///
/// One concept per call: [`crate::sam3::Sam3::segment_concept`] takes a single
/// prompt and feeds the language encoder one row of tokens, so asking for
/// streets or bridges as well means another language encode and grounding
/// decode each. Those are the cheap half; the image encode they read is
/// reusable, once [`crate::scene::detect`] holds it on the scene rather than
/// running it per call.
const CONCEPT: &str = "building";

/// The most regions one panel is read at.
///
/// The overlay draws each mark in one of twenty distinguishable colors, so past
/// that two marks look alike and the number is all that separates them. The
/// describe loop also costs one VLM call per mark, so the bound is what keeps a
/// busy street scene from spending a hundred.
const MAX_REGIONS: usize = 20;

/// The models and the font one image is read with, held together so a batch
/// pays the load once.
///
/// `analyze_image` takes `&self`, so many images run against one `Pipeline` at
/// once. Each model sits behind an async mutex: ONNX Runtime's session takes
/// `&mut self` per run, so images queue for a model, and they queue without
/// holding a thread. Qwen batches internally and needs no such lock.
///
/// Every handle is an `Arc` so a panel's read can own a clone of the whole
/// pipeline. The scene scope is higher-ranked over the brand, and a future that
/// borrowed the pipeline could not be shown to outlive a brand the compiler
/// quantifies over, so the future owns its handles instead.
#[derive(Clone)]
pub struct Pipeline {
    qwen: Arc<Qwen3>,
    sam3: Arc<Mutex<Sam3>>,
    dinov3: Arc<Mutex<Dinov3>>,
    font: Arc<FontVec>,
}

impl Pipeline {
    /// Holds the loaded models and the marker font for the reads that follow.
    pub fn new(qwen: Qwen3, sam3: Sam3, dinov3: Dinov3, font: FontVec) -> Self {
        Self {
            qwen: Arc::new(qwen),
            sam3: Arc::new(Mutex::new(sam3)),
            dinov3: Arc::new(Mutex::new(dinov3)),
            font: Arc::new(font),
        }
    }

    /// Reads one image: its panels, and what each panel is.
    ///
    /// A single picture yields one full-frame panel, so a caller handles one
    /// shape rather than two. Every panel is read, and any hard failure fails
    /// the whole image: a partial `Analysis` would look like a complete reading
    /// of a smaller image.
    pub async fn analyze_image(&self, image: &DynamicImage) -> Result<Analysis, AnalyzeError> {
        let outcome = detect_composite(&self.qwen, image.clone())
            .await
            .map_err(ReadError::Ask)?;
        let composite = require_complete(outcome, "composite")?;

        let subimages = subimages(&composite)?;
        // The first subimage seeds the panel list, so the count `subimages`
        // guarantees reaches `Analysis` without a conversion that could fail.
        let SubimageRegion::Rect { rect } = subimages.first();
        let mut panels = NonEmptyVec::singleton(self.analyze_panel(image, *rect).await?);
        for SubimageRegion::Rect { rect } in subimages.iter().skip(1) {
            panels.push(self.analyze_panel(image, *rect).await?);
        }
        Ok(Analysis { panels })
    }

    /// Reads one panel: cropped out of the parent frame, scoped to its own
    /// scene, then gated and read according to its medium.
    ///
    /// The scene scope owns the crop, so the regions, the features they are
    /// pooled from and the overlay the marks are described over all belong to
    /// one frame and cannot escape to another panel.
    async fn analyze_panel(
        &self,
        image: &DynamicImage,
        rect: ProportionalRect,
    ) -> Result<Panel, AnalyzeError> {
        let panel = Arc::new(crop(image, rect)?);
        let reader = self.clone();
        let reading = with_scene::<_, Result<Reading, AnalyzeError>>(panel, move |scene| {
            Box::pin(async move {
                match gate(&reader.qwen, scene.image()).await? {
                    ImageOutcome::Irrelevant { reason } => Ok(Reading::Irrelevant { reason }),
                    ImageOutcome::Relevant { medium } => {
                        let features = embed(scene, &reader.dinov3).await?;
                        let medium = match medium {
                            RelevantMedium::Picture { view } => Medium::Picture {
                                view,
                                content: reader.analyze_entities(scene, &features).await?,
                            },
                            RelevantMedium::PictorialMap => Medium::PictorialMap {
                                content: reader.analyze_entities(scene, &features).await?,
                            },
                            RelevantMedium::Map => Medium::Map,
                            RelevantMedium::Plan => Medium::Plan,
                        };
                        Ok(Reading::Relevant {
                            embedding: Box::new(features.image()?),
                            medium,
                        })
                    }
                }
            })
        })
        .await?;
        Ok(Panel { rect, reading })
    }

    /// Reads what a content-bearing panel holds: an entity per surviving
    /// region, or the recall backstop when the segmenter found none.
    ///
    /// The regions are how the entities are found rather than the answer, so
    /// what leaves here is entities: a mask, a reading and an embedding each.
    async fn analyze_entities<'b>(
        &self,
        scene: &Scene<'b>,
        features: &Features<'b>,
    ) -> Result<Content, AnalyzeError> {
        let detections = detect(scene, &self.sam3, CONCEPT).await?;
        let regions = postprocess_regions(detections, MAX_REGIONS)?;

        match read_content(&self.qwen, scene, &regions, self.font.as_ref()).await? {
            Marks::Triaged(outcome) => Ok(Content::Triaged { outcome }),
            Marks::Described(descriptions) => {
                // The read stage answers one mark per region it was handed and
                // triages only when handed none, so a count that disagrees, or
                // a described panel with nothing in it, is that contract
                // breaking rather than an image being unreadable. Both counts
                // are read before the zip consumes them, so either failure
                // reports what actually arrived.
                let pairing = AnalyzeError::Pairing {
                    regions: regions.len(),
                    readings: descriptions.len(),
                };
                if descriptions.len() != regions.len() {
                    return Err(pairing);
                }
                let entities = regions
                    .iter()
                    .zip(descriptions)
                    .map(|(region, description)| {
                        Ok(Entity {
                            region: region.mask().clone(),
                            description,
                            embedding: features.region(region)?,
                        })
                    })
                    .collect::<Result<Vec<_>, AnalyzeError>>()?;
                let entities = NonEmptyVec::try_from_vec(entities).map_err(|_| pairing)?;
                Ok(Content::Described { entities })
            }
        }
    }
}

/// One image's reading: its panels in the order pass 1 found them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    /// One per panel, left to right then top to bottom. A single picture is one
    /// full-frame panel, so there is no such thing as an analysis of no panels.
    pub panels: NonEmptyVec<Panel>,
}

/// One panel: where it sits in the parent frame, and what it reads as.
///
/// Identity is positional. The rect is the panel's own place in the image, and
/// nothing here names the image itself.
///
/// Two frames meet on this type. The rect is proportional over the parent
/// image, while everything under [`Self::reading`] is measured on the panel's
/// own crop. A consumer that wants parent-frame coordinates composes the two;
/// one that draws a panel's masks over the parent without composing them is
/// right only when the panel is the whole frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Panel {
    /// The panel's place in the parent frame, in proportional coordinates.
    pub rect: ProportionalRect,
    /// What the panel reads as, measured on the panel's own crop.
    pub reading: Reading,
}

/// What a panel is, once the gate has judged it.
///
/// The embedding rides on [`Self::Relevant`], so "embedded but irrelevant" and
/// "relevant but unembedded" are both unspellable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reading {
    /// The panel carries nothing worth reading, and why.
    Irrelevant {
        /// The gate's reason for setting it aside.
        reason: Text,
    },
    /// The panel is worth reading: its whole-panel embedding and its medium.
    Relevant {
        /// DINOv3's reading of the whole panel, boxed: an embedding is four
        /// kilobytes, and a composite is often mostly panels that never earn
        /// one, which would each carry the cost of the ones that do.
        embedding: Box<Embedding>,
        /// The medium, carrying what was read from it.
        medium: Medium,
    },
}

/// A relevant panel's medium, carrying the content the segmentable ones hold.
///
/// A map or a plan carries none: the building flow does not read abstract
/// media, and the map path is a later stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Medium {
    /// A photograph, read from a viewpoint.
    Picture {
        /// Exterior or interior.
        view: Perspective,
        /// What the segmenter and the describe loop made of it.
        content: Content,
    },
    /// A pictorial map, segmented like a picture but with no single viewpoint.
    PictorialMap {
        /// What the segmenter and the describe loop made of it.
        content: Content,
    },
    /// A cartographic map.
    Map,
    /// An orthographic plan.
    Plan,
}

/// What a segmentable panel held: its entities, or the backstop's verdict when
/// the segmenter found nothing.
///
/// [`Self::Described`] is non-empty by type, so a panel with no regions must
/// carry the backstop rather than an empty list that reads like a clean answer.
///
/// Both variants carry named fields. The tag rides inside the object, and a
/// payload that serializes as a sequence has nowhere to put it: an unnamed
/// sequence here fails at run time on the ordinary success path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    /// One entity per surviving region.
    Described {
        /// The entities, in the order the marks were numbered.
        entities: NonEmptyVec<Entity>,
    },
    /// No regions survived; whether the segmenter missed a real structure.
    Triaged {
        /// The backstop's verdict.
        outcome: TriageOutcome,
    },
}

/// One entity of a panel: where it is, what it is, and how it embeds.
///
/// No score: the detector's confidences are spent ranking in
/// [`crate::postprocess::postprocess_regions`], and what
/// survives that ranking is regions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// The mask, on the pixel grid of the panel it was read from, not of the
    /// parent image. [`Panel::rect`] is what places that grid in the parent, so
    /// a consumer working in parent coordinates composes the two.
    pub region: Mask,
    /// What the model said it is.
    pub description: Text,
    /// DINOv3's reading of the region alone.
    pub embedding: Embedding,
}

/// Why an image could not be read.
///
/// The variants follow the stages, so the source chain reads in the order the
/// work happened. Whether a failure is worth retrying is
/// [`is_retriable`](Self::is_retriable), which asks the leaf error rather than
/// the stage that wrapped it.
#[derive(Debug, Error)]
pub enum AnalyzeError {
    /// A read stage failed: pass 1, the gate, the describe loop, or triage.
    #[error(transparent)]
    Read(#[from] ReadError),

    /// Pass 1 placed a panel off the 0-1000 grid.
    #[error(transparent)]
    Panel(#[from] PanelError),

    /// A panel's rect covers no whole pixel of the image.
    #[error(transparent)]
    Crop(#[from] DegenerateCrop),

    /// DINOv3 did not read the panel.
    #[error(transparent)]
    Embed(#[from] EmbedError),

    /// SAM 3 did not segment the panel.
    #[error(transparent)]
    Detect(#[from] DetectError),

    /// The raw regions could not be resolved into a clean set.
    #[error(transparent)]
    Postprocess(#[from] PostprocessError),

    /// A pooled embedding had no direction.
    #[error(transparent)]
    Embedding(#[from] DegenerateEmbedding),

    /// The describe loop's answer does not pair with the regions it was handed:
    /// a different count, or a described panel carrying nothing at all. Either
    /// way no entity can be built without guessing which reading is whose.
    #[error("the read stage answered {readings} marks for {regions} regions")]
    Pairing {
        /// How many regions the describe loop was handed.
        regions: usize,
        /// How many readings it answered with.
        readings: usize,
    },
}

impl AnalyzeError {
    /// Whether the same image is worth running again.
    ///
    /// The question is asked of the leaf rather than the stage that wrapped it:
    /// a request that failed to reach the model, or a graph that failed to run,
    /// says nothing about the image and may answer next time, while a graph that
    /// returned the wrong shape will return it again.
    ///
    /// The engine's own faults go with the transport. A request that never
    /// reached the model, one the engine answered with no choices, and one it
    /// reported a generation fault on are all the backend failing rather than
    /// the image being unreadable. What stays permanent is a malformed answer:
    /// constrained decoding makes one near-impossible, so a parse failure is a
    /// genuine mismatch rather than noise worth spending another model call on.
    ///
    /// Every leaf is named rather than swept into a wildcard, so a new failure
    /// mode is a compile error here instead of defaulting to permanent, which
    /// is the answer that quietly drops an image a rerun would have read.
    ///
    /// Classifying is all this does. The worker seam is what acts on it, since
    /// `chronoscope_workers` owns the retry budget and the loop that spends it,
    /// which is why nothing in this crate calls it yet.
    pub fn is_retriable(&self) -> bool {
        match self {
            Self::Read(read) => match read {
                ReadError::Ask(ask) => match ask {
                    AskError::Send(_) | AskError::NoChoices => true,
                    AskError::Decode(decode) => match decode {
                        DecodeError::Generation { .. } => true,
                        DecodeError::EmptyStop | DecodeError::Parse { .. } => false,
                    },
                    AskError::Constraint(_) | AskError::Render(_) => false,
                },
                ReadError::Incomplete { .. } => false,
            },
            Self::Embed(EmbedError::Model(embed)) => match embed {
                Dinov3EmbedError::Forward(forward) => match forward {
                    ForwardError::Input(_) | ForwardError::Run(_) => true,
                    ForwardError::MissingOutput
                    | ForwardError::OutputType(_)
                    | ForwardError::OutputShape { .. }
                    | ForwardError::NonFiniteToken => false,
                },
                Dinov3EmbedError::Resize(_) => false,
            },
            Self::Detect(detect) => match detect {
                DetectError::Encode(encode) => match encode {
                    EncodeError::Graph(graph) => graph_is_retriable(graph),
                    EncodeError::Original(_) | EncodeError::Resize(_) => false,
                },
                DetectError::Concept(concept) => match concept {
                    ConceptError::Graph(graph) => graph_is_retriable(graph),
                    ConceptError::Tokenize(_)
                    | ConceptError::OutputShape { .. }
                    | ConceptError::Mask(_) => false,
                },
            },
            Self::Panel(_) => false,
            Self::Crop(_) => false,
            Self::Postprocess(postprocess) => match postprocess {
                PostprocessError::Region(_) => false,
            },
            Self::Embedding(_) => false,
            Self::Pairing { .. } => false,
        }
    }
}

/// Whether an ONNX graph failure is the run stumbling or the graph answering
/// wrongly. Shared by the encoder and the concept head, which fail the same way.
fn graph_is_retriable(graph: &GraphError) -> bool {
    match graph {
        GraphError::Input { .. } | GraphError::Run { .. } => true,
        GraphError::MissingOutput { .. }
        | GraphError::OutputType { .. }
        | GraphError::NonFinite { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use chronoscope_core::grammar::geometry::{Dimensions, Region};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A unit embedding. The width is the model's, and a zero vector has no
    /// direction to normalize, so one axis carries the length.
    fn embedding() -> Result<Embedding, DegenerateEmbedding> {
        let mut raw = [0.0_f32; 1024];
        if let Some(first) = raw.first_mut() {
            *first = 1.0;
        }
        Embedding::from_raw(&raw)
    }

    /// One panel carrying `reading`, the shape the binary writes.
    fn analysis(reading: Reading) -> Analysis {
        Analysis {
            panels: NonEmptyVec::singleton(Panel {
                rect: ProportionalRect::full(),
                reading,
            }),
        }
    }

    /// A relevant panel of `medium`.
    fn relevant(medium: Medium) -> Result<Reading, DegenerateEmbedding> {
        Ok(Reading::Relevant {
            embedding: Box::new(embedding()?),
            medium,
        })
    }

    /// Serializing, reading back, and serializing again: equal JSON means both
    /// directions agree, without asking the types for equality.
    fn round_trips(analysis: &Analysis) -> TestResult {
        let once = serde_json::to_string(analysis)?;
        let restored: Analysis = serde_json::from_str(&once)?;
        assert_eq!(serde_json::to_string(&restored)?, once);
        Ok(())
    }

    #[test]
    fn every_reading_round_trips_through_json() -> TestResult {
        let mask = Region::from_dense(Dimensions::new(2, 2)?, &[true, false, false, false])?
            .ok_or("a one-pixel mask")?;
        let described = Content::Described {
            entities: NonEmptyVec::singleton(Entity {
                region: mask,
                description: Text::new("a tower")?,
                embedding: embedding()?,
            }),
        };
        let triaged = Content::Triaged {
            outcome: TriageOutcome::NothingHere,
        };

        // Every variant of all three enums, because each is internally tagged
        // and the tag has to fit inside its payload: a described panel carries
        // a sequence, and `Map`/`Plan` carry nothing at all. This is the only
        // place that exercises the serialization the `analyze` binary writes,
        // so a variant left out here is one that can fail in the binary alone.
        for medium in [
            Medium::Picture {
                view: Perspective::Exterior,
                content: described,
            },
            Medium::Picture {
                view: Perspective::Interior,
                content: triaged.clone(),
            },
            Medium::PictorialMap { content: triaged },
            Medium::Map,
            Medium::Plan,
        ] {
            round_trips(&analysis(relevant(medium)?))?;
        }
        round_trips(&analysis(Reading::Irrelevant {
            reason: Text::new("a plate of food")?,
        }))?;
        Ok(())
    }

    #[test]
    fn the_backend_stumbling_is_retriable_and_a_bad_answer_is_not() -> TestResult {
        // The graph arms are asserted by the match being exhaustive rather than
        // by a value here: constructing an `ort::Error` loads ONNX Runtime,
        // which the commit gate does not provide, and a pure classification
        // test has no business needing it.
        let wrong_shape = AnalyzeError::Detect(DetectError::Concept(ConceptError::OutputShape {
            scores: 3,
            masks: 7,
            pixels: 2,
        }));
        assert!(!wrong_shape.is_retriable());

        // The engine answering with no choices at all is the backend failing,
        // so the image deserves another attempt.
        let empty = AnalyzeError::Read(ReadError::Ask(AskError::NoChoices));
        assert!(empty.is_retriable());

        // The engine failing mid-generation is the backend, not the answer.
        let generation =
            AnalyzeError::Read(ReadError::Ask(AskError::Decode(DecodeError::Generation {
                raw_prefix: String::new(),
            })));
        assert!(generation.is_retriable());

        // A malformed answer under constrained decoding is a real mismatch.
        let Err(source) = serde_json::from_str::<u8>("{") else {
            return Err("`{` is not a number".into());
        };
        let parse = AnalyzeError::Read(ReadError::Ask(AskError::Decode(DecodeError::Parse {
            raw: "{".to_owned(),
            source,
        })));
        assert!(!parse.is_retriable());
        Ok(())
    }
}
