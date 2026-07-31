// Build-time rendering of the static content pages: markdown in, HTML and
// generated Rust out.
//
// Nothing in the shipping crate declares it. `build.rs` `include!`s it to do
// the work, and `tests/content_render.rs` declares it as a module to give the
// suite below a runner (cargo does not run a build script's tests). So nothing
// here may name a `crate::` path, and each relative `include!`/`include_str!`
// resolves against *this* file's directory under either includer.
//
// The test side uses `#[path] mod` because rustfmt follows module declarations
// and not `include!`: that is what puts this file under the gate's `cargo fmt`
// check.

use std::collections::BTreeSet;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};

const FAQ_MARKDOWN: &str = include_str!("../content/faq.md");

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
fn expand_snippets(markdown: &str) -> String {
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

/// Markup the author wrote by hand instead of in markdown.
///
/// These pages are markdown, and raw markup in them is unsupported. Both render
/// paths, the article body and the FAQ answer, filter on this one predicate, so
/// the rule has a single home.
fn is_raw_html(event: &Event<'_>) -> bool {
    matches!(event, Event::Html(_) | Event::InlineHtml(_))
}

/// Convert markdown to HTML using pulldown-cmark, dropping any raw markup the
/// source carried: the content pages are written in markdown, and hand-written
/// HTML in them is not a supported way to say anything.
fn md_to_html(markdown: &str) -> String {
    let parser = Parser::new_ext(markdown, Options::empty()).filter(|event| !is_raw_html(event));
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    html_output
}

/// Render one content page into the HTML its component drops into `inner_html`.
fn render_article(markdown: &str) -> String {
    md_to_html(&expand_snippets(markdown))
}

/// A content page: the markdown it is written in, the heading it renders under,
/// and the stem every other name derives from.
struct ArticleSource {
    stem: &'static str,
    title: &'static str,
    markdown: &'static str,
}

/// Paths `web/src/app.rs` routes that no article row accounts for: the map at
/// the site root, and the FAQ, which is an accordion rather than a rendered
/// body. A route added there is a route added here.
const NON_ARTICLE_ROUTES: &[&str] = &["/", "/faq"];

/// The path an article is served at.
///
/// `Article::path()` in `web/src/content.rs` hands out this same string: it is
/// what [`generated_articles_source`] writes into each generated constant.
fn article_path(stem: &str) -> String {
    format!("/{stem}")
}

/// Whether the stem opens a name Rust will accept.
///
/// `2026-roadmap` derives `pub const 2026_ROADMAP`, a number literal where a
/// name has to be, and `-roadmap` derives nothing at all. Caught here, that is
/// a message against the row; caught by the compiler, it is a syntax error
/// inside a generated file under `OUT_DIR` that nobody edits.
const fn stem_opens_with_a_letter(stem: &str) -> bool {
    let bytes = stem.as_bytes();
    !bytes.is_empty() && bytes[0].is_ascii_lowercase()
}

/// Whether `route` is exactly the path an article with this stem is served at.
const fn is_path_of_stem(route: &str, stem: &str) -> bool {
    let route = route.as_bytes();
    let stem = stem.as_bytes();
    if route.len() != stem.len() + 1 || route[0] != b'/' {
        return false;
    }
    let mut at = 0;
    while at < stem.len() {
        if route[at + 1] != stem[at] {
            return false;
        }
        at += 1;
    }
    true
}

/// Whether an article with this stem would be served at a path the app already
/// routes elsewhere.
///
/// The other two rules pass a row like `"faq" => …`, and the constant it
/// generates routes cleanly alongside the hand-written `/faq`. Which of the two
/// pages a reader lands on is then whichever route the router happened to match
/// first.
const fn stem_shadows_a_route(stem: &str) -> bool {
    let mut at = 0;
    while at < NON_ARTICLE_ROUTES.len() {
        if is_path_of_stem(NON_ARTICLE_ROUTES[at], stem) {
            return true;
        }
        at += 1;
    }
    false
}

/// Whether the stem carries only `a-z`, `0-9` and `-`.
///
/// Narrower than [`constant_name`] needs: `how_it_works` and `HowItWorks` would
/// each derive a perfectly good Rust name. The stem is also the page's public
/// URL segment, and the narrow charset is what keeps one page from being
/// spelled two ways in a URL people paste.
const fn stem_is_url_charset(stem: &str) -> bool {
    let bytes = stem.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && byte != b'-' {
            return false;
        }
        at += 1;
    }
    true
}

