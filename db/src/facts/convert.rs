//! The [`FactId`](chronoscope_core::grammar::ids::FactId) row boundary.
//!
//! Core's fact-id space is `u64`; SQLite stores `INTEGER` (`i64`). Every
//! crossing goes through these checked conversions so an out-of-range value
//! surfaces as a structured error naming what was being converted, never a
//! silent wrap. The backend's own subject ids are `i64` end-to-end
//! ([`ids`](super::ids)) and never come through here.

/// A value that doesn't fit the other side of the `u64` ⇄ `i64` boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{context}: value {value} does not fit an {target}")]
pub struct IdConvertError {
    /// What was being converted, from the call site.
    pub context: &'static str,
    /// The offending value, rendered.
    pub value: String,
    /// The type the value had to fit.
    pub target: &'static str,
}

/// Convert an id-space `u64` to a storable `i64`.
pub fn u64_to_i64(value: u64, context: &'static str) -> Result<i64, IdConvertError> {
    i64::try_from(value).map_err(|_| IdConvertError {
        context,
        value: value.to_string(),
        target: "i64",
    })
}

/// Convert a stored `i64` back to the id space.
pub fn i64_to_u64(value: i64, context: &'static str) -> Result<u64, IdConvertError> {
    u64::try_from(value).map_err(|_| IdConvertError {
        context,
        value: value.to_string(),
        target: "u64",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn u64_above_i64_max_is_rejected_with_context() -> TestResult {
        let Err(err) = u64_to_i64(u64::MAX, "test id") else {
            return Err("u64::MAX must not convert to i64".into());
        };
        assert_eq!(err.context, "test id");
        assert_eq!(err.value, u64::MAX.to_string());
        assert_eq!(err.target, "i64");
        Ok(())
    }

    #[test]
    fn negative_i64_is_rejected_with_context() -> TestResult {
        let Err(err) = i64_to_u64(-1, "stored fact id") else {
            return Err("-1 must not convert to u64".into());
        };
        assert_eq!(err.context, "stored fact id");
        assert_eq!(err.value, "-1");
        assert_eq!(err.target, "u64");
        Ok(())
    }

    #[test]
    fn boundary_values_round_trip() -> TestResult {
        assert_eq!(u64_to_i64(0, "zero")?, 0);
        let max = u64::try_from(i64::MAX)?;
        assert_eq!(u64_to_i64(max, "max")?, i64::MAX);
        assert_eq!(i64_to_u64(i64::MAX, "max")?, max);
        Ok(())
    }
}
