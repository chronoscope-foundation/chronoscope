use leptos::prelude::*;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};

const FAQ_MARKDOWN: &str = include_str!("../../content/faq.md");

struct FaqCategory {
    title: String,
    entries: Vec<(String, String)>,
}

/// Parse FAQ markdown using pulldown-cmark events.
///
/// Expected structure: `# Category` headings containing `## Question` headings
/// followed by answer content. Uses the pulldown-cmark parser to handle all
/// markdown edge cases correctly instead of fragile string splitting.
/// Render accumulated answer events to HTML and push onto the current category.
fn flush_answer<'a>(
    question: Option<String>,
    answer_events: impl Iterator<Item = Event<'a>>,
    categories: &mut [FaqCategory],
) {
    if let Some(question) = question {
        let mut answer_html = String::new();
        html::push_html(&mut answer_html, answer_events);
        let answer_html = answer_html.trim().to_string();
        if !answer_html.is_empty()
            && let Some(cat) = categories.last_mut()
        {
            cat.entries.push((question, answer_html));
        }
    }
}

fn parse_faq(markdown: &str) -> Vec<FaqCategory> {
    let parser = Parser::new_ext(markdown, Options::empty());
    let mut categories: Vec<FaqCategory> = Vec::new();
    let mut current_heading_level: Option<HeadingLevel> = None;
    let mut heading_text = String::new();
    let mut answer_events: Vec<Event<'_>> = Vec::new();
    let mut current_question: Option<String> = None;

    for event in parser {
        match &event {
            Event::Start(Tag::Heading { level, .. }) => {
                flush_answer(
                    current_question.take(),
                    answer_events.drain(..),
                    &mut categories,
                );
                current_heading_level = Some(*level);
                heading_text.clear();
            }
            Event::Text(text) if current_heading_level.is_some() => {
                heading_text.push_str(text);
            }
            Event::End(TagEnd::Heading(_)) => {
                match current_heading_level.take() {
                    Some(HeadingLevel::H1) => {
                        categories.push(FaqCategory {
                            title: heading_text.clone(),
                            entries: Vec::new(),
                        });
                    }
                    Some(HeadingLevel::H2) => {
                        current_question = Some(heading_text.clone());
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

#[component]
fn FaqItem(question: String, answer_html: String) -> impl IntoView {
    let (open, set_open) = signal(false);

    view! {
        <div class="border-b border-sepia/20">
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
    let categories = parse_faq(FAQ_MARKDOWN);

    view! {
        <section class="px-6 py-12 max-w-3xl mx-auto">
            <h1 class="text-3xl font-bold mb-10">"Frequently Asked Questions"</h1>

            {categories.into_iter().map(|cat| view! {
                <div class="mb-10">
                    <h2 class="text-lg font-semibold text-copper mb-2 font-sans">{cat.title}</h2>
                    <div class="divide-y divide-sepia/20">
                        {cat.entries.into_iter().map(|(q, a)| view! { <FaqItem question=q answer_html=a/> }).collect::<Vec<_>>()}
                    </div>
                </div>
            }).collect::<Vec<_>>()}
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_faq_basic_structure() {
        let md = "# Category One\n\n## Question A\n\nAnswer A.\n\n## Question B\n\nAnswer B.\n\n# Category Two\n\n## Question C\n\nAnswer C.\n";
        let cats = parse_faq(md);
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0].title, "Category One");
        assert_eq!(cats[0].entries.len(), 2);
        assert_eq!(cats[0].entries[0].0, "Question A");
        assert!(cats[0].entries[0].1.contains("Answer A."));
        assert_eq!(cats[0].entries[1].0, "Question B");
        assert_eq!(cats[1].title, "Category Two");
        assert_eq!(cats[1].entries.len(), 1);
        assert_eq!(cats[1].entries[0].0, "Question C");
    }

    #[test]
    fn parse_faq_with_links_in_answer() {
        let md = "# Tech\n\n## How?\n\nSee [the docs](https://example.com) for details.\n";
        let cats = parse_faq(md);
        assert_eq!(cats.len(), 1);
        assert!(
            cats[0].entries[0]
                .1
                .contains("<a href=\"https://example.com\">")
        );
    }

    #[test]
    fn parse_faq_with_list_in_answer() {
        let md = "# Info\n\n## What tools?\n\nThree tools:\n\n- **Tool A** — does X\n- **Tool B** — does Y\n";
        let cats = parse_faq(md);
        assert_eq!(cats.len(), 1);
        assert!(cats[0].entries[0].1.contains("<li>"));
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
        assert!(cats[0].entries[0].1.contains("First paragraph."));
        assert!(cats[0].entries[0].1.contains("Second paragraph."));
    }
}