/// Declare the article pages, one `stem => heading` per page.
///
/// A row is a stem and the heading the page renders under. The stem is the
/// whole of the page's identity: the markdown it reads, the file `build.rs`
/// renders it into, the constant that reads that file back and the URL it is
/// served at are all built from it here. There is no slot to put another page's
/// prose in.
///
/// The three rules a stem is held to are asserted per row, so a rejected stem
/// is reported here with the reason it was rejected. Const evaluation has no
/// formatting, so each message names the rule rather than the character that
/// broke it.
macro_rules! articles {
    ($($stem:literal => $title:literal),* $(,)?) => {
        const ARTICLES: &[ArticleSource] = &[$(ArticleSource {
            stem: $stem,
            title: $title,
            markdown: include_str!(concat!("../content/", $stem, ".md")),
        },)*];

        $(
            const _: () = assert!(
                stem_opens_with_a_letter($stem),
                concat!(
                    "article stem `", $stem, "` has to open with a lowercase ASCII letter. \
                     The stem names both the constant `web/src/content.rs` publishes the page \
                     under and the page's URL segment: a digit or `-` cannot open a Rust name, \
                     and anything else is outside the `a-z`, `0-9` and `-` the rest of the stem \
                     is held to",
                ),
            );

            const _: () = assert!(
                stem_is_url_charset($stem),
                concat!(
                    "article stem `", $stem, "` carries a character outside `a-z`, `0-9` and \
                     `-`. The stem is the page's public URL segment as well as the name \
                     `web/src/content.rs` publishes it under, so it is held to the URL \
                     convention: `how-it-works`, not `how_it_works` or `HowItWorks`",
                ),
            );

            const _: () = assert!(
                !stem_shadows_a_route($stem),
                concat!(
                    "article stem `", $stem, "` is already a path `web/src/app.rs` serves \
                     something else at. Both routes would claim it, and which page a reader \
                     gets would come down to the order they are declared in",
                ),
            );
        )*
    };
}

articles! {
    "about" => "About Chronoscope",
    "related-work" => "Related work and where we fit",
}

/// The file `build.rs` renders a page into.
fn output_file(stem: &str) -> String {
    format!("{stem}.html")
}

/// The constant `web/src/content.rs` publishes a page as, in its `articles`
/// module: `related-work` is served up as `articles::RELATED_WORK`.
fn constant_name(stem: &str) -> String {
    stem.to_uppercase().replace('-', "_")
}

/// Emit the article pages as Rust source: one `pub const` per row of
/// [`ARTICLES`], over the `Article` type `web/src/content.rs` declares, for
/// inclusion into that file's `articles` module.
///
/// Generated rather than hand-written so the constant, the file it reads, and
/// the markdown that file was rendered from all come from one row. Two
/// hand-written constants can be swapped onto each other's files and still
/// compile, and the pages that result read perfectly well under the wrong
/// heading.
fn generated_articles_source() -> String {
    let mut source = String::new();
    for article in ARTICLES {
        let name = constant_name(article.stem);
        let path = article_path(article.stem);
        let title = article.title;
        let file = output_file(article.stem);
        source.push_str(&format!(
            "pub const {name}: Article = Article {{ path: {path:?}, title: {title:?}, \
             html: RenderedHtml::from_build_output(include_str!(concat!(env!(\"OUT_DIR\"), \"/{file}\"))) }};\n"
        ));
    }
    source
}

/// Emit the `article_routes!` macro `web/src/app.rs` invokes: the hand-written
/// routes it passes in, followed by one `<ArticleRoute/>` per row of
/// [`ARTICLES`].
///
/// The routes are generated for the reason the constants are. Written by hand,
/// a route per page is a line to forget, and the page it would have served is
/// then reachable only by typing its URL: the drawer still lists it, the links
/// in the content still name it, and every guard over `ARTICLES` still passes,
/// because they all read the same table the missing route was supposed to
/// serve.
///
/// It expands to element syntax rather than to a value: `<Routes>` takes its
/// children as a statically typed tuple, so a route has to reach it as a child
/// node of the `view!` invocation. That is why the macro wraps the whole
/// `<Routes>` element instead of being called inside one.
fn generated_route_macro_source() -> String {
    let routes: Vec<String> = ARTICLES
        .iter()
        .map(|article| {
            format!(
                "                <ArticleRoute article=$crate::content::articles::{}/>",
                constant_name(article.stem)
            )
        })
        .collect();
    ROUTE_MACRO_TEMPLATE.replace("{ARTICLE_ROUTES}", &routes.join("\n"))
}

/// The macro source, with the per-row lines standing out of the way behind a
/// placeholder, so what `web/src/app.rs` invokes reads here as it will read
/// there.
const ROUTE_MACRO_TEMPLATE: &str = r"macro_rules! article_routes {
    (fallback = $fallback:expr; $($route:tt)*) => {
        ::leptos::view! {
            <Routes fallback=$fallback>
                $($route)*
{ARTICLE_ROUTES}
            </Routes>
        }
    };
}
pub(crate) use article_routes;
";

/// Stem for a heading with no alphanumerics at all, so every entry has a
/// non-empty anchor.
const FALLBACK_SLUG: &str = "question";

/// Quote characters that vanish rather than split the word around them, so
/// "aren't" slugs to `arent`, the usual heading-anchor convention.
const ELIDED_IN_SLUG: [char; 2] = ['\'', '\u{2019}'];

struct FaqCategory<E> {
    title: String,
    entries: Vec<E>,
}

struct FaqEntry {
    question: String,
    /// Anchor for this entry, unique across the whole page.
    slug: String,
    answer_html: String,
}

/// An entry awaiting its anchor, which can only be settled once every heading
/// on the page has been seen.
struct PendingEntry {
    question: String,
    /// The anchor the heading pinned with `{#id}`, if it pinned one.
    pinned: Option<String>,
    answer_html: String,
}

/// A `## Question` heading, open until its answer content has been collected.
struct OpenQuestion {
    text: String,
    pinned: Option<String>,
}

/// Lowercase, drop apostrophes, collapse each remaining run of
/// non-alphanumerics to a single `-`, and trim `-` off both ends.
///
/// ASCII-only, so the result needs no escaping as either an `id` or a URL
/// fragment. Pinned anchors go through this too: whatever the markdown author
/// typed still has to be usable as both.
fn slugify(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !ELIDED_IN_SLUG.contains(&ch) && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        FALLBACK_SLUG.to_string()
    } else {
        slug.to_string()
    }
}

