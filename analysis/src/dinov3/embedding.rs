//! The unit-length embedding the pipeline compares and stores.
//!
//! Every [`Embedding`] is finite and unit-length, so the cosine similarity of
//! two is their dot product. Two paths mint one: [`normalize`], which refuses a
//! vector with no direction, and the validating `Deserialize`, which refuses a
//! stored vector that is not already one.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use super::manifest::EMBEDDING_DIM;

/// The fraction of its input magnitude a vector must keep to have a direction.
///
/// The sums arrive accumulated in f64, whose rounding over a few hundred
/// patches stays near 1e-13 of the magnitude that went in. Once cancellation
/// leaves less than 1e-6 of that magnitude, the rounding reaches the f32
/// resolution of the direction handed out. The floor is relative to the input,
/// so no constant ties it to the model's typical token norm.
pub(super) const CANCELLATION: f64 = 1e-6;

/// How far a stored embedding's norm may sit from 1.
///
/// An f64-normalized vector rounded to f32 lands within 6e-8 of unit length
/// (half an f32 ulp per component), so this admits everything [`normalize`]
/// produces with wide margin and rejects any vector that was never normalized:
/// raw DINOv3 tokens carry norms near 8.
const UNIT_NORM_TOLERANCE: f64 = 1e-6;

/// A unit-length, finite DINOv3 embedding: a whole-image CLS or a pooled
/// region, compared by dot product.
///
/// Serialized as a bare sequence of components; deserializing holds the stored
/// vector to the same width, finiteness, and unit length every embedding the
/// crate mints carries.
#[derive(Debug, Clone)]
pub struct Embedding([f32; EMBEDDING_DIM]);

impl Embedding {
    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    /// Cosine similarity with `other`. Both are unit-length, so the dot product
    /// is the cosine itself, with no norms left to divide by.
    ///
    /// Accumulated in f64 so a caller subtracting the result from 1 keeps the
    /// digits that distinguish rounding from a fault.
    #[cfg(test)]
    pub(crate) fn cosine(&self, other: &Self) -> f64 {
        self.0
            .iter()
            .zip(&other.0)
            .map(|(&left, &right)| f64::from(left) * f64::from(right))
            .sum()
    }

    /// The direction of a raw model-space vector: the CLS token a forward pass
    /// produced, or a stored reference a comparison holds beside one, which
    /// cosine needs on the same footing.
    pub(crate) fn from_raw(components: &super::Token) -> Result<Self, DegenerateEmbedding> {
        normalize(&components.map(f64::from), super::magnitude(components))
    }
}

/// Scales `sum` to unit length, or refuses it when it keeps too little of
/// `mass`, the summed magnitude of the vectors that went into it.
///
/// A zero or cancelled vector has no direction: dividing by its norm would hand
/// out `NaN`, or rounding noise scaled up to unit length, under a type that
/// promises a meaningful direction.
pub(super) fn normalize(
    sum: &[f64; EMBEDDING_DIM],
    mass: f64,
) -> Result<Embedding, DegenerateEmbedding> {
    let norm = sum.iter().map(|value| value * value).sum::<f64>().sqrt();
    // The comparison fails for a NaN norm and for a zero norm against a zero
    // mass, so both land on the error.
    if norm > CANCELLATION * mass {
        Ok(Embedding(sum.map(|value| (value / norm) as f32)))
    } else {
        Err(DegenerateEmbedding { norm, mass })
    }
}

/// A vector whose inputs cancelled, leaving no direction to embed.
#[derive(Debug, Clone, Copy, PartialEq, Error)]
#[error(
    "the vector's norm {norm:e} is at most {CANCELLATION:e} times the {mass:e} magnitude that went \
     into it, so its inputs cancelled and it has no direction"
)]
pub struct DegenerateEmbedding {
    /// The vector's norm before normalizing.
    pub norm: f64,
    /// The summed magnitude of the vectors that went into it.
    pub mass: f64,
}

impl Serialize for Embedding {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter())
    }
}

impl<'de> Deserialize<'de> for Embedding {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Through `Vec` because serde derives array impls only up to 32
        // elements, and through validation because a stored vector is untrusted
        // input to a type that promises finite unit length.
        let components = Vec::<f32>::deserialize(deserializer)?;
        validate(components).map_err(de::Error::custom)
    }
}

/// Holds a stored vector to what [`normalize`] produces.
fn validate(components: Vec<f32>) -> Result<Embedding, EmbeddingInvalid> {
    let components = <[f32; EMBEDDING_DIM]>::try_from(components).map_err(|rejected| {
        EmbeddingInvalid::Width {
            len: rejected.len(),
        }
    })?;
    if let Some((index, &value)) = components
        .iter()
        .enumerate()
        .find(|(_, value)| !value.is_finite())
    {
        return Err(EmbeddingInvalid::NonFinite { index, value });
    }
    let norm = components
        .iter()
        .map(|&value| f64::from(value) * f64::from(value))
        .sum::<f64>()
        .sqrt();
    if (norm - 1.0).abs() > UNIT_NORM_TOLERANCE {
        return Err(EmbeddingInvalid::NotUnit { norm });
    }
    Ok(Embedding(components))
}

