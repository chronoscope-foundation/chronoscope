use std::collections::BTreeSet;

use leptos::prelude::*;
use leptos_router::hooks::use_location;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};

use crate::components::nav::NAV_CLEARANCE;
use crate::markdown::expand_snippets;

const FAQ_MARKDOWN: &str = include_str!("../../content/faq.md");

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
        html::push_html(&mut answer_html, answer_events);
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
                        answer_events.clear();
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

#[component]
fn FaqItem(question: String, slug: String, answer_html: String) -> impl IntoView {
    // The router sets its URL signal as the incoming view is built and only
    // moves `window.location` once that build finishes, so the signal is the
    // hash source that is already correct here.
    let hash = use_location().hash;
    let targets_this_item = {
        let slug = slug.clone();
        move |hash: &str| hash.strip_prefix('#') == Some(slug.as_str())
    };

    // Seeded untracked so a deep-linked item is open on its first paint.
    let (open, set_open) = signal(targets_this_item(&hash.get_untracked()));
    let item_ref = NodeRef::<leptos::html::Div>::new();

    // A hash-only navigation rebuilds nothing, so a second anchor followed from
    // this same page has to expand reactively. The hash is the effect's only
    // trigger, so a manual collapse of the named item sticks.
    Effect::new(move || {
        if targets_this_item(&hash.get())
            && let Some(el) = item_ref.get()
        {
            set_open.set(true);
            el.scroll_into_view();
        }
    });

    view! {
        <div node_ref=item_ref id=slug class="border-b border-sepia/20">
            <button
                class="w-full py-5 flex justify-between items-center text-left cursor-pointer"
                on:click=move |_| set_open.update(|v| *v = !*v)
                aria-expanded=move || open.get().to_string()
            >
                <span class="text-lg font-semibold pr-4">{question}</span>
                <span class="text-sepia text-xl shrink-0 transition-transform duration-200 font-sans"
                    style=move || if open.get() { "transform: rotate(45deg)" } else { "" }
                >
                    "+"
                </span>
            </button>
            <div
                class="grid transition-[grid-template-rows] duration-200"
                style=move || if open.get() { "grid-template-rows: 1fr" } else { "grid-template-rows: 0fr" }
            >
                <div class="overflow-hidden">
                    <div class="pb-5 text-sepia leading-relaxed prose-chronoscope" inner_html=answer_html></div>
                </div>
            </div>
        </div>
    }
}

#[component]
pub fn Faq() -> impl IntoView {
    let expanded = expand_snippets(FAQ_MARKDOWN);
    let categories = parse_faq(&expanded);

    view! {
        <section class=format!("px-6 pb-12 max-w-3xl mx-auto {NAV_CLEARANCE}")>
            <h1 class="text-3xl font-bold mb-10">"Frequently Asked Questions"</h1>

            {categories.into_iter().map(|cat| view! {
                <div class="mb-10">
                    <h2 class="text-lg font-semibold text-copper mb-2 font-sans">{cat.title}</h2>
                    <div class="divide-y divide-sepia/20">
                        {cat.entries.into_iter().map(|e| view! {
                            <FaqItem question=e.question slug=e.slug answer_html=e.answer_html/>
                        }).collect::<Vec<_>>()}
                    </div>
                </div>
            }).collect::<Vec<_>>()}
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::existence_legend::EXISTENCE_STATES_ANCHOR;

    const ABOUT_MARKDOWN: &str = include_str!("../../content/about.md");

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
    fn parse_faq_empty_category_skipped() {
        let md = "# Empty\n\n# HasContent\n\n## Q\n\nA.\n";
        let cats = parse_faq(md);
        // Empty category has no entries, but is still created (just empty)
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

    /// Fragments of the `/faq#…` links in a markdown source, in document order.
    fn faq_link_fragments(markdown: &str) -> Vec<String> {
        Parser::new(markdown)
            .filter_map(|event| match event {
                Event::Start(Tag::Link { dest_url, .. }) => {
                    dest_url.strip_prefix("/faq#").map(str::to_string)
                }
                _ => None,
            })
            .collect()
    }

    /// Deep links from the About page into the FAQ, checked against the anchors
    /// the FAQ actually assigns. Nothing else would notice either side moving:
    /// not the FAQ heading the link names, and not the link itself.
    #[test]
    fn every_faq_deep_link_on_the_about_page_names_a_real_anchor() {
        // Both sides expanded, so the check sees the pages as a reader does.
        let cats = parse_faq(&expand_snippets(FAQ_MARKDOWN));
        let slugs = all_slugs(&cats);
        let fragments = faq_link_fragments(&expand_snippets(ABOUT_MARKDOWN));
        // Without a link to check, the loop below asserts nothing at all.
        assert!(
            !fragments.is_empty(),
            "the About page no longer deep-links into the FAQ; this guard has nothing left to check"
        );
        for fragment in &fragments {
            assert!(
                slugs.contains(&fragment.as_str()),
                "the About page links to /faq#{fragment}, which no FAQ heading anchors; anchors: {slugs:?}"
            );
        }
    }

    /// The map legend's deep link into the FAQ, checked against the anchors the
    /// FAQ assigns.
    ///
    /// Its own test rather than an entry in the guard above: that one reads the
    /// About markdown, and the legend's link is a Rust `view!` that no markdown
    /// parse can see. Both sides go through `EXISTENCE_STATES_ANCHOR`, so a
    /// renamed heading fails here rather than shipping a link to nowhere.
    #[test]
    fn the_map_legends_faq_link_names_a_real_anchor() {
        let cats = parse_faq(&expand_snippets(FAQ_MARKDOWN));
        let slugs = all_slugs(&cats);
        assert!(
            slugs.contains(&EXISTENCE_STATES_ANCHOR),
            "the map legend links to /faq#{EXISTENCE_STATES_ANCHOR}, which no FAQ heading anchors; anchors: {slugs:?}"
        );
    }
}
