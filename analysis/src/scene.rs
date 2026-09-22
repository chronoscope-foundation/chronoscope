//! One image and the regions and features read from it, tied together by type.
//!
//! A mask and a patch grid are meaningless against a frame they did not come
//! from: pooling a region of image A against B's features reads the wrong
//! patches and answers confidently, and no runtime check catches it, since two
//! crops of one size share a grid. So a [`Scene`] carries an invariant lifetime
//! brand, and [`Region`] and [`Features`] carry the same one. Same brand means
//! same scene, which is why [`Region::iou`] cannot fail and
//! [`Features::region`] cannot read another image's patches.
//!
//! The brand is the [`with_scene`] closure's existential `'b`, fresh per call
//! under a `for<'b>` bound, the shape `FactStore::with_tx` uses for
//! transactions. Two scenes' brands are distinct regions that invariance
//! refuses to unify, so mixing them is a compile error rather than a check.
//!
//! Minting is what the guarantee rests on. A branded value is stamped by
//! [`detect`] and [`embed`], which ran the model on that scene's own image, or
//! by [`Region::new`], where a caller holding a mask from elsewhere states the
//! claim in code and the grid half of it is checked.
//!
//! Three rules hold the brand up, none of them enforced by the compiler:
//!
//! - Branded types never derive `Deserialize` or `Default`. Either would let a
//!   caller conjure a region for an inferred brand out of nothing.
//! - `'b` is spelled out in every branded signature, never elided and never
//!   introduced by a `&self` borrow. An elided lifetime binds to the borrow
//!   region, which two scenes in one block can share.
//! - Nothing public hands out the brand marker itself.
//!
//! `Mask` names the plain [`chronoscope_core`] mask throughout this crate, so
//! the branded [`Region`] keeps the short name at the call sites that pair it
//! with a scene.

use std::{future::Future, marker::PhantomData, pin::Pin, sync::Arc};

use chronoscope_core::grammar::geometry::Region as Mask;
use image::DynamicImage;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::{
    concept::Concept,
    dinov3::{
        DegenerateEmbedding, Dinov3, EmbedError as Dinov3EmbedError, Embedding,
        Features as RawFeatures,
    },
    geometry,
    sam3::{ConceptError, EncodeError, Sam3},
};

/// The invariant-lifetime marker every branded type carries. `'b` sits in both
/// argument and return position, so it is neither covariant nor contravariant
/// and two scenes' brands cannot unify.
type Brand<'b> = PhantomData<fn(&'b ()) -> &'b ()>;

/// One image, and the brand that ties what is read from it back to it.
pub struct Scene<'b> {
    image: Arc<DynamicImage>,
    brand: Brand<'b>,
}

impl<'b> Scene<'b> {
    /// The image this scene reads. Regions over it are masks on its pixel grid.
    pub fn image(&self) -> &DynamicImage {
        &self.image
    }
}

/// Runs `f` over a fresh scene for `image`, handing it a brand no other scene
/// shares.
///
/// The closure returns a boxed future so branded values can be held across an
/// `.await` inside the scope while still being unable to leave it: `'b` is the
/// closure's own existential, so nothing carrying it outlives the call.
pub async fn with_scene<F, T>(image: Arc<DynamicImage>, f: F) -> T
where
    F: for<'b> FnOnce(&'b Scene<'b>) -> Pin<Box<dyn Future<Output = T> + Send + 'b>> + Send,
    T: Send,
{
    let scene = Scene {
        image,
        brand: PhantomData,
    };
    f(&scene).await
}

/// [`with_scene`] for a caller with nothing to await: the closure returns its
/// value directly.
///
/// The brand is a type-level device and does not need a runtime; pure region
/// work reaches for this rather than a runtime it has no other use for.
pub fn with_scene_sync<F, T>(image: Arc<DynamicImage>, f: F) -> T
where
    F: for<'b> FnOnce(&'b Scene<'b>) -> T,
{
    let scene = Scene {
        image,
        brand: PhantomData,
    };
    f(&scene)
}

