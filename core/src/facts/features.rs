//! Feature vocabulary for building observations.
//!
//! A [`Feature`] is one observed attribute of a building — a roof shape, a
//! facade material, a story count, an overall condition impression, a
//! transcribed sign. Feature claims live on
//! [`crate::facts::observation::Fact::Feature`]; the evidential basis (which
//! image, which region, who observed it) lives in the citation, via
//! [`crate::facts::citations::JudgmentSource::ImageObservation`].
//!
//! Feature facts are per-observation singular: a building with both a gabled
//! wing and a dome produces two `RoofShape` facts. No exhaustiveness claim is
//! built into the fact-shape; whether absence of a feature variant from a
//! source means "not present" or "not reported" is a property of the source,
//! not the grammar.

use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One observed feature on a building.
///
/// Each variant wraps its claim in a single named field, serialized as
/// `{"type": "...", "<field>": ...}` under internal `"type"` tagging.
/// Localization (which image, where in the image) lives on the citation
/// that backs the fact, not on the fact itself.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    /// Number of visible stories on the section being observed.
    StoryCount {
        /// Count of visible stories.
        stories: u32,
    },
    /// A roof structure observed on the building.
    RoofShape {
        /// The observed roof shape.
        shape: RoofShape,
    },
    /// A facade material observed on the building.
    FacadeMaterial {
        /// The observed facade material.
        material: FacadeMaterial,
    },
    /// Overall observed condition of the building.
    Condition {
        /// The observed overall condition.
        condition: Condition,
    },
    /// Transcribed text from a sign, plaque, or inscription visible on
    /// the building.
    Sign {
        /// The transcribed sign text.
        text: SignText,
    },
}

/// Roof structures the analysis pipeline distinguishes.
///
/// Closed enum: no open `Other { name: String }` variant, because conflict
/// detection needs cheap structural equality between observations. Extending
/// the vocabulary is a code change.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RoofShape {
    /// Flat or near-flat roof.
    Flat,
    /// Two sloping sides meeting at a ridge — the prototypical pitched
    /// roof.
    Gabled,
    /// Four sloping sides meeting at ridges, no vertical end walls.
    Hipped,
    /// Two pitch angles per side (steep lower, shallower upper).
    Mansard,
    /// Hemispherical or oblate dome.
    Dome,
    /// Tall pointed structure (church spire, steeple).
    Spire,
}

/// Facade materials the analysis pipeline distinguishes.
///
/// Closed enum, same reasoning as [`RoofShape`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FacadeMaterial {
    Brick,
    Stone,
    Wood,
    Stucco,
    Concrete,
    Glass,
    Metal,
}

/// Overall observed condition of a building, ordinal.
///
/// Ordered from intact to ruined. Conflict detection that wants to
/// flag impossible transitions (a building going from `Ruined` back to
/// `Intact` without a `Repaired` event) compares ordinal positions.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    /// Visibly intact; no obvious damage or significant wear.
    Intact,
    /// Showing weather, wear, or aging but structurally sound.
    Weathered,
    /// Partial damage visible (broken windows, missing sections,
    /// scorched walls) but the structure is still recognizable.
    Damaged,
    /// Largely or wholly collapsed; substantial portions missing or
    /// reduced to foundation.
    Ruined,
}

/// Minimum length for a [`SignText`] in trimmed characters. One-
/// character signs (e.g. a numeric house number "7") are valid, so the
/// floor is low.
pub const SIGN_TEXT_MIN_LEN: usize = 1;

/// Maximum length for a [`SignText`] in characters. Large enough for a
/// historical-marker paragraph, small enough to prevent unbounded
/// transcriptions of, say, a whole page of inscribed text.
pub const SIGN_TEXT_MAX_LEN: usize = 1024;

crate::validated_string_newtype! {
    /// Validated transcription of a sign, plaque, or inscription.
    ///
    /// Wire shape: a transparent string. The smart constructor trims
    /// surrounding whitespace and enforces
    /// [`SIGN_TEXT_MIN_LEN`]..=[`SIGN_TEXT_MAX_LEN`] — an empty `Sign`
    /// fact would convey nothing, and an unbounded transcription is a
    /// storage hazard.
    SignText, min = SIGN_TEXT_MIN_LEN, max = SIGN_TEXT_MAX_LEN, trim = true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::ids::ValidatedStringError;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn sign_text_rejects_whitespace_only() {
        assert!(matches!(
            SignText::new("   "),
            Err(ValidatedStringError::TooShort { .. })
        ));
    }

    #[test]
    fn sign_text_rejects_oversized() {
        let s = "x".repeat(SIGN_TEXT_MAX_LEN + 1);
        assert!(matches!(
            SignText::new(s),
            Err(ValidatedStringError::TooLong { .. })
        ));
    }

    #[test]
    fn sign_text_round_trips_storing_trimmed_form() -> TestResult {
        // Validates the "trim once, store once" contract — input with
        // surrounding whitespace gets trimmed before storage, so the
        // round-trip yields the trimmed value.
        let s = SignText::new("  POST OFFICE  ")?;
        assert_eq!(s.as_str(), "POST OFFICE");
        let json = serde_json::to_string(&s)?;
        let parsed: SignText = serde_json::from_str(&json)?;
        assert_eq!(parsed, s);
        Ok(())
    }
}
