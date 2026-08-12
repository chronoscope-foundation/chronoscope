//! The planar CHW image both models feed their encoder.
//!
//! DINOv3 and SAM 3 preprocess identically up to one axis: DINOv3 rescales each
//! byte to `[0, 1]` and resamples in float, SAM 3 keeps the raw byte and
//! resamples in uint8 (its graph bakes the rescale). The resample plumbing —
//! deinterleave to planar, antialiased-bilinear per plane, back to a flat
//! buffer — is one operation for both and lives here once. The dtype and the
//! rescale are the only variation, so they stay with the caller: the sample type
//! is the `T` parameter and the rescale is the `map` handed to
//! [`ChwImage::from_rgb`].

use std::fmt::Debug;

use fast_image_resize::{
    FilterType, PixelTrait, ResizeAlg, ResizeOptions, Resizer,
    images::{TypedImage, TypedImageRef},
    pixels::{F32, U8},
};
use image::RgbImage;
use thiserror::Error;

/// RGB, the only channel count either model takes and what `to_rgb8` produces.
pub(crate) const CHANNELS: usize = 3;

/// A scalar sample paired with the resampler pixel it resizes as: `u8` for the
/// uint8 path (SAM 3), `f32` for the rescaled float path (DINOv3). The resize is
/// generic over this, so a fix to the resample plumbing lands on both models.
pub(crate) trait Sample: Copy {
    type Pixel: PixelTrait + Default + Copy + Debug;
    fn to_pixel(self) -> Self::Pixel;
    fn from_pixel(pixel: Self::Pixel) -> Self;
}

impl Sample for u8 {
    type Pixel = U8;
    fn to_pixel(self) -> U8 {
        U8::new(self)
    }
    fn from_pixel(pixel: U8) -> u8 {
        pixel.0
    }
}

impl Sample for f32 {
    type Pixel = F32;
    fn to_pixel(self) -> F32 {
        F32::new(self)
    }
    fn from_pixel(pixel: F32) -> f32 {
        pixel.0
    }
}

/// A planar CHW image over sample type `T`, resamplable to the model's square.
pub(crate) struct ChwImage<T: Sample> {
    height: usize,
    width: usize,
    samples: Vec<T>,
}

impl<T: Sample> ChwImage<T> {
    /// Packs an RGB frame HWC to CHW, mapping each byte into the sample space.
    /// DINOv3 folds its `[0, 1]` rescale into `map`; SAM 3 passes the identity.
    pub(crate) fn from_rgb(image: &RgbImage, map: impl Fn(u8) -> T) -> Self {
        let (width, height) = image.dimensions();
        let interleaved = image.as_raw();
        let mut samples = Vec::with_capacity(interleaved.len());
        for channel in 0..CHANNELS {
            samples.extend(
                interleaved
                    .iter()
                    .skip(channel)
                    .step_by(CHANNELS)
                    .map(|&byte| map(byte)),
            );
        }
        Self {
            height: height as usize,
            width: width as usize,
            samples,
        }
    }

    /// Resamples to the `resolution` square with an antialiased bilinear kernel,
    /// squashing the frame rather than letterboxing it: the processor is handed
    /// one `size` and no aspect to preserve.
    ///
    /// Each channel is resampled on its own, which is what a separable kernel
    /// over a planar image means and what torchvision does with the same input.
    /// `Convolution` is the antialiased form: it widens kernel support by the
    /// reduction factor the way torchvision and Pillow both do, so every
    /// downscale is filtered rather than point-sampled.
    pub(crate) fn resize(&self, resolution: usize) -> Result<Self, ResizeError> {
        let (Some(width), Some(height), Some(side)) = (
            positive_extent(self.width),
            positive_extent(self.height),
            positive_extent(resolution),
        ) else {
            return Err(ResizeError::Dimensions {
                height: self.height,
                width: self.width,
                resolution,
            });
        };

        let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear));
        let mut resizer = Resizer::new();
        let mut samples = Vec::with_capacity(CHANNELS * resolution * resolution);

        for plane in self.samples.chunks_exact(self.height * self.width) {
            let pixels: Vec<T::Pixel> = plane.iter().map(|&sample| sample.to_pixel()).collect();
            let source = TypedImageRef::new(width, height, &pixels).map_err(|source| {
                ResizeError::Plane {
                    height: self.height,
                    width: self.width,
                    source,
                }
            })?;
            let mut resampled: TypedImage<T::Pixel> = TypedImage::new(side, side);
            resizer
                .resize_typed(&source, &mut resampled, &options)
                .map_err(|source| ResizeError::Resample { resolution, source })?;
            samples.extend(resampled.pixels().iter().map(|&pixel| T::from_pixel(pixel)));
        }

        Ok(Self {
            height: resolution,
            width: resolution,
            samples,
        })
    }

    /// The grid width, so a caller can shape the encoder input tensor.
    pub(crate) fn width(&self) -> usize {
        self.width
    }

    /// The grid height.
    pub(crate) fn height(&self) -> usize {
        self.height
    }

    /// The planar samples, channel-major, for the encoder input tensor.
    pub(crate) fn samples(&self) -> &[T] {
        &self.samples
    }
}

/// Narrows a dimension to the `u32` the resampler indexes with, rejecting zero.
///
/// The resampler answers a zero-sized request with an empty image rather than an
/// error, so a degenerate extent has to be caught here or it becomes a silently
/// empty tensor. Shared with the mask upscale, which resamples on the same rule.
pub(crate) fn positive_extent(value: usize) -> Option<u32> {
    u32::try_from(value).ok().filter(|&value| value > 0)
}

/// Why an image could not be resampled to a square.
#[derive(Debug, Error)]
pub enum ResizeError {
    #[error("cannot resample a {height}x{width} image to {resolution}x{resolution}")]
    Dimensions {
        height: usize,
        width: usize,
        resolution: usize,
    },

    #[error("the resampler rejected a {height}x{width} image plane")]
    Plane {
        height: usize,
        width: usize,
        #[source]
        source: fast_image_resize::InvalidPixelsSize,
    },

    #[error("resampling a plane to {resolution}x{resolution} failed")]
    Resample {
        resolution: usize,
        #[source]
        source: fast_image_resize::ResizeError,
    },
}