/// A mask over one scene's image.
///
/// A region is a mask and the image it belongs to, nothing else. A confidence is
/// something a particular producer attached to one, so [`detect`] pairs its
/// regions with scores in a [`Detection`] rather than the region carrying a
/// number a hand-drawn mask would have no meaning for.
pub struct Region<'b> {
    mask: Mask,
    brand: Brand<'b>,
}

impl<'b> Region<'b> {
    /// Brands a mask that did not come from [`detect`]: a reference mask a
    /// comparison holds beside ours, or a stored region read back later. `None`
    /// when the mask is not on `scene`'s image grid.
    ///
    /// The grid is the half of the claim that can be checked, and it is the half
    /// the code depends on: the checks this brand replaced compared grids, so a
    /// mask sized to another frame now indexes past the overlay buffer rather
    /// than being skipped. Whether the mask describes *this* image is the
    /// caller's to know, which is what branding it says.
    pub fn new(scene: &Scene<'b>, mask: Mask) -> Option<Region<'b>> {
        (mask.width() == scene.image.width() && mask.height() == scene.image.height())
            .then(|| Region::new_unchecked(scene, mask))
    }

    /// A mask this module just read off `scene`'s own image, branded without a
    /// check because the grid came from the image the model ran on.
    fn new_unchecked(scene: &Scene<'b>, mask: Mask) -> Region<'b> {
        Region {
            mask,
            brand: scene.brand,
        }
    }

    /// This region's brand carried onto a mask derived from its own pixels,
    /// which `postprocess` rebuilds a survivor from. `None` when the mask is not
    /// on this region's grid.
    ///
    /// Derivation keeps the grid, so the check costs a comparison and never
    /// fires; it is here because the signature accepts any mask and the runtime
    /// guards that once caught a foreign one are gone.
    pub(crate) fn with_mask(&self, mask: Mask) -> Option<Region<'b>> {
        (mask.dimensions() == self.mask.dimensions()).then_some(Region {
            mask,
            brand: self.brand,
        })
    }

    /// The mask itself, for plain data on its way out of the scope.
    pub fn mask(&self) -> &Mask {
        &self.mask
    }

    /// Intersection over union with another region of the same scene: shared
    /// foreground area over combined foreground area, `1.0` for equal masks and
    /// `0.0` for disjoint ones.
    ///
    /// Total, because the brand already proves what a runtime check would look
    /// for: both masks are on this scene's grid.
    pub fn iou(&self, other: &Region<'b>) -> f64 {
        geometry::intersection_over_union(&self.mask, &other.mask)
    }
}

/// A region, the confidence its producer gave it, and the concept whose prompt
/// found it.
///
/// A record of one proposal rather than fields on [`Region`], because both
/// belong to the detector rather than the mask: a stored or hand-drawn region
/// has neither. The score is what `postprocess` ranks by; the concept is what
/// the detector was asked for when this mask came back, which is the difference
/// between a road and the building beside it.
pub struct Detection<'b> {
    /// The mask the detector proposed.
    pub region: Region<'b>,
    /// How confident the detector was in it.
    pub score: f32,
    /// The concept whose prompt found it.
    pub concept: Concept,
}

/// DINOv3's reading of one scene, held so a region of that scene can be pooled
/// out of it.
pub struct Features<'b> {
    raw: RawFeatures,
    brand: Brand<'b>,
}

impl<'b> Features<'b> {
    /// The whole-image embedding.
    pub fn image(&self) -> Result<Embedding, DegenerateEmbedding> {
        self.raw.image()
    }

    /// The embedding of the part of the image `region` covers.
    ///
    /// The brand is what makes this answer meaningful: `region` is a mask over
    /// the frame these features were read from, so the proportional coordinates
    /// pooling reads it in land on the patches that actually cover it.
    pub fn region(&self, region: &Region<'b>) -> Result<Embedding, DegenerateEmbedding> {
        self.raw.region(&region.mask)
    }
}

