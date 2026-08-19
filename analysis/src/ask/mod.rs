//! The Qwen ask: the schema-constrained output the VLM produces, and the
//! plumbing around it.
//!
//! [`types`] are what the model emits under constrained decode, in the model's
//! own coordinates but core's leaf vocabulary. [`convert`] maps the model's box
//! into core's geometry.
//!
//! [`constraint::constraint_value`] turns a schema type into the value mistral.rs
//! compiles into an llguidance grammar. [`render::render_schema`] renders that
//! same value as the compact type text the prompt-assembly step will place ahead
//! of the decode, from the same source so the two cannot desync.
//! [`outcome::decode`] reads one completion's finish reason and content into an
//! [`Outcome`].

pub mod constraint;
pub mod convert;
pub mod outcome;
pub mod prompt;
pub mod render;
pub mod types;

pub use constraint::{ConstraintError, constraint_value};
pub use outcome::{DecodeError, Outcome, decode};
pub use prompt::Prompt;
pub use render::{RenderError, render_schema};
pub use types::{CompositeOutcome, Entity, Rect, RelevanceOutcome};
