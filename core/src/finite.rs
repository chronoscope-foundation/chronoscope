//! [`Finite`] — an `f64` whose boundary owns the finiteness check.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An `f64` guaranteed finite — no `NaN`, no `±∞` — with `-0.0` normalized to
/// `+0.0`. That makes `Eq`, `Ord`, and `Hash` total and honest, so a type built
/// from `Finite` fields derives them instead of hand-rolling `total_cmp` /
/// `to_bits`. Validate once at the boundary with [`new`](Finite::new); the typed
/// interior mints from trusted arithmetic with
/// [`new_unchecked`](Finite::new_unchecked).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub(crate) struct Finite(f64);

impl Finite {
    /// The value if it is finite, else `None`. `-0.0` is normalized to `+0.0`,
    /// the one bit pattern that would otherwise split an equal magnitude across
    /// `Hash`/`Ord`.
    pub(crate) fn new(value: f64) -> Option<Self> {
        value.is_finite().then_some(Self(value + 0.0))
    }

    /// Mint a value already known finite — a geodesic-solver output or
    /// arithmetic over finite values. The `debug_assert` catches a broken
    /// promise in dev; release trusts the caller.
    pub(crate) const fn new_unchecked(value: f64) -> Self {
        debug_assert!(
            value.is_finite(),
            "Finite::new_unchecked on a non-finite value"
        );
        Self(value + 0.0)
    }

    /// The wrapped `f64`.
    pub(crate) fn get(self) -> f64 {
        self.0
    }
}

impl Eq for Finite {}

impl Hash for Finite {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

impl PartialOrd for Finite {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Finite {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl<'de> Deserialize<'de> for Finite {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        Self::new(value)
            .ok_or_else(|| serde::de::Error::custom(format!("value must be finite, got {value}")))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn hash_of(f: Finite) -> u64 {
        let mut hasher = DefaultHasher::new();
        f.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn new_rejects_non_finite_and_keeps_finite() {
        assert!(Finite::new(f64::NAN).is_none());
        assert!(Finite::new(f64::INFINITY).is_none());
        assert!(Finite::new(f64::NEG_INFINITY).is_none());
        assert_eq!(Finite::new(3.5).map(Finite::get), Some(3.5));
    }

    #[test]
    fn negative_zero_normalizes_across_eq_hash_ord() -> TestResult {
        // to_bits and total_cmp both distinguish -0.0 from +0.0, so this passes
        // only if `new` folded -0.0 to +0.0.
        let neg = Finite::new(-0.0).ok_or("-0.0 is finite")?;
        let pos = Finite::new(0.0).ok_or("+0.0 is finite")?;
        assert_eq!(neg, pos);
        assert_eq!(hash_of(neg), hash_of(pos));
        assert_eq!(neg.cmp(&pos), std::cmp::Ordering::Equal);
        Ok(())
    }

    #[test]
    fn ordering_is_numeric() -> TestResult {
        let a = Finite::new(1.0).ok_or("finite")?;
        let b = Finite::new(2.0).ok_or("finite")?;
        assert!(a < b);
        Ok(())
    }

    #[test]
    fn deserialize_rejects_non_finite_wire_value() {
        // serde_json parses 1e400 as f64::INFINITY (or errors); either way the
        // wire value is rejected. A finite number round-trips.
        assert!(serde_json::from_str::<Finite>("1e400").is_err());
        assert!(serde_json::from_str::<Finite>("1.5").is_ok());
    }
}