/// Why a stored vector is not an [`Embedding`].
#[derive(Debug, Error)]
enum EmbeddingInvalid {
    #[error(
        "a stored embedding has {len} components, not the {EMBEDDING_DIM} this crate is built for"
    )]
    Width { len: usize },

    #[error("component {index} of a stored embedding is {value}, not finite")]
    NonFinite { index: usize, value: f32 },

    #[error(
        "a stored embedding has norm {norm}, more than {UNIT_NORM_TOLERANCE:e} from the unit length \
         every embedding carries"
    )]
    NotUnit { norm: f64 },
}

#[cfg(test)]
mod tests {
    use serde::de::value::{Error as ValueError, SeqDeserializer};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn magnitude(vector: &[f64; EMBEDDING_DIM]) -> f64 {
        vector.iter().map(|value| value * value).sum::<f64>().sqrt()
    }

    /// The message a stored vector is rejected with, read through the real
    /// `Deserialize` rather than `validate`, so the wiring is what is tested.
    /// `SeqDeserializer` carries a `NaN`, which JSON cannot.
    fn rejection(components: Vec<f32>) -> Result<String, Box<dyn std::error::Error>> {
        let deserializer = SeqDeserializer::<_, ValueError>::new(components.into_iter());
        match Embedding::deserialize(deserializer) {
            Ok(_) => Err("the stored vector was accepted".into()),
            Err(error) => Ok(error.to_string()),
        }
    }

    /// A raw vector along `axes`, each axis carrying `scale`, so a wrong
    /// normalization shows up as a cosine off the hand-computed angle.
    fn along(axes: &[usize], scale: f32) -> Result<Embedding, DegenerateEmbedding> {
        let mut raw = [0.0_f32; EMBEDDING_DIM];
        for &axis in axes {
            if let Some(slot) = raw.get_mut(axis) {
                *slot = scale;
            }
        }
        Embedding::from_raw(&raw)
    }

    #[test]
    fn cosine_is_the_angle_between_two_directions() -> TestResult {
        let x = along(&[0], 2.0)?;
        let diagonal = along(&[0, 1], 1.0)?;

        // A one-hot vector normalizes to exactly 1 on its axis whatever its
        // scale, so these three products are exact.
        assert_eq!(x.cosine(&x), 1.0);
        assert_eq!(x.cosine(&along(&[0], -5.0)?), -1.0);
        assert_eq!(x.cosine(&along(&[1], 3.0)?), 0.0);

        // Half a right angle: the f32 rounding of each component is all that
        // separates this from the exact value.
        let drift = (x.cosine(&diagonal) - 0.5_f64.sqrt()).abs();
        assert!(drift < 1e-7, "45 degrees came out {drift:e} off");
        Ok(())
    }

    #[test]
    fn normalized_embeddings_round_trip_through_json_bit_for_bit() -> TestResult {
        let spread: [f64; EMBEDDING_DIM] = std::array::from_fn(|i| (i as f64 * 0.37).sin());
        // One dominant component over many tiny ones: the tiny ones normalize
        // into f32's subnormal range, where a format that flushes or shortens
        // floats would change bits.
        let lopsided: [f64; EMBEDDING_DIM] =
            std::array::from_fn(|i| if i == 0 { 1e6 } else { 1e-36 * (i as f64) });

        for vector in [spread, lopsided] {
            let embedding = normalize(&vector, magnitude(&vector))?;
            let json = serde_json::to_string(&embedding)?;
            let restored: Embedding = serde_json::from_str(&json)?;
            let bits = |embedding: &Embedding| -> Vec<u32> {
                embedding
                    .as_slice()
                    .iter()
                    .map(|value| value.to_bits())
                    .collect()
            };
            assert_eq!(bits(&restored), bits(&embedding));
        }
        Ok(())
    }

    #[test]
    fn a_stored_vector_of_the_wrong_width_is_rejected() -> TestResult {
        let message = rejection(vec![1.0])?;
        assert!(message.contains("has 1 components"), "{message}");
        Ok(())
    }

    #[test]
    fn a_stored_non_finite_component_is_rejected_by_index() -> TestResult {
        let mut components = vec![0.0_f32; EMBEDDING_DIM];
        if let Some(first) = components.first_mut() {
            *first = 1.0;
        }
        if let Some(slot) = components.get_mut(7) {
            *slot = f32::NAN;
        }
        let message = rejection(components)?;
        assert!(message.contains("component 7"), "{message}");
        Ok(())
    }

    #[test]
    fn a_stored_vector_off_unit_length_is_rejected() -> TestResult {
        // A raw token's norm and one just past the tolerance: both were never
        // produced by `normalize`.
        let unit = 1.0 / (EMBEDDING_DIM as f32).sqrt();
        for scale in [8.0_f32, 1.0 + 1e-4] {
            let message = rejection(vec![unit * scale; EMBEDDING_DIM])?;
            assert!(message.contains("norm"), "scale {scale}: {message}");
        }
        Ok(())
    }
}
