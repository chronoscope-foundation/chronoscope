//! Type-level non-empty vector.
//!
//! `NonEmptyVec<T>` is a `Vec<T>` whose length is guaranteed to be at least
//! one. Used at structural positions where "at least one value" is a real
//! invariant so consumers don't have to defend against the empty case at
//! every site.

use std::num::NonZeroUsize;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Errors from constructing a [`NonEmptyVec`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NonEmptyVecError {
    /// The supplied `Vec` was empty.
    #[error("non-empty vec must contain at least one element")]
    Empty,
}

/// A `Vec<T>` whose length is at least one.
///
/// Construction: [`NonEmptyVec::singleton`] for a single element,
/// `TryFrom<Vec<T>>` or [`NonEmptyVec::try_from_vec`] from an
/// unvalidated `Vec`. Deserialization routes through the same
/// fallible path so empty wire input is rejected at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct NonEmptyVec<T> {
    inner: Vec<T>,
}

impl<T> NonEmptyVec<T> {
    /// Build a `NonEmptyVec` containing exactly one element.
    pub fn singleton(value: T) -> Self {
        Self { inner: vec![value] }
    }

    /// Build a `NonEmptyVec` from a `Vec`, returning an error if empty.
    pub fn try_from_vec(values: Vec<T>) -> Result<Self, NonEmptyVecError> {
        if values.is_empty() {
            return Err(NonEmptyVecError::Empty);
        }
        Ok(Self { inner: values })
    }

    /// The first element. Always present by construction.
    pub fn first(&self) -> &T {
        // The inner vector is non-empty by construction.
        #[allow(
            clippy::indexing_slicing,
            reason = "length >= 1 is the type-level invariant"
        )]
        &self.inner[0]
    }

    /// Borrow the inner slice.
    pub fn as_slice(&self) -> &[T] {
        &self.inner
    }

    /// Iterate over the elements.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.inner.iter()
    }

    /// Number of elements, guaranteed non-zero by construction.
    pub fn len(&self) -> NonZeroUsize {
        // The inner vector is non-empty by construction; `unwrap_or`
        // returns `MIN` (= 1) in the impossible case so the type-level
        // invariant carries through without a panic path.
        NonZeroUsize::new(self.inner.len()).unwrap_or(NonZeroUsize::MIN)
    }

    /// Append a value, preserving the non-empty invariant.
    pub fn push(&mut self, value: T) {
        self.inner.push(value);
    }

    /// Convert into the underlying `Vec`.
    pub fn into_vec(self) -> Vec<T> {
        self.inner
    }
}

impl<T> TryFrom<Vec<T>> for NonEmptyVec<T> {
    type Error = NonEmptyVecError;
    fn try_from(values: Vec<T>) -> Result<Self, Self::Error> {
        Self::try_from_vec(values)
    }
}

impl<T> IntoIterator for NonEmptyVec<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.inner.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a NonEmptyVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.inner.iter()
    }
}

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for NonEmptyVec<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = Vec::<T>::deserialize(deserializer)?;
        Self::try_from_vec(v).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_from_vec_rejects_empty() {
        let result: Result<NonEmptyVec<u8>, _> = NonEmptyVec::try_from_vec(Vec::new());
        assert_eq!(result, Err(NonEmptyVecError::Empty));
    }

    #[test]
    fn deserialize_rejects_empty_array() {
        let result: Result<NonEmptyVec<u8>, _> = serde_json::from_str("[]");
        assert!(result.is_err(), "empty JSON array must fail");
    }
}
