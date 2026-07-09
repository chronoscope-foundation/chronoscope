//! Conflict reports — typed, enumerable records of over-determined projection
//! slots, each with a stable id, a structured location, and a per-kind
//! resolution menu.
//!
//! A [`ConflictReport`] pins where a projection over-determined a slot
//! ([`ConflictLocation`]), what evidence fought ([`ConflictKind`]-specific
//! data), and the curator's options ([`Resolution`]). [`AnyConflictReport`] is
//! the closed sum the API serves. Identity is content-derived
//! ([`ConflictId`]), so a frontend URL routes to the same conflict across
//! projection runs while the same facts keep fighting.
//!
//! The report types re-exported here are defined in a `report` submodule;
//! [`minimize`] is the deletion-MUS primitive the detector uses to reduce an
//! over-determined slot's fact set to one minimal fighting set. The detector
//! itself lands here alongside them.

mod detect;
pub mod minimize;
mod report;

pub use detect::{CitedFact, cited_lineage, detect_conflicts};
pub use report::{
    AnyConflictReport, BookendEndpoint, ConflictId, ConflictKind, ConflictLocation, ConflictPath,
    ConflictReport, DateConflict, EventEndpoint, Resolution, Uninhabited,
};