/// Reads `scene`'s image with DINOv3, stamping the result with its brand.
///
/// Inference runs on ONNX Runtime's own thread pool and the future resolves when
/// it completes, so no runtime worker is held for the duration of a forward pass.
///
/// The lock is an async mutex rather than a blocking one because the wait belongs
/// in async-land: `Session::run_async` still takes `&mut self`, so images in
/// flight queue for the model, and they should queue without holding threads.
pub async fn embed<'b>(
    scene: &Scene<'b>,
    model: &Arc<Mutex<Dinov3>>,
) -> Result<Features<'b>, EmbedError> {
    let raw = model.lock().await.embed(&scene.image).await?;
    Ok(Features {
        raw,
        brand: scene.brand,
    })
}

/// Segments `scene`'s image for each of `concepts` with SAM 3, stamping every
/// region with the scene's brand.
///
/// The image encode is the expensive half and it does not depend on the prompt,
/// so it runs once and every concept reads it. That is why the vocabulary is a
/// list here rather than a caller looping: a loop outside would re-encode the
/// image per concept.
///
/// The whole sequence holds one lock, so the encoder's features live and die
/// inside the call while the prompts take turns over them.
pub async fn detect<'b>(
    scene: &Scene<'b>,
    model: &Arc<Mutex<Sam3>>,
    concepts: &[Concept],
) -> Result<Vec<Detection<'b>>, DetectError> {
    let mut model = model.lock().await;
    let encoded = model.encode(&scene.image).await?;

    let mut detections = Vec::new();
    for &concept in concepts {
        let scored = model.segment_concept(&encoded, concept.prompt()).await?;
        detections.extend(scored.into_iter().map(|scored| Detection {
            region: Region::new_unchecked(scene, scored.region),
            score: scored.score,
            concept,
        }));
    }
    Ok(detections)
}

