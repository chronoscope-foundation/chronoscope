use pulldown_cmark::{Event, Options, Parser, html};

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