/// Claim `base` as an anchor, numbering it `-2`, `-3`, … past anything already
/// claimed.
///
/// Two questions can slug alike (differing only in punctuation, say), and a
/// numbered form can itself collide with a question that slugs to it directly.
/// Walking up from the document-order position keeps the assignment stable for
/// a given markdown source.
fn claim_slug(base: &str, claimed: &mut BTreeSet<String>) -> String {
    let mut candidate = base.to_string();
    let mut suffix = 1;
    while !claimed.insert(candidate.clone()) {
        suffix += 1;
        candidate = format!("{base}-{suffix}");
    }
    candidate
}

/// Render accumulated answer events to HTML and push onto the current category.
fn flush_answer<'a>(
    question: Option<OpenQuestion>,
    answer_events: impl Iterator<Item = Event<'a>>,
    categories: &mut [FaqCategory<PendingEntry>],
) {
    if let Some(question) = question {
        let mut answer_html = String::new();
        html::push_html(
            &mut answer_html,
            answer_events.filter(|event| !is_raw_html(event)),
        );
        let answer_html = answer_html.trim().to_string();
        if !answer_html.is_empty()
            && let Some(cat) = categories.last_mut()
        {
            cat.entries.push(PendingEntry {
                question: question.text,
                pinned: question.pinned,
                answer_html,
            });
        }
    }
}

/// Parse FAQ markdown using pulldown-cmark events.
///
/// Expected structure: `# Category` headings containing `## Question` headings
/// followed by answer content. Uses the pulldown-cmark parser to handle all
/// markdown edge cases correctly instead of fragile string splitting.
///
/// Anchors are assigned here rather than at render time: a slug is a
/// page-global id, so uniqueness is only enforceable where every category is
/// in view.
fn parse_faq(markdown: &str) -> Vec<FaqCategory<FaqEntry>> {
    assign_slugs(parse_entries(markdown))
}

/// Collect categories and their questions, each carrying whatever anchor its
/// heading pinned.
fn parse_entries(markdown: &str) -> Vec<FaqCategory<PendingEntry>> {
    let parser = Parser::new_ext(markdown, Options::ENABLE_HEADING_ATTRIBUTES);
    let mut categories: Vec<FaqCategory<PendingEntry>> = Vec::new();
    let mut current_heading_level: Option<HeadingLevel> = None;
    let mut heading_text = String::new();
    let mut heading_id: Option<String> = None;
    let mut answer_events: Vec<Event<'_>> = Vec::new();
    let mut current_question: Option<OpenQuestion> = None;

    for event in parser {
        match &event {
            Event::Start(Tag::Heading { level, id, .. }) => {
                flush_answer(
                    current_question.take(),
                    answer_events.drain(..),
                    &mut categories,
                );
                current_heading_level = Some(*level);
                heading_id = id.as_ref().map(|id| id.to_string());
                heading_text.clear();
            }
            Event::Text(text) if current_heading_level.is_some() => {
                heading_text.push_str(text);
            }
            Event::End(TagEnd::Heading(_)) => {
                let pinned = heading_id.take();
                match current_heading_level.take() {
                    Some(HeadingLevel::H1) => {
                        categories.push(FaqCategory {
                            title: heading_text.clone(),
                            entries: Vec::new(),
                        });
                    }
                    Some(HeadingLevel::H2) => {
                        current_question = Some(OpenQuestion {
                            text: heading_text.clone(),
                            pinned,
                        });
                    }
                    _ => {}
                }
                heading_text.clear();
            }
            _ if current_question.is_some() && current_heading_level.is_none() => {
                answer_events.push(event);
            }
            _ => {}
        }
    }

    flush_answer(
        current_question.take(),
        answer_events.into_iter(),
        &mut categories,
    );
    categories
}

/// Give every entry its page-global anchor.
///
/// A pinned `{#id}` is a promise to whoever already linked to it, so pinned
/// anchors are claimed first and a derived slug wanting the same name numbers
/// itself out of the way. Two headings pinning one id is a content bug; it
/// settles like any other collision, with document order winning.
fn assign_slugs(mut pending: Vec<FaqCategory<PendingEntry>>) -> Vec<FaqCategory<FaqEntry>> {
    let mut claimed_slugs: BTreeSet<String> = BTreeSet::new();
    for entry in pending.iter_mut().flat_map(|cat| cat.entries.iter_mut()) {
        if let Some(pinned) = &mut entry.pinned {
            *pinned = claim_slug(&slugify(pinned), &mut claimed_slugs);
        }
    }

    let mut anchored = Vec::with_capacity(pending.len());
    for cat in pending {
        let mut entries = Vec::with_capacity(cat.entries.len());
        for PendingEntry {
            question,
            pinned,
            answer_html,
        } in cat.entries
        {
            let slug =
                pinned.unwrap_or_else(|| claim_slug(&slugify(&question), &mut claimed_slugs));
            entries.push(FaqEntry {
                question,
                slug,
                answer_html,
            });
        }
        anchored.push(FaqCategory {
            title: cat.title,
            entries,
        });
    }
    anchored
}

