//! Identifier newtypes for the fact-store wire model.
//!
//! Two families live here:
//!
//! - **String-shaped IDs** (`UserId`, `IngesterRunId`, `AnalyzerProcess`,
//!   `AnalyzerVersion`). Transparent `String` newtypes built by the
//!   [`validated_string_newtype`](crate::validated_string_newtype) macro, so
//!   the smart constructor and the wire boundary alike run the shared NUL
//!   check and the [`ID_MAX_LEN`] cap. `AnalyzerProcess` / `AnalyzerVersion`
//!   name the machine process behind an analyzer commit and the build that
//!   ran it.
//! - **Integer-shaped IDs** (`FactId`, `CommitId`). [`FactId`] is a `u64`
//!   newtype that prevents intermixing with other integer ids. [`CommitId`] is
//!   content-addressed — derived from the commit's canonical encoding so
//!   re-submitting an identical commit yields the same id.
//!
//! The backend's entity / event / image id kinds are bundled behind an
//! [`IdScheme`] (its `Entity` / `Event` / `Image` associated types), pinned on
//! [`FactStore::Ids`]. Each backend supplies its own scheme: the in-memory
//! backend's [`MemoryIds`](crate::store::memory::MemoryIds) over `MemoryEntityId(u64)`
//! etc., and a Postgres backend would substitute its own.
//!
//! [`FactStore`]: crate::store::FactStore
//! [`FactStore::Ids`]: crate::store::FactStore::Ids

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ============================================================================
// Id-scheme traits
// ============================================================================

/// The bounds every id kind in a scheme must satisfy.
///
/// Mirrors the `PersistentId` bound alias in `store.rs` but adds what the
/// grammar's serde / schema path needs: `Debug` and `DeserializeOwned` for the
/// wire round-trip, `JsonSchema` for the schema derive. A supertrait plus
/// blanket impl, so the bound pile is named once rather than repeated at every
/// associated-type declaration.
pub trait SchemeId:
    Clone
    + std::fmt::Debug
    + Ord
    + std::hash::Hash
    + Serialize
    + serde::de::DeserializeOwned
    + JsonSchema
    + Send
    + Sync
    + 'static
{
}

impl<T> SchemeId for T where
    T: Clone
        + std::fmt::Debug
        + Ord
        + std::hash::Hash
        + Serialize
        + serde::de::DeserializeOwned
        + JsonSchema
        + Send
        + Sync
        + 'static
{
}

/// Bundles the grammar's three id kinds behind one type parameter.
///
/// The grammar / store types take a single `R: IdScheme` instead of three
/// separate `<EntId, EvtId, ImgId>` parameters; each reads `R::Entity`,
/// `R::Event`, `R::Image` for the kind it needs. A scheme is a zero-size marker
/// — [`BundleLocal`](crate::submit::BundleLocal) (submission indices) and
/// [`MemoryIds`](crate::store::memory::MemoryIds) (the in-memory backend).
///
/// The marker carries the comparison / debug bounds the bundled types derive:
/// `#[derive(Debug, Clone, PartialEq, Eq, Ord, Hash)]` on a type generic over
/// `R` emits an `R: Debug` (etc.) bound on each impl, so `R: IdScheme` alone
/// has to satisfy them for the derives to be usable behind the uniform bound.
/// `'static` because ids are plain owned data: values built over a scheme
/// (commits, facts) ride inside boxed `Send` futures whose lifetime is the
/// caller's to choose.
pub trait IdScheme: Clone + std::fmt::Debug + Eq + Ord + std::hash::Hash + 'static {
    /// The entity id kind.
    type Entity: SchemeId;
    /// The lifetime-event id kind.
    type Event: SchemeId;
    /// The image id kind.
    type Image: SchemeId;
}

// ============================================================================
// Subject-ID macro (integer-backed, string wire form)
// ============================================================================

