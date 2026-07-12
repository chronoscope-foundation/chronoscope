//! Date-conflict detection over a projected entity.
//!
//! [`fact_lineage`] projects each slot's support as the whole stored facts
//! ([`FactAtom`]) behind it, so an over-determined slot reads its fighting
//! evidence straight off its consensus support. [`minimize`] is the deletion-MUS
//! primitive that reduces such a slot's fact set to one minimal fighting set.

mod detect;
pub mod minimize;

pub use detect::{FactAtom, fact_lineage};
pub(crate) use detect::{fact_date, minimal_fighting_sets};
