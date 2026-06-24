//! The join-semilattice abstraction shared by the value lattices.
//!
//! A join-semilattice has a least element ⊥ ([`bottom`](JoinSemilattice::bottom))
//! and an idempotent, commutative, associative binary join ⊔
//! ([`join`](JoinSemilattice::join)). [`join_all`](JoinSemilattice::join_all)
//! folds a stream from ⊥, so an empty stream yields ⊥ and a singleton yields
//! itself. Each value lattice (uncertain dates, resolved and unresolved
//! locations) supplies its own canonicalizing `join`; the fold rides on top.
//!
//! Only the join half is abstracted here. The meet half stays date-specific —
//! locations carry no meet — so this trait is deliberately narrower than a full
//! lattice.

/// A join-semilattice: a least element ⊥ and an idempotent, commutative,
/// associative join ⊔.
pub trait JoinSemilattice: Sized {
    /// The least element ⊥ — the join identity, `a ⊔ ⊥ == a`.
    fn bottom() -> Self;

    /// The join ⊔ of two values.
    fn join(&self, other: &Self) -> Self;

    /// The join of a stream, folded from ⊥. An empty stream yields ⊥; a
    /// singleton yields itself. Any per-type canonicalization lives in
    /// [`join`](Self::join), so the fold needs no override.
    fn join_all(values: impl IntoIterator<Item = Self>) -> Self {
        values
            .into_iter()
            .fold(Self::bottom(), |acc, v| acc.join(&v))
    }
}
