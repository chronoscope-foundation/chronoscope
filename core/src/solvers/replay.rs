//! Recompute-from-scratch folds: whole-fan-in scans, named so a call site says
//! what it costs.
//!
//! Everything here scans every fact contributing to an aggregate, at a cost that
//! grows with the entity's fan-in. Where a maintained counterpart exists, the
//! replay fold is the oracle it is checked against — the homomorphism law — and
//! the rebuild when a cached entry is evicted.
//!
//! [`lifespan`] has no maintained counterpart yet, so it runs on live
//! requests — every listing row, marker pin, and tile cell pays its entity's
//! depiction fan-in. Naming `replay::` at those call sites is what keeps the
//! scan visible until the caching work turns it into a rebuild path.
//!
//! The bodies sit in `crate::projection::replay`, where the accumulators they
//! start from are sealed; the module name is the same on both routes.

pub use crate::projection::replay::lifespan;