/// Why a scene could not be embedded.
#[derive(Debug, Error)]
pub enum EmbedError {
    /// The model did not produce features.
    #[error(transparent)]
    Model(#[from] Dinov3EmbedError),
}

/// Why a scene could not be segmented.
#[derive(Debug, Error)]
pub enum DetectError {
    /// The image encoder failed.
    #[error(transparent)]
    Encode(#[from] EncodeError),

    /// The concept head failed.
    #[error(transparent)]
    Concept(#[from] ConceptError),
}

/// Negative compile-time check for the guarantee this module exists for: a
/// region of one scene cannot be compared against a region of another.
///
/// `iou` demands the two brands unify, which the outer scene's region can only
/// do by outliving its own scope, so the compiler rejects it as escaping
/// borrowed data (`E0521`). The function is never called, and an uncalled one is
/// still typechecked. [`BrandsNestWithoutMixing`] is the positive control: the
/// same nesting minus the cross-use, as a normal doctest.
/// A `compile_fail` block passes on any compile error, so the pair is what pins
/// the rejection to the mixing — break the shape itself and the control fails
/// loudly instead of this block passing for the wrong reason.
///
/// The runtime checks that used to catch a mask from the wrong frame are gone,
/// so this pair is the only thing standing between a region and the wrong
/// image.
///
/// ```compile_fail
/// use std::sync::Arc;
///
/// use chronoscope_analysis::scene::{Region, with_scene_sync};
/// use chronoscope_core::grammar::geometry::Region as Mask;
/// use image::DynamicImage;
///
/// fn mix(mask: Mask) -> Option<f64> {
///     let image = Arc::new(DynamicImage::new_rgb8(2, 1));
///     let other = Arc::clone(&image);
///     with_scene_sync(image, |first| {
///         let ours = Region::new(first, mask.clone())?;
///         with_scene_sync(other, |second| {
///             let theirs = Region::new(second, mask)?;
///             Some(ours.iou(&theirs))
///         })
///     })
/// }
/// ```
#[cfg(doctest)]
struct BrandsCannotUnify;

/// Positive control for [`BrandsCannotUnify`]: two scenes nest, each region
/// compared only against one of its own scene, and that compiles.
///
/// Same shape, same nesting, same uncalled-function trick, only the mixing is
/// gone. Each region is put to work so both brands are exercised rather than
/// merely bound.
///
/// ```
/// use std::sync::Arc;
///
/// use chronoscope_analysis::scene::{Region, with_scene_sync};
/// use chronoscope_core::grammar::geometry::Region as Mask;
/// use image::DynamicImage;
///
/// fn nest(mask: Mask) -> Option<f64> {
///     let image = Arc::new(DynamicImage::new_rgb8(2, 1));
///     let other = Arc::clone(&image);
///     with_scene_sync(image, |first| {
///         let ours = Region::new(first, mask.clone())?;
///         let inner = with_scene_sync(other, |second| {
///             let theirs = Region::new(second, mask)?;
///             Some(theirs.iou(&theirs))
///         })?;
///         Some(ours.iou(&ours) + inner)
///     })
/// }
/// ```
#[cfg(doctest)]
struct BrandsNestWithoutMixing;

#[cfg(test)]
mod tests {
    use chronoscope_core::grammar::geometry::Dimensions;

    use super::*;
    use crate::test_support::frame;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A mask over `grid` covering the pixels `dense` marks.
    fn mask(grid: Dimensions, dense: &[bool]) -> Result<Mask, Box<dyn std::error::Error>> {
        Ok(Mask::from_dense(grid, dense)?.ok_or("non-empty")?)
    }

    #[test]
    fn iou_scores_overlap_between_two_regions_of_one_scene() -> TestResult {
        let grid = Dimensions::new(2, 2)?;
        let full = mask(grid, &[true, true, true, true])?;
        let left = mask(grid, &[true, false, true, false])?;
        let right = mask(grid, &[false, true, false, true])?;

        with_scene_sync(frame(2, 2), |scene| -> TestResult {
            let full = Region::new(scene, full).ok_or("on the scene's grid")?;
            let left = Region::new(scene, left).ok_or("on the scene's grid")?;
            let right = Region::new(scene, right).ok_or("on the scene's grid")?;

            assert_eq!(full.iou(&full), 1.0);
            assert_eq!(left.iou(&right), 0.0);
            // Left is two of full's four pixels: intersection 2, union 4.
            assert_eq!(left.iou(&full), 0.5);
            Ok(())
        })
    }

    #[test]
    fn a_mask_on_another_frames_grid_is_not_a_region_of_this_scene() -> TestResult {
        let grid = Dimensions::new(3, 1)?;
        let elsewhere = mask(grid, &[true, false, false])?;

        with_scene_sync(frame(2, 1), |scene| {
            assert!(Region::new(scene, elsewhere).is_none());
        });
        Ok(())
    }

    #[test]
    fn a_region_carries_its_mask_back_out() -> TestResult {
        let grid = Dimensions::new(2, 1)?;
        let dense = mask(grid, &[true, false])?;

        let area = with_scene_sync(frame(2, 1), |scene| {
            let region = Region::new(scene, dense).ok_or("on the scene's grid")?;
            Ok::<_, Box<dyn std::error::Error>>(region.mask().area())
        })?;
        assert_eq!(area, 1);
        Ok(())
    }

    #[test]
    fn a_derived_mask_off_this_regions_grid_is_not_a_region() -> TestResult {
        let grid = Dimensions::new(2, 1)?;
        let dense = mask(grid, &[true, false])?;
        let elsewhere = mask(Dimensions::new(3, 1)?, &[true, false, false])?;

        with_scene_sync(frame(2, 1), |scene| -> TestResult {
            let region = Region::new(scene, dense).ok_or("on the scene's grid")?;
            assert!(region.with_mask(elsewhere).is_none());
            Ok(())
        })
    }

    /// The async scope is the one the pipeline uses, and its higher-ranked
    /// closure returning a boxed future is where a call site fails to infer, so
    /// it is exercised here rather than first in the wiring.
    ///
    /// The closure's output crosses threads, so it carries an owned message
    /// rather than a boxed error, which is not `Send`.
    #[tokio::test]
    async fn branded_values_cross_an_await_inside_the_async_scope() -> TestResult {
        let grid = Dimensions::new(2, 1)?;
        let dense = mask(grid, &[true, false])?;

        let overlap = with_scene(frame(2, 1), |scene| {
            Box::pin(async move {
                let region = Region::new(scene, dense).ok_or("on the scene's grid")?;
                tokio::task::yield_now().await;
                Ok::<_, String>(region.iou(&region))
            })
        })
        .await?;
        assert_eq!(overlap, 1.0);
        Ok(())
    }
}