/// Emit an integer-backed subject-id newtype with the wire convention every
/// backend id scheme shares: `Serialize` writes the number as its canonical
/// decimal string and the `JsonSchema` is a non-referenceable bare `string`,
/// so the read wire's id stays a `string` for every backend and no consumer
/// bakes in an integer-shaped id (or a backend type name). `Deserialize`
/// accepts ONLY the canonical rendering — input that parses but re-renders
/// differently (`007`, `+7`, `-0`) is rejected, so every string-decoded
/// position (path params, cursors, stored JSON) admits exactly one spelling
/// per id. One macro, consumed by every backend's id scheme, so the schemes
/// cannot drift in wire behavior.
#[macro_export]
macro_rules! subject_id_newtype {
    ($name:ident, $repr:ty, $prefix:literal, $doc:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub $repr);

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(f, concat!($prefix, "-{}"), self.0)
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(
                &self,
                serializer: S,
            ) -> ::std::result::Result<S::Ok, S::Error> {
                serializer.collect_str(&self.0)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> ::std::result::Result<Self, D::Error> {
                let s = ::std::string::String::deserialize(deserializer)?;
                let value = s.parse::<$repr>().map_err(|e| {
                    ::serde::de::Error::custom(format!("invalid {} {s:?}: {e}", stringify!($name)))
                })?;
                if value.to_string() != s {
                    return Err(::serde::de::Error::custom(format!(
                        "invalid {} {s:?}: non-canonical rendering of {value}",
                        stringify!($name)
                    )));
                }
                Ok(Self(value))
            }
        }

        impl ::schemars::JsonSchema for $name {
            fn schema_name() -> ::std::string::String {
                <::std::string::String as ::schemars::JsonSchema>::schema_name()
            }

            fn json_schema(
                generator: &mut ::schemars::r#gen::SchemaGenerator,
            ) -> ::schemars::schema::Schema {
                <::std::string::String as ::schemars::JsonSchema>::json_schema(generator)
            }

            fn is_referenceable() -> bool {
                false
            }
        }
    };
}

// ============================================================================
// Validated-string-newtype macro and shared error
// ============================================================================

/// A free-text string field held a NUL (`U+0000`).
///
/// Postgres's `$N::jsonb` cast rejects a NUL outright (jsonb cannot hold one)
/// while SQLite stores it silently, so the two backends would otherwise disagree
/// on what is storable. Every free-text string constructor rejects it at the
/// grammar boundary so both agree — and so no NUL survives to truncate a
/// C-string downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("string contains a NUL character (U+0000), which cannot be stored")]
pub(crate) struct NulError;

/// Reject a NUL (`U+0000`) anywhere in a free-text field. The one shared check
/// behind every free-text constructor, so no string type can drift on whether a
/// NUL is storable.
pub(crate) fn reject_nul(s: &str) -> Result<(), NulError> {
    if s.contains('\u{0000}') {
        Err(NulError)
    } else {
        Ok(())
    }
}

/// Errors from [`validated_string_newtype`](crate::validated_string_newtype)-generated constructors.
///
/// Single shared error type so the macro doesn't have to mint a fresh
/// `<Name>Error` per invocation. Variants cover the length-bound checks
/// the macro can configure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidatedStringError {
    /// Trimmed length was below the configured minimum.
    #[error("{type_name} too short: {len} chars (min {min})")]
    TooShort {
        /// The label of the type (the macro invocation's identifier).
        type_name: &'static str,
        /// Observed character count.
        len: usize,
        /// Configured minimum.
        min: usize,
    },
    /// Length was above the configured maximum.
    #[error("{type_name} too long: {len} chars (max {max})")]
    TooLong {
        /// The label of the type (the macro invocation's identifier).
        type_name: &'static str,
        /// Observed character count.
        len: usize,
        /// Configured maximum.
        max: usize,
    },
    /// The string held a NUL (`U+0000`), which Postgres jsonb cannot store.
    #[error("{type_name} contains a NUL character (U+0000)")]
    ContainsNul {
        /// The label of the type (the macro invocation's identifier).
        type_name: &'static str,
    },
}

