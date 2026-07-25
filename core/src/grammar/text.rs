//! The grammar's free-text value.

/// Maximum length for a [`Text`] in characters, matching
/// [`EXCERPT_MAX_LEN`](crate::grammar::citations::EXCERPT_MAX_LEN).
pub const TEXT_MAX_LEN: usize = 4096;

crate::validated_string_newtype! {
    /// Free text as the source recorded it — a title, a name, a description.
    ///
    /// The constructor rejects a NUL (`U+0000`), which is what keeps the SQLite
    /// and Postgres stores agreeing on what a fact may hold: Postgres's `jsonb`
    /// cast refuses one outright, SQLite takes it silently.
    ///
    /// [`TEXT_MAX_LEN`] bounds the length: wire defense at the submit
    /// boundary, sized to match [`Excerpt`](crate::grammar::citations::Excerpt),
    /// the grammar's other free-form string. Within that bound the value is
    /// stored verbatim — no trim, no minimum — so what a source recorded
    /// survives round-tripping.
    Text, max = TEXT_MAX_LEN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_rejects_nul() {
        assert!(matches!(
            Text::new("a\u{0}b"),
            Err(crate::grammar::ids::ValidatedStringError::ContainsNul { .. })
        ));
    }

    #[test]
    fn text_accepts_a_nul_free_value_verbatim() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(Text::new("  p. 142 ")?.as_str(), "  p. 142 ");
        Ok(())
    }

    #[test]
    fn text_takes_the_cap_length_and_rejects_one_char_past_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let at_cap = "x".repeat(TEXT_MAX_LEN);
        assert_eq!(Text::new(&at_cap)?.as_str().chars().count(), TEXT_MAX_LEN);

        let over_cap = "x".repeat(TEXT_MAX_LEN + 1);
        assert!(matches!(
            Text::new(&over_cap),
            Err(crate::grammar::ids::ValidatedStringError::TooLong { len, max, .. })
                if len == TEXT_MAX_LEN + 1 && max == TEXT_MAX_LEN
        ));
        Ok(())
    }
}