/// Emit the parsed FAQ as Rust source: the `FAQ` table `web/src/content.rs`
/// `include!`s, over the borrowed `FaqCategory`/`FaqEntry` declared there.
/// Those declarations and this emitter are a drift pair; a mismatch fails to
/// compile.
///
/// Answers go out wrapped in `RenderedHtml`, which only `content.rs` can mint,
/// so the whole table is emitted for inclusion there and published `pub` for
/// the page to read.
///
/// Every string goes out through `{:?}`, whose `str` impl is a valid Rust
/// literal for any input. That is what makes the emitter correct: each rendered
/// link carries `"`, and the prose carries typographic quotes and dashes.
fn generated_faq_source(markdown: &str) -> String {
    let mut source = String::from("pub static FAQ: &[FaqCategory] = &[\n");
    for cat in parse_faq(&expand_snippets(markdown)) {
        source.push_str(&format!(
            "    FaqCategory {{\n        title: {:?},\n        entries: &[\n",
            cat.title
        ));
        for entry in cat.entries {
            source.push_str(&format!(
                "            FaqEntry {{ question: {:?}, slug: {:?}, answer_html: RenderedHtml::from_build_output({:?}) }},\n",
                entry.question, entry.slug, entry.answer_html
            ));
        }
        source.push_str("        ],\n    },\n");
    }
    source.push_str("];\n");
    source
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every markdown source shipped to a reader, by the name to blame in a
    /// failure.
    fn content_pages() -> Vec<(String, &'static str)> {
        ARTICLES
            .iter()
            .map(|article| (format!("{}.md", article.stem), article.markdown))
            .chain([("faq.md".to_string(), FAQ_MARKDOWN)])
            .collect()
    }

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
        for (page, source) in content_pages() {
            let expanded = expand_snippets(source);
            let unexpanded = first_unexpanded_token(&expanded);
            assert_eq!(
                unexpanded, None,
                "{page} names snippet {unexpanded:?}, which no entry defines; defined: {defined:?}"
            );
        }
    }

    /// Markdown carrying raw markup, the tag that must not survive it, and the
    /// prose that follows it.
    struct RawHtmlCase {
        markdown: &'static str,
        tag: &'static str,
        trailing: &'static str,
    }

    /// Raw markup in each of the two shapes the parser reports: inside a
    /// paragraph, which arrives as `InlineHtml`, and alone between blank lines,
    /// which arrives as `Html`. [`is_raw_html`] matches both, and either arm
    /// going missing is invisible without a case for each.
    ///
    /// The prose *after* the markup is what says the filter dropped one event
    /// rather than swallowing the rest of the document with it.
    const RAW_HTML_CASES: &[RawHtmlCase] = &[
        RawHtmlCase {
            markdown: "Before <span class=\"x\">markup</span> after.\n",
            tag: "<span",
            trailing: "after.",
        },
        RawHtmlCase {
            markdown: "Before\n\n<div>raw</div>\n\nAfter\n",
            tag: "<div",
            trailing: "<p>After</p>",
        },
    ];

    /// The content pages are markdown, so hand-written markup in one says
    /// nothing the renderer supports. No page uses raw HTML today, which is
    /// exactly why losing the filter would go unnoticed.
    #[test]
    fn raw_html_in_a_content_page_is_dropped_rather_than_rendered() {
        for case in RAW_HTML_CASES {
            let markdown = case.markdown;
            let rendered = render_article(markdown);
            assert!(
                !rendered.contains(case.tag),
                "{markdown:?} rendered {rendered:?}"
            );
            assert!(
                rendered.contains("Before"),
                "{markdown:?} rendered {rendered:?}"
            );
            assert!(
                rendered.contains(case.trailing),
                "{markdown:?} lost the content after the markup: {rendered:?}"
            );
        }
    }

    /// The FAQ answers go through a render path of their own, so the filter has
    /// to reach both.
    #[test]
    fn raw_html_in_a_faq_answer_is_dropped_rather_than_rendered() {
        for case in RAW_HTML_CASES {
            let markdown = case.markdown;
            let cats = parse_faq(&format!("# Cat\n\n## Q\n\n{markdown}"));
            let answer = cats
                .first()
                .and_then(|cat| cat.entries.first())
                .map_or("", |entry| entry.answer_html.as_str());
            assert!(
                !answer.is_empty(),
                "{markdown:?} produced no FAQ answer to check"
            );
            assert!(
                !answer.contains(case.tag),
                "{markdown:?} answered {answer:?}"
            );
            assert!(
                answer.contains("Before"),
                "{markdown:?} answered {answer:?}"
            );
            assert!(
                answer.contains(case.trailing),
                "{markdown:?} lost the answer content after the markup: {answer:?}"
            );
        }
    }

    /// `web/src/content.rs` publishes exactly what this emitter writes, so a row
    /// this emitter skipped is a page missing from the crate, and a constant
    /// pointed at another row's file or heading is a page that reads perfectly
    /// well under the wrong title. Neither shows up downstream: an unrouted page
    /// is nobody's compile error, and a swapped one still renders.
    ///
    /// Two rows publishing one name is not among what this can see. The charset
    /// a stem is held to leaves [`constant_name`] injective, so it takes a row
    /// copy-pasted stem and all, and that is a redefinition in the generated
    /// source: the crate stops compiling before any test runs.
    #[test]
    fn every_article_row_publishes_a_constant_over_its_own_page() {
        let source = generated_articles_source();
        for article in ARTICLES {
            let name = constant_name(article.stem);
            let line = source
                .lines()
                .find(|line| line.starts_with(&format!("pub const {name}:")))
                .unwrap_or_default();
            assert!(
                !line.is_empty(),
                "{name} is published by no line of the emitted source: {source}"
            );
            let file = output_file(article.stem);
            assert!(
                line.contains(&format!("\"/{file}\"")),
                "{name} should read {file}, the page `build.rs` rendered for it: {line}"
            );
            assert!(
                line.contains(&format!("path: {:?}", article_path(article.stem))),
                "{name} should carry the URL its own page is served at: {line}"
            );
            assert!(
                line.contains(&format!("title: {:?}", article.title)),
                "{name} should carry its own heading: {line}"
            );
        }

        assert_eq!(
            source.lines().count(),
            ARTICLES.len(),
            "the emitted source carries lines no article row accounts for: {source}"
        );
    }

    /// The macro is what leaves no per-page route to forget, so a row it
    /// skipped puts the hole straight back: a page the drawer lists, the
    /// content links to, and nothing serves.
    #[test]
    fn the_emitted_macro_routes_every_article_row() {
        let source = generated_route_macro_source();
        for article in ARTICLES {
            let name = constant_name(article.stem);
            assert!(
                source.contains(&format!(
                    "<ArticleRoute article=$crate::content::articles::{name}/>"
                )),
                "{name} is routed by no line of the emitted macro: {source}"
            );
        }
        assert_eq!(
            source.matches("<ArticleRoute").count(),
            ARTICLES.len(),
            "the emitted macro routes something no article row accounts for: {source}"
        );
    }

    /// An article whose markdown was emptied still builds, still routes, and
    /// still renders: the reader just gets a heading over nothing.
    #[test]
    fn every_article_renders_a_body() {
        for article in ARTICLES {
            let stem = article.stem;
            assert!(!article.title.is_empty(), "{stem} has no heading");
            let rendered = render_article(article.markdown);
            assert!(
                rendered.contains("</p>"),
                "{stem}.md rendered no prose at all: {rendered:?}"
            );
        }
    }

    /// Slugs of every entry, in document order across all categories.
    fn all_slugs(cats: &[FaqCategory<FaqEntry>]) -> Vec<&str> {
        cats.iter()
            .flat_map(|c| c.entries.iter().map(|e| e.slug.as_str()))
            .collect()
    }

    #[test]
    fn parse_faq_basic_structure() {
        let md = "# Category One\n\n## Question A\n\nAnswer A.\n\n## Question B\n\nAnswer B.\n\n# Category Two\n\n## Question C\n\nAnswer C.\n";
        let cats = parse_faq(md);
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0].title, "Category One");
        assert_eq!(cats[0].entries.len(), 2);
        assert_eq!(cats[0].entries[0].question, "Question A");
        assert!(cats[0].entries[0].answer_html.contains("Answer A."));
        assert_eq!(cats[0].entries[1].question, "Question B");
        assert_eq!(cats[1].title, "Category Two");
        assert_eq!(cats[1].entries.len(), 1);
        assert_eq!(cats[1].entries[0].question, "Question C");
    }

    #[test]
    fn parse_faq_with_links_in_answer() {
        let md = "# Tech\n\n## How?\n\nSee [the docs](https://example.com) for details.\n";
        let cats = parse_faq(md);
        assert_eq!(cats.len(), 1);
        assert!(
            cats[0].entries[0]
                .answer_html
                .contains("<a href=\"https://example.com\">")
        );
    }

    #[test]
    fn parse_faq_with_list_in_answer() {
        let md = "# Info\n\n## What tools?\n\nThree tools:\n\n- **Tool A** — does X\n- **Tool B** — does Y\n";
        let cats = parse_faq(md);
        assert_eq!(cats.len(), 1);
        assert!(cats[0].entries[0].answer_html.contains("<li>"));
    }

    #[test]
    fn a_category_heading_with_no_questions_is_kept_and_left_empty() {
        let md = "# Empty\n\n# HasContent\n\n## Q\n\nA.\n";
        let cats = parse_faq(md);
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0].entries.len(), 0);
        assert_eq!(cats[1].entries.len(), 1);
    }

    #[test]
    fn parse_faq_multi_paragraph_answer() {
        let md = "# Cat\n\n## Q\n\nFirst paragraph.\n\nSecond paragraph.\n";
        let cats = parse_faq(md);
        assert!(cats[0].entries[0].answer_html.contains("First paragraph."));
        assert!(cats[0].entries[0].answer_html.contains("Second paragraph."));
    }

    #[test]
    fn slug_lowercases_and_collapses_non_alphanumeric_runs() {
        assert_eq!(
            slugify("How does Chronoscope work?"),
            "how-does-chronoscope-work"
        );
        assert_eq!(
            slugify("Is it open source -- really?!"),
            "is-it-open-source-really"
        );
        assert_eq!(slugify("  Spaced out  "), "spaced-out");
    }

    #[test]
    fn slug_of_a_question_without_alphanumerics_falls_back_to_a_stem() {
        assert_eq!(slugify("?!"), FALLBACK_SLUG);
    }

    #[test]
    fn questions_differing_only_in_punctuation_get_distinct_slugs() {
        let md = "# Cat\n\n## Is it open source?\n\nYes.\n\n## Is it open source!\n\nStill yes.\n";
        let cats = parse_faq(md);
        assert_eq!(
            all_slugs(&cats),
            ["is-it-open-source", "is-it-open-source-2"]
        );
    }

    #[test]
    fn slugs_are_unique_across_categories_not_just_within_one() {
        let md = "# A\n\n## Same question\n\nOne.\n\n# B\n\n## Same question\n\nTwo.\n";
        let cats = parse_faq(md);
        assert_eq!(all_slugs(&cats), ["same-question", "same-question-2"]);
    }

    #[test]
    fn disambiguation_skips_a_number_a_question_already_slugs_to() {
        let md = "# Cat\n\n## Step\n\nOne.\n\n## Step 2\n\nTwo.\n\n## Step!\n\nThree.\n";
        let cats = parse_faq(md);
        assert_eq!(all_slugs(&cats), ["step", "step-2", "step-3"]);
    }

    #[test]
    fn a_dropped_empty_answer_does_not_consume_its_slug() {
        let md = "# Cat\n\n## Repeat\n\n## Repeat\n\nOnly answer.\n";
        let cats = parse_faq(md);
        assert_eq!(all_slugs(&cats), ["repeat"]);
    }

    #[test]
    fn slug_elides_apostrophes_rather_than_splitting_the_word() {
        assert_eq!(
            slugify("Dates that aren't precise?"),
            "dates-that-arent-precise"
        );
        assert_eq!(
            slugify("Dates that aren\u{2019}t precise?"),
            "dates-that-arent-precise"
        );
    }

    #[test]
    fn a_pinned_anchor_becomes_the_slug_verbatim() {
        let md = "# Cat\n\n## How is this different from a dozen other projects? {#vs-other-projects}\n\nAnswer.\n";
        let cats = parse_faq(md);
        assert_eq!(all_slugs(&cats), ["vs-other-projects"]);
    }

    #[test]
    fn a_pinned_anchor_is_normalized_into_a_usable_id() {
        let md = "# Cat\n\n## Q {#Weird--ID!}\n\nAnswer.\n";
        let cats = parse_faq(md);
        assert_eq!(all_slugs(&cats), ["weird-id"]);
    }

    #[test]
    fn a_pinned_anchor_does_not_leak_into_the_question_text() {
        let md = "# Cat\n\n## How does it work? {#how-it-works}\n\nAnswer.\n";
        let cats = parse_faq(md);
        assert_eq!(cats[0].entries[0].question, "How does it work?");
    }

    #[test]
    fn a_derived_slug_numbers_out_of_the_way_of_a_pinned_one() {
        let md = "# Cat\n\n## Overview\n\nOne.\n\n## Something else {#overview}\n\nTwo.\n";
        let cats = parse_faq(md);
        assert_eq!(all_slugs(&cats), ["overview-2", "overview"]);
    }

    #[test]
    fn headings_pinning_the_same_anchor_settle_in_document_order() {
        let md = "# Cat\n\n## First {#dup}\n\nOne.\n\n## Second {#dup}\n\nTwo.\n";
        let cats = parse_faq(md);
        assert_eq!(all_slugs(&cats), ["dup", "dup-2"]);
    }

    /// Destinations of the site-internal links in a markdown source, in
    /// document order: every one starting `/`, fragment and all.
    fn internal_link_destinations(markdown: &str) -> Vec<String> {
        Parser::new(markdown)
            .filter_map(|event| match event {
                Event::Start(Tag::Link { dest_url, .. }) => {
                    dest_url.starts_with('/').then(|| dest_url.to_string())
                }
                _ => None,
            })
            .collect()
    }

    /// The one page whose headings carry ids, and so the one a fragment link
    /// can name something on.
    const ANCHORED_PAGE: &str = "/faq";

    /// Fragments of the `/faq#…` links in a markdown source, in document order.
    fn faq_link_fragments(markdown: &str) -> Vec<String> {
        internal_link_destinations(markdown)
            .iter()
            .filter_map(|dest| {
                dest.strip_prefix(&format!("{ANCHORED_PAGE}#"))
                    .map(str::to_string)
            })
            .collect()
    }

    /// The stem of the page the browser suite reads its FAQ deep link off.
    const ABOUT_STEM: &str = "about";

    /// The markdown of the article with this stem, if a row declares one.
    fn article_markdown(stem: &str) -> Option<&'static str> {
        ARTICLES
            .iter()
            .find(|article| article.stem == stem)
            .map(|article| article.markdown)
    }

    /// Every path the app serves, in the form the content links to it.
    ///
    /// Reading the rows is reading the routes: `web/src/app.rs` gets its article
    /// routes from [`generated_route_macro_source`], which walks this same
    /// table, so a row here is a page served there.
    fn served_paths() -> Vec<String> {
        ARTICLES
            .iter()
            .map(|article| article_path(article.stem))
            .chain(NON_ARTICLE_ROUTES.iter().map(|route| (*route).to_string()))
            .collect()
    }

    /// A content file's name is the URL its page is served at, so renaming one
    /// moves a public URL and strands every link that named it: the links are
    /// literals in prose, and the route they used to reach is derived from the
    /// stem that just changed. The reader gets "Not found." and nothing else
    /// notices.
    ///
    /// A fragment is checked too, because only the FAQ anchors its headings. A
    /// link into the middle of an article page resolves as far as the page and
    /// then lands the reader at the top of it, which reads as the link being
    /// fine.
    #[test]
    fn every_internal_link_in_the_content_names_a_route_that_exists() {
        let served = served_paths();
        let mut checked = 0;
        for (page, source) in content_pages() {
            for dest in internal_link_destinations(&expand_snippets(source)) {
                checked += 1;
                let (path, fragment) = dest
                    .split_once('#')
                    .map_or((dest.as_str(), None), |(path, fragment)| {
                        (path, Some(fragment))
                    });
                assert!(
                    served.iter().any(|route| route == path),
                    "{page} links to {dest}, which no route serves; served: {served:?}"
                );
                assert!(
                    fragment.is_none() || path == ANCHORED_PAGE,
                    "{page} links to {dest}, but only {ANCHORED_PAGE} anchors its headings: an article page renders with heading attributes off, so the fragment names nothing and the reader lands at the top of the page"
                );
            }
        }
        // Without a link to check, the loop above asserts nothing at all.
        assert!(
            checked > 0,
            "no content page links anywhere in the site, so this guard has nothing left to check"
        );
    }

    /// Deep links into the FAQ from anywhere in the content, checked against the
    /// anchors the FAQ actually assigns. Nothing else would notice either side
    /// moving: not the FAQ heading a link names, and not the link itself. The
    /// FAQ is checked against itself too, since it cross-links like any other
    /// page.
    #[test]
    fn every_faq_deep_link_in_the_content_names_a_real_anchor() {
        // Both sides expanded, so the check sees the pages as a reader does.
        let cats = parse_faq(&expand_snippets(FAQ_MARKDOWN));
        let slugs = all_slugs(&cats);
        let mut checked = 0;
        for (page, source) in content_pages() {
            for fragment in faq_link_fragments(&expand_snippets(source)) {
                checked += 1;
                assert!(
                    slugs.contains(&fragment.as_str()),
                    "{page} links to /faq#{fragment}, which no FAQ heading anchors; anchors: {slugs:?}"
                );
            }
        }
        // Without a link to check, the loop above asserts nothing at all.
        assert!(
            checked > 0,
            "no content page deep-links into the FAQ, so this guard has nothing left to check"
        );
    }

    /// The browser suite finds the FAQ item it follows by reading the About
    /// page's deep link off the page. Move that link to another page and the
    /// check above is still satisfied, while two browser tests lose the link
    /// they read their anchor from and fail for a reason a browser can only
    /// describe as "no such element".
    #[test]
    fn the_about_page_deep_links_into_the_faq() {
        let markdown = article_markdown(ABOUT_STEM);
        assert!(
            markdown.is_some(),
            "no article row declares `{ABOUT_STEM}`, the page the browser suite reads its FAQ deep link off"
        );
        let fragments = faq_link_fragments(&expand_snippets(markdown.unwrap_or_default()));
        assert!(
            !fragments.is_empty(),
            "{ABOUT_STEM}.md carries no /faq# deep link; the browser suite reads the anchor it follows off that page"
        );
    }

    /// The anchors `faq.md` pins by hand with `{#id}`, in the form the page
    /// publishes them, in document order.
    ///
    /// Writing a pin is how an author says a heading's URL is already out in
    /// the world, so the pins *are* the published set, kept in the file where
    /// the reword that would strand one happens.
    ///
    /// Read the way [`assign_slugs`] reads them, or the guards below hunt for
    /// something the page never carried: a pin goes out through [`slugify`],
    /// and only a `## Question` heading is given an anchor at all, since
    /// [`parse_entries`] drops the pin on a category heading.
    fn pinned_faq_anchors(markdown: &str) -> Vec<String> {
        Parser::new_ext(markdown, Options::ENABLE_HEADING_ATTRIBUTES)
            .filter_map(|event| match event {
                Event::Start(Tag::Heading {
                    level: HeadingLevel::H2,
                    id: Some(id),
                    ..
                }) => Some(slugify(&id)),
                _ => None,
            })
            .collect()
    }

    /// Without the heading-attribute option the parser reports no ids at all,
    /// and the guards below would derive an empty published set and check
    /// nothing. The other two cases are the ones that made this disagree with
    /// the page: a pin ships normalized, and a pinned category heading ships no
    /// anchor whatsoever.
    #[test]
    fn a_pinned_question_reports_the_anchor_the_page_publishes() {
        assert_eq!(
            pinned_faq_anchors("# Cat\n\n## Q {#pinned-here}\n\nA.\n"),
            ["pinned-here"]
        );
        assert!(pinned_faq_anchors("# Cat\n\n## Q\n\nA.\n").is_empty());
        assert_eq!(
            pinned_faq_anchors("# Cat\n\n## Q {#Weird--ID!}\n\nA.\n"),
            ["weird-id"]
        );
        assert!(pinned_faq_anchors("# Cat {#cat}\n\n## Q\n\nA.\n").is_empty());
    }

    /// Two headings pinning one id is a content bug nothing else reports: the
    /// second numbers itself out of the way, both anchors exist, and the URL
    /// that was published opens whichever heading happens to come first.
    #[test]
    fn no_two_faq_headings_pin_the_same_anchor() {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let duplicates: Vec<String> = pinned_faq_anchors(&expand_snippets(FAQ_MARKDOWN))
            .into_iter()
            .filter(|anchor| !seen.insert(anchor.clone()))
            .collect();
        assert!(
            duplicates.is_empty(),
            "faq.md pins {duplicates:?} on more than one heading, so one of them is served at a numbered anchor nobody linked to"
        );
    }

    /// Published FAQ anchors that pin nothing of their own.
    ///
    /// `how-does-chronoscope-work` is the deep link the About page hands
    /// readers, and from there it travels into messages and bookmarks, but its
    /// heading carries no pin. Reword that heading and update the About link in
    /// the same commit and every other anchor check still passes, because they
    /// are all relative.
    const PUBLISHED_FAQ_ANCHORS: &[&str] = &["how-does-chronoscope-work"];

    /// A published anchor is a promise, and the FAQ pipeline can break one
    /// without any heading being touched: a pinned entry whose answer went
    /// empty is dropped from the page, taking its anchor with it. What this
    /// checks is that every anchor the file pins, plus the unpinned ones named
    /// below, is still assigned to some entry. Deleting a pin retires its URL,
    /// which is the one way this passes and a reader still gets nothing; that
    /// edit is deliberate and lands in the diff of `faq.md`.
    #[test]
    fn the_faq_still_anchors_every_published_fragment() {
        let expanded = expand_snippets(FAQ_MARKDOWN);
        let cats = parse_faq(&expanded);
        let slugs = all_slugs(&cats);
        let pinned = pinned_faq_anchors(&expanded);
        for anchor in pinned
            .iter()
            .map(String::as_str)
            .chain(PUBLISHED_FAQ_ANCHORS.iter().copied())
        {
            assert!(
                slugs.contains(&anchor),
                "/faq#{anchor} is a published URL that no FAQ heading anchors any more; anchors: {slugs:?}"
            );
        }
    }

    /// The emitted table is Rust source no human reads before the compiler
    /// does, and every rendered link puts a `"` inside an answer. A quote that
    /// got out of its literal would end the string and turn prose into syntax.
    #[test]
    fn an_emitted_answer_keeps_its_quotes_inside_the_literal() {
        let source = generated_faq_source("# Cat\n\n## Q\n\nSee [docs](https://example.com).\n");
        assert!(source.contains(r#"title: "Cat""#), "{source}");
        assert!(
            source.contains(
                r#"FaqEntry { question: "Q", slug: "q", answer_html: RenderedHtml::from_build_output("<p>See <a href=\"https://example.com\">docs</a>.</p>") }"#
            ),
            "{source}"
        );
    }

    /// Every string literal in `source`, in the order it appears, quotes and
    /// escapes as written.
    ///
    /// The emitter writes each field through `{:?}` and nothing else, so the
    /// literals in what it produced *are* the fields it emitted. Escape-aware
    /// only where it must be: a `\"` inside an answer does not end the literal
    /// carrying it.
    fn string_literals(source: &str) -> Vec<&str> {
        let bytes = source.as_bytes();
        let mut literals = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            if bytes.get(at) != Some(&b'"') {
                at += 1;
                continue;
            }
            let start = at;
            at += 1;
            loop {
                match bytes.get(at) {
                    Some(b'\\') => at += 2,
                    Some(b'"') => {
                        at += 1;
                        break;
                    }
                    Some(_) => at += 1,
                    // An unterminated literal would not compile. Leaving the
                    // tail out is what the count assertion downstream reports.
                    None => return literals,
                }
            }
            literals.push(source.get(start..at).unwrap_or_default());
        }
        literals
    }

    /// The shipped FAQ is whatever this emitter wrote, so a table that dropped a
    /// category, reordered two questions or truncated an answer is a page that
    /// silently disagrees with `faq.md`. Nothing downstream can tell: it all
    /// compiles and renders.
    ///
    /// Checked against the real page rather than a fixture, so the guard covers
    /// the content actually shipped, and field by field in document order, so a
    /// failure names the entry that moved.
    #[test]
    fn the_emitted_table_carries_every_parsed_faq_entry_in_document_order() {
        let parsed = parse_faq(&expand_snippets(FAQ_MARKDOWN));
        // An empty parse would make every assertion below vacuously true.
        assert!(
            parsed.iter().any(|cat| !cat.entries.is_empty()),
            "faq.md parsed to no entries at all, so this guard would compare nothing"
        );

        // Each field labelled with what it is, so a mismatch reads as "the
        // answer to question X" rather than as a position in a long list.
        let expected: Vec<(String, String)> = parsed
            .iter()
            .flat_map(|cat| {
                let title = &cat.title;
                std::iter::once((format!("title of category {title:?}"), format!("{title:?}")))
                    .chain(cat.entries.iter().flat_map(move |entry| {
                        let question = &entry.question;
                        [
                            (
                                format!("question {question:?} of category {title:?}"),
                                format!("{question:?}"),
                            ),
                            (format!("slug of {question:?}"), format!("{:?}", entry.slug)),
                            (
                                format!("answer to {question:?}"),
                                format!("{:?}", entry.answer_html),
                            ),
                        ]
                    }))
            })
            .collect();

        let generated = generated_faq_source(FAQ_MARKDOWN);
        let emitted = string_literals(&generated);
        for (position, (what, want)) in expected.iter().enumerate() {
            let got = emitted
                .get(position)
                .copied()
                .unwrap_or("<nothing: the emitted table ended here>");
            assert_eq!(
                got,
                want.as_str(),
                "emitted field {position} is not the {what}"
            );
        }
        assert_eq!(
            emitted.len(),
            expected.len(),
            "the emitted table carries strings the parse of faq.md does not: {:?}",
            emitted.get(expected.len()..)
        );
    }
}
