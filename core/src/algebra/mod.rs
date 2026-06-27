//! Algebraic-structure traits shared by the value lattices.
//!
//! [`monoid`] is the floor — a [`CommutativeMonoid`](monoid::CommutativeMonoid).
//! [`lattice`] adds idempotence to reach the semilattices and the bounded
//! lattice over them. [`semiring`] is the two-monoid algebra provenance
//! accumulates in.

pub mod lattice;
pub mod monoid;
pub mod semiring;
