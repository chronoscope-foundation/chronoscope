//! [`JsonPath`] — an address into a projected entity.

use std::fmt;

use serde::{Serialize, Serializer};

/// One segment of a [`JsonPath`]: a field access or an array index. The
/// projector emits only these two; the value shape it addresses has no maps
/// keyed by anything but field names, and no nesting beyond indexed lists.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PathSegment {
    /// A named field (`.names`).
    Field(String),
    /// An array index (`[0]`).
    Index(usize),
}

impl fmt::Display for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Field(name) => write!(f, ".{name}"),
            Self::Index(i) => write!(f, "[{i}]"),
        }
    }
}

/// A path addressing one citable position in a [`ProjectedEntity`](super::ProjectedEntity).
///
/// Renders as `$`, `$.names[0]`, `$.construction.started_at`, etc. — the
/// leading `$` is the value root. Emit-only: it serializes to its rendered
/// string but carries no `Deserialize`, so the sidecar never needs a parser.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct JsonPath(Vec<PathSegment>);

impl JsonPath {
    /// The root path, `$`.
    pub const fn root() -> Self {
        Self(Vec::new())
    }

    /// Extend with a field segment.
    pub fn field(mut self, name: impl Into<String>) -> Self {
        self.0.push(PathSegment::Field(name.into()));
        self
    }

    /// Extend with an array-index segment.
    pub fn index(mut self, idx: usize) -> Self {
        self.0.push(PathSegment::Index(idx));
        self
    }

    /// The segments, in order.
    pub fn segments(&self) -> &[PathSegment] {
        &self.0
    }
}

impl fmt::Display for JsonPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("$")?;
        for seg in &self.0 {
            seg.fmt(f)?;
        }
        Ok(())
    }
}

impl Serialize for JsonPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}
