use pulldown_cmark::{Event, Options, Parser, html};

const OPEN: &str = "{{";
const CLOSE: &str = "}}";

/// Prose that has to read identically on more than one content page, so that a
/// fact about the world has exactly one edit site.
///
/// Spelled out rather than derived from the directory listing: the set is small
/// and worth knowing at a glance.
const SNIPPETS: &[(&str, &str)] = &[(
    "foundation_status",
    include_str!("../content/snippets/foundation-status.md"),
)];

fn snippet(token: &str) -> Option<&'static str> {
    SNIPPETS
        .iter()
        .find_map(|(name, content)| (*name == token).then_some(*content))
}

/// Splice shared snippets into markdown, replacing each `{{token}}` with the
/// content of the matching entry in [`SNIPPETS`].
///
/// A token no snippet defines is copied through verbatim, so a typo reaches a
/// reader as visible braces and a test can name it, rather than deleting the
/// sentence it was meant to carry.
pub fn expand_snippets(markdown: &str) -> String {
    let mut expanded = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some((before, after_open)) = rest.split_once(OPEN) {
        expanded.push_str(before);
        let Some((token, after_close)) = after_open.split_once(CLOSE) else {
            expanded.push_str(OPEN);
            rest = after_open;
            continue;
        };
        match snippet(token.trim()) {
            // Trailing newlines belong to the snippet file, not to the
            // paragraph the snippet lands in.
            Some(content) => expanded.push_str(content.trim_end()),
            None => {
                expanded.push_str(OPEN);
                expanded.push_str(token);
                expanded.push_str(CLOSE);
            }
        }
        rest = after_close;
    }
    expanded.push_str(rest);
    expanded
}

/// Convert markdown to HTML using pulldown-cmark.
///
/// Raw HTML in the markdown source is stripped (not passed through) to prevent
/// any possibility of XSS, even if the input is trusted compile-time content.
pub fn md_to_html(markdown: &str) -> String {
    let parser = Parser::new_ext(markdown, Options::empty())
        .filter(|event| !matches!(event, Event::Html(_) | Event::InlineHtml(_)));
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    html_output
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every markdown source shipped to a reader, by the name to blame in a
    /// failure.
    const CONTENT_PAGES: &[(&str, &str)] = &[
        ("about.md", include_str!("../content/about.md")),
        (
            "related-work.md",
            include_str!("../content/related-work.md"),
        ),
        ("faq.md", include_str!("../content/faq.md")),
    ];

    /// The first `{{…}}` still standing after expansion, if any.
    fn first_unexpanded_token(markdown: &str) -> Option<&str> {
        let (_, after_open) = markdown.split_once(OPEN)?;
        Some(
            after_open
                .split_once(CLOSE)
                .map_or(after_open, |(token, _)| token),
        )
    }

    #[test]
    fn a_known_token_is_replaced_by_its_snippet() {
        let expanded = expand_snippets("Before. {{foundation_status}} After.");
        assert!(expanded.contains("EIN 42-3826455"), "{expanded}");
        assert!(!expanded.contains(OPEN), "{expanded}");
    }

    /// A snippet lands mid-paragraph, where the file's own trailing newline
    /// would break the sentence in two.
    #[test]
    fn a_spliced_snippet_carries_no_trailing_whitespace() {
        let expanded = expand_snippets("Before. {{foundation_status}} After.");
        assert!(expanded.lines().count() == 1, "{expanded}");
        assert!(expanded.ends_with(" After."), "{expanded}");
    }

    /// Guards against a typo'd token silently deleting the prose it stood for,
    /// which is what lets the page-wide check below see it.
    #[test]
    fn an_undefined_token_is_left_in_place() {
        assert_eq!(
            expand_snippets("Before. {{no_such_snippet}} After."),
            "Before. {{no_such_snippet}} After."
        );
    }

    /// A misspelled token ships literal braces to a reader, and nothing else
    /// would notice: the page still renders and the snippet is small enough to
    /// miss in review.
    #[test]
    fn no_content_page_names_a_snippet_that_does_not_exist() {
        let defined: Vec<&str> = SNIPPETS.iter().map(|(name, _)| *name).collect();
        for (page, source) in CONTENT_PAGES {
            let expanded = expand_snippets(source);
            let unexpanded = first_unexpanded_token(&expanded);
            assert_eq!(
                unexpanded, None,
                "{page} names snippet {unexpanded:?}, which no entry defines; defined: {defined:?}"
            );
        }
    }
}