/// Emit a `#[serde(transparent)]` string newtype with length-bound
/// validation, smart constructor, `Display`, `AsRef<str>`, and manual
/// `Deserialize` that routes through the smart constructor.
///
/// Parameters:
/// - `name`: the struct identifier (also used as the human-readable
///   label in error messages).
/// - `min = N`: minimum character count after optional trimming.
///   Defaults to 0.
/// - `max = N`: maximum character count. Defaults to `usize::MAX`.
/// - `trim`: when present, the constructor trims surrounding whitespace
///   before counting characters and stores the trimmed form.
///
/// The inner `String` is sealed behind `pub(in crate::grammar)` so the
/// smart constructor is the only path in from outside `crate::grammar`.
#[macro_export]
macro_rules! validated_string_newtype {
    (
        $(#[$meta:meta])*
        $name:ident
        $(, min = $min:expr)?
        $(, max = $max:expr)?
        $(, trim = $trim:literal)?
        $(,)?
    ) => {
        $(#[$meta])*
        #[derive(
            ::std::fmt::Debug,
            ::std::clone::Clone,
            ::std::cmp::PartialEq,
            ::std::cmp::Eq,
            ::std::cmp::PartialOrd,
            ::std::cmp::Ord,
            ::std::hash::Hash,
            ::serde::Serialize,
            ::schemars::JsonSchema,
        )]
        #[serde(transparent)]
        pub struct $name {
            pub(in $crate::grammar) inner: ::std::string::String,
        }

        impl $name {
            const MIN_LEN: usize = $crate::__validated_string_newtype_min!($($min)?);
            const MAX_LEN: usize = $crate::__validated_string_newtype_max!($($max)?);

            #[doc = "Construct, validating length bounds (and trimming if configured)."]
            pub fn new(
                s: impl ::std::convert::AsRef<str>,
            ) -> ::std::result::Result<Self, $crate::grammar::ids::ValidatedStringError> {
                let raw = s.as_ref();
                let candidate = $crate::__validated_string_newtype_maybe_trim!(raw $(, $trim)?);
                if $crate::grammar::ids::reject_nul(candidate).is_err() {
                    return ::std::result::Result::Err(
                        $crate::grammar::ids::ValidatedStringError::ContainsNul {
                            type_name: stringify!($name),
                        },
                    );
                }
                let len = candidate.chars().count();
                if len < Self::MIN_LEN {
                    return ::std::result::Result::Err(
                        $crate::grammar::ids::ValidatedStringError::TooShort {
                            type_name: stringify!($name),
                            len,
                            min: Self::MIN_LEN,
                        },
                    );
                }
                if len > Self::MAX_LEN {
                    return ::std::result::Result::Err(
                        $crate::grammar::ids::ValidatedStringError::TooLong {
                            type_name: stringify!($name),
                            len,
                            max: Self::MAX_LEN,
                        },
                    );
                }
                ::std::result::Result::Ok(Self {
                    inner: candidate.to_string(),
                })
            }

            pub fn as_str(&self) -> &str {
                &self.inner
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> ::std::result::Result<Self, D::Error>
            where
                D: ::serde::Deserializer<'de>,
            {
                let s = <::std::string::String as ::serde::Deserialize>::deserialize(deserializer)?;
                Self::new(s).map_err(::serde::de::Error::custom)
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                ::std::fmt::Display::fmt(&self.inner, f)
            }
        }

        impl ::std::convert::AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.inner
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __validated_string_newtype_min {
    () => {
        0
    };
    ($value:expr) => {
        $value
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __validated_string_newtype_max {
    () => {
        usize::MAX
    };
    ($value:expr) => {
        $value
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __validated_string_newtype_maybe_trim {
    ($s:expr) => {
        $s
    };
    ($s:expr, $do_trim:literal) => {{
        // Only the literal `true` triggers trimming; any other literal is a
        // no-op, so a typo at the call site doesn't silently enable it.
        if $do_trim { $s.trim() } else { $s }
    }};
}

// ============================================================================
// String IDs (macro-generated)
// ============================================================================

/// Maximum length in characters of an identifier-shaped string.
///
/// Wire defense sized for identifiers: a UUID is 36 characters, an email
/// address at most 254.
pub const ID_MAX_LEN: usize = 256;

crate::validated_string_newtype! {
    /// User ID for attribution. Opaque identifier minted by the
    /// auth/identity layer. Core reads it as the author of
    /// `IngestedBy::User` commits.
    UserId, max = ID_MAX_LEN
}

crate::validated_string_newtype! {
    /// Ingester run ID. Opaque identifier minted at the start of an
    /// ingester run (URL worker, Wikidata pipeline, image analysis);
    /// stable for the lifetime of the run. Stored as the author of
    /// `IngestedBy::Ingester` commits.
    IngesterRunId, max = ID_MAX_LEN
}

crate::validated_string_newtype! {
    /// The name of a machine analysis process (e.g. the submit matcher).
    /// Names *what kind* of computation authored a judgment, so consumers can
    /// filter and group machine-authored commits without string parsing.
    AnalyzerProcess, max = ID_MAX_LEN
}

crate::validated_string_newtype! {
    /// The version label of a machine analysis process — the build that ran
    /// it (a git SHA, or a crate version when none was injected; see
    /// [`crate::BUILD_VERSION`]). Paired with [`AnalyzerProcess`] it pins what
    /// produced a judgment, so a re-derivation check knows which code to
    /// re-run.
    AnalyzerVersion, max = ID_MAX_LEN
}

// ============================================================================
// FactId
// ============================================================================

/// Fact identifier — a `u64` newtype.
///
/// Gives ids type-distinct identity from other integer ids; construction is
/// infallible and every `u64` (including zero) is valid. The fact-store trait
/// surface uses plain [`FactId`] with "next id" semantics — `FactId::new(0)`
/// denotes both an empty-store snapshot (exclusive upper bound) and a
/// first-page pagination cursor (inclusive lower bound). See
/// [`FactStore`](crate::store::FactStore) for the full snapshot/cursor
/// convention.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct FactId(u64);

impl FactId {
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for FactId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

// ============================================================================
// SubjectKind
// ============================================================================

/// Which of the three subject id families an id-shaped value belongs to.
///
/// The fact-store grammar carries ids of three families — entity,
/// lifetime-event, and image. [`SubjectKind`] is the family tag for code that
/// needs to name which family an id-shaped value belongs to without naming a
/// concrete id type. Two layers tag values this way:
///
/// - the grammar id-traversal's leaf-lookup error
///   ([`IdMapError::LeafLookup`](crate::grammar::identity::IdMapError::LeafLookup)),
///   when a leaf closure rejects a reference of one family; and
/// - the submit layer's unused-declaration error
///   ([`SubmitError::UnusedDeclaration`](crate::submit::SubmitError::UnusedDeclaration)),
///   when a declaration of one family goes unreferenced.
///
/// It lives here — in the id-model module both layers depend on — because the
/// grammar layer must not depend on `submit` (the dependency runs `submit` →
/// grammar), so the shared tag can't live on either side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectKind {
    Entity,
    Event,
    Image,
}

impl std::fmt::Display for SubjectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Entity => write!(f, "entity"),
            Self::Event => write!(f, "event"),
            Self::Image => write!(f, "image"),
        }
    }
}

// ============================================================================
// CommitId — content-addressed
// ============================================================================

/// Commit identifier — content-addressed.
///
/// The id is the lowercase-hex SHA-256 over a canonical JSON encoding of the
/// commit's contents. Identical contents produce identical ids, so a retried
/// submission of the same bundle returns the existing commit rather than a
/// duplicate.
///
/// # Canonical encoding
///
/// The canonical form is JCS (RFC 8785 JSON Canonicalization Scheme), produced
/// by the [`serde_jcs`] crate. JCS gives one byte-for-byte representation for a
/// JSON value — object keys sorted, numbers normalized, strings escaped per the
/// spec — so independent serializers agree on the bytes that get hashed.
///
/// [`submit::Commit::id`](crate::submit::Commit::id) builds the following
/// logical JSON document and feeds the JCS encoding to SHA-256:
///
/// ```json
/// {
///   "author": "<author>",
///   "entities": [ <decl>, <decl>, ... ],
///   "events": [ <decl>, <decl>, ... ],
///   "facts": [ <fact>, <fact>, ... ],
///   "images": [ <decl>, <decl>, ... ],
///   "recorded_at": "<rfc3339-seconds>"
/// }
/// ```
///
/// - `<author>` is the canonical form of the author identity (e.g.
///   `user:<UserId>`, `ingester:<IngesterRunId>`, or
///   `analyzer:<process>@<version>`).
/// - The three declaration lists (`entities`, `events`, `images`) are
///   serialized in their declared positional order — not sorted — because a
///   fact references a subject by its index into that order; reordering a list
///   would silently rebind every index pointing into it. Each `<decl>` is a
///   [`Decl`](crate::submit::Decl): `Local` (mint a fresh subject) or
///   `Existing(id)` (adopt a caller-named one). The lists are part of the
///   address because each index binds a mint-vs-adopt-existing identity
///   decision — commit content. Two commits with byte-identical facts but
///   differing decls assert different identities and must content-address
///   apart.
/// - `<rfc3339-seconds>` quantizes the commit time to whole-second
///   granularity. Encoded as a string rather than a chrono-typed value so the
///   wire form is fixed regardless of how `DateTime<Utc>` serializes.
/// - Each `<fact>` is one of the commit's typed
///   [`SubmitFact`](crate::submit::SubmitFact) values, serialized through its
///   derived `Serialize` impl. The facts array is sorted by each fact's own JCS
///   encoding before assembly so submission order doesn't perturb the id.
///
/// The hash is full SHA-256 (no truncation); the wire shape is 64 lowercase hex
/// characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct CommitId {
    inner: String,
}

impl<'de> Deserialize<'de> for CommitId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(s).map_err(serde::de::Error::custom)
    }
}

impl CommitId {
    /// Adopt an already-validated `CommitId` string. Wire input goes
    /// through [`CommitId::try_from`] which validates length and hex
    /// shape; this entry point exists for in-crate callers that compute
    /// the id locally and want a typed wrapper without re-hashing.
    pub fn parse(id: impl Into<String>) -> Result<Self, CommitIdError> {
        let s = id.into();
        if s.len() != COMMIT_ID_HEX_LEN {
            return Err(CommitIdError::WrongLength {
                len: s.len(),
                expected: COMMIT_ID_HEX_LEN,
            });
        }
        if !s
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(CommitIdError::NotLowercaseHex);
        }
        Ok(Self { inner: s })
    }

    /// The hex digest as a string slice.
    pub fn as_str(&self) -> &str {
        &self.inner
    }
}

