//! Identity cluster — same-thing equivalence judgments.
//!
//! Cluster module for the `Identity` variant of
//! [`crate::facts::assertions::JudgmentAssertion`]. Covers same-entity,
//! same-artifact, and same-event equivalence claims.
//!
//! `SameArtifact` captures conceptual identity across different image
//! realizations of the same physical artifact: different scan
//! resolutions, B&W vs colorized scans, separate ingestions of the
//! same photograph or map sheet. Bytes differ (different `ImageId`s)
//! but the conceptual artifact is the same. There is no byte-level
//! equivalence variant — byte-equality is machine-derivable from a
//! hash and isn't a fact because we deduplicate it before it hits the
//! fact layer. The equivalence forms a class; projection unions the
//! metadata so a low-resolution ingestion inherits the LOC-quality
//! scan's capture-date, role-claim, etc.
//!
//! # Error states (rejected at submit time)
//!
//! - **Self-equivalence.** Each variant requires `a != b`. An equivalence
//!   claim that pairs an id with itself isn't an uncertainty conflict,
//!   it's a malformed claim.
//!
//! # Conflicts (surfaced at projection time)
//!
//! - **Transitive merge contradictions.** Equivalence classes form via
//!   union-find over the asserted pairs. When a class transitively
//!   merges entities (or artifacts, or events) that carry mutually
//!   contradictory attributes — different construction dates, different
//!   demolition locations — the solver surfaces the contradiction
//!   inside the merged class rather than rejecting the equivalence
//!   itself. Human review picks which underlying attribute claim to
//!   retract.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Identity-cluster fact.
///
/// Generic over the three reference kinds carried by the equivalence
/// variants. `Eq` is derivable here — none of the variants reach `f64`
/// coordinates; they hold pairs of opaque identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(bound(
    serialize = "EntId: Serialize, EvtId: Serialize, ImgId: Serialize",
    deserialize = "EntId: serde::de::DeserializeOwned, EvtId: serde::de::DeserializeOwned, ImgId: serde::de::DeserializeOwned"
))]
#[schemars(bound = "EntId: JsonSchema, EvtId: JsonSchema, ImgId: JsonSchema")]
pub enum Fact<EntId, EvtId, ImgId> {
    /// Two entity references describe the same entity.
    SameEntity {
        /// One side of the equivalence.
        a: EntId,
        /// The other side of the equivalence.
        b: EntId,
    },
    /// Two images represent the same physical artifact — different
    /// scans, resolutions, or color treatments of the same underlying
    /// photograph, painting, or map sheet. Bytes differ (different
    /// `ImageId`s); the conceptual artifact is the same. Metadata
    /// inherits across the equivalence class.
    ///
    /// Distinct from cryptographic image-equality (same SHA-256),
    /// which is machine-derivable from bytes and not a fact.
    SameArtifact {
        /// One side of the equivalence.
        a: ImgId,
        /// The other side of the equivalence.
        b: ImgId,
    },
    /// Two lifetime-event references describe the same event.
    SameEvent {
        /// One side of the equivalence.
        a: EvtId,
        /// The other side of the equivalence.
        b: EvtId,
    },
}