impl std::convert::TryFrom<String> for CommitId {
    type Error = CommitIdError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(s)
    }
}

impl std::fmt::Display for CommitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl AsRef<str> for CommitId {
    fn as_ref(&self) -> &str {
        &self.inner
    }
}

/// Length in characters of the hex-encoded SHA-256 used for [`CommitId`].
pub const COMMIT_ID_HEX_LEN: usize = 64;

/// Errors from [`CommitId::parse`] / `TryFrom<String>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommitIdError {
    /// The supplied string did not have the expected hex-digest length.
    #[error("commit id has wrong length: {len} chars (expected {expected})")]
    WrongLength { len: usize, expected: usize },
    /// The supplied string contained non-hex or uppercase-hex bytes.
    #[error("commit id must be lowercase hex (a-f, 0-9)")]
    NotLowercaseHex,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_id_parse_rejects_short() {
        assert!(matches!(
            CommitId::parse("deadbeef"),
            Err(CommitIdError::WrongLength { .. })
        ));
    }

    #[test]
    fn commit_id_parse_rejects_uppercase_hex() {
        let upper = "A".repeat(COMMIT_ID_HEX_LEN);
        assert_eq!(CommitId::parse(upper), Err(CommitIdError::NotLowercaseHex));
    }

    #[test]
    fn commit_id_parse_accepts_round_trip_of_self_produced_hex() -> Result<(), CommitIdError> {
        // Hex sequence matching the SHA-256 hex shape (length 64,
        // lowercase). The exact bytes don't matter — `parse` is purely
        // shape-validation.
        let hex = "0".repeat(COMMIT_ID_HEX_LEN);
        let parsed = CommitId::parse(hex.clone())?;
        assert_eq!(parsed.as_str(), hex);
        Ok(())
    }

    // --- string-id validation ---

    #[test]
    fn user_id_new_rejects_a_nul() {
        assert!(matches!(
            UserId::new("ali\u{0}ce"),
            Err(ValidatedStringError::ContainsNul { .. })
        ));
    }

    /// The error text, not just the failure: a bare-`String` deserialize has
    /// no other way to fail, so only the message shows the NUL check ran.
    fn deserialize_error<T: serde::de::DeserializeOwned>(wire: serde_json::Value) -> String {
        match serde_json::from_value::<T>(wire) {
            Ok(_) => String::new(),
            Err(e) => e.to_string(),
        }
    }

    /// The wire is the path that matters: a `UserId` rides inside
    /// `commit_json.author`, so a NUL arriving over submit reaches a Postgres
    /// `jsonb` cast that refuses it while SQLite takes it silently.
    #[test]
    fn user_id_deserialize_rejects_a_nul() {
        assert!(
            deserialize_error::<UserId>(serde_json::json!("ali\u{0}ce")).contains("NUL"),
            "a NUL in the wire form must be refused by the UserId constructor"
        );
    }

    #[test]
    fn ingester_run_id_deserialize_rejects_a_nul() {
        assert!(
            deserialize_error::<IngesterRunId>(serde_json::json!("run\u{0}1")).contains("NUL"),
            "a NUL in the wire form must be refused by the IngesterRunId constructor"
        );
    }

    #[test]
    fn analyzer_process_takes_the_cap_length_and_rejects_one_char_past_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let at_cap = "x".repeat(ID_MAX_LEN);
        assert_eq!(
            AnalyzerProcess::new(&at_cap)?.as_str().chars().count(),
            ID_MAX_LEN
        );

        let over_cap = "x".repeat(ID_MAX_LEN + 1);
        assert!(matches!(
            AnalyzerProcess::new(&over_cap),
            Err(ValidatedStringError::TooLong { len, max, .. })
                if len == ID_MAX_LEN + 1 && max == ID_MAX_LEN
        ));
        assert!(
            deserialize_error::<AnalyzerVersion>(serde_json::json!(over_cap)).contains("too long"),
            "an over-cap wire form must be refused by the AnalyzerVersion constructor"
        );
        Ok(())
    }

    // --- subject_id_newtype wire behavior ---

    crate::subject_id_newtype!(
        TestUnsignedId,
        u64,
        "unsigned",
        "A u64-backed instantiation for the wire-behavior tests."
    );
    crate::subject_id_newtype!(
        TestSignedId,
        i64,
        "signed",
        "An i64-backed instantiation for the wire-behavior tests."
    );

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn subject_id_serializes_as_canonical_decimal_string() -> TestResult {
        assert_eq!(
            serde_json::to_value(TestUnsignedId(7))?,
            serde_json::json!("7")
        );
        assert_eq!(
            serde_json::to_value(TestSignedId(-7))?,
            serde_json::json!("-7")
        );
        Ok(())
    }

    #[test]
    fn subject_id_round_trips_canonical_input() -> TestResult {
        assert_eq!(
            serde_json::from_value::<TestUnsignedId>(serde_json::json!("7"))?,
            TestUnsignedId(7)
        );
        assert_eq!(
            serde_json::from_value::<TestSignedId>(serde_json::json!("-7"))?,
            TestSignedId(-7)
        );
        Ok(())
    }

    /// Every parseable-but-non-canonical spelling is refused, so one id has
    /// exactly one wire form at every string-decoded position (path params,
    /// cursors, stored JSON alike).
    #[test]
    fn subject_id_rejects_non_canonical_spellings() {
        for aliased in ["007", "+7", " 7", "7 ", ""] {
            assert!(
                serde_json::from_value::<TestUnsignedId>(serde_json::json!(aliased)).is_err(),
                "u64 spelling {aliased:?} must be rejected"
            );
        }
        for aliased in ["-0", "+7", "007", "-07"] {
            assert!(
                serde_json::from_value::<TestSignedId>(serde_json::json!(aliased)).is_err(),
                "i64 spelling {aliased:?} must be rejected"
            );
        }
        // An integer-shaped wire value is the wrong type outright.
        assert!(serde_json::from_value::<TestUnsignedId>(serde_json::json!(7)).is_err());
    }
}
