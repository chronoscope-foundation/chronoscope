//! The static pages the build script renders, and the type that says where
//! they came from.
//!
//! [`RenderedHtml`]'s field and constructor are private to this module, so the
//! code that can mint one is the code compiled inside it: this file, and the
//! generated tables it includes. Rust extends module-private access to
//! descendants, so [`articles`] can mint too, which is exactly what lets the
//! generated constants sit in a module of their own. No module outside
//! `content` can, and that is the whole of what the privacy buys.
//!
//! One `pub const` per row of `ARTICLES` in `web/build/render.rs` arrives via
//! `articles.rs`, named after that row's stem: `about` is published as
//! [`articles::ABOUT`]. Generating them is what makes a page, its heading and
//! its URL a single edit, and what keeps a constant from ending up over a
//! different page's HTML.

/// HTML the build script's markdown renderer produced.
///
/// Provenance is the whole of what the name claims. A component taking one
/// cannot be handed arbitrary markup by a future caller, because obtaining a
/// value means going through this module. The renderer drops raw markup, since
/// these pages are markdown, but it reads no link or image destination: this is
/// static content the repo authors, not input to be defended against.
#[derive(Clone, Copy)]
pub struct RenderedHtml(&'static str);

impl RenderedHtml {
    /// Private on purpose. Minting one of these is a claim about where the HTML
    /// came from, and this module is the only place that can back the claim up.
    const fn from_build_output(html: &'static str) -> Self {
        Self(html)
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

/// A static content page: the URL it is served at, the heading it renders
/// under, and the HTML the build script rendered its markdown into.
///
/// The path travels with the page so the router names it from the same row the
/// markdown and the heading came from, which is what keeps `/about` and
/// `about.md` from drifting apart. The fields are read through accessors so
/// that pairing survives: a struct literal recombining one page's path with
/// another's prose is a page served at a URL its markdown never named, and
/// outside this module there is no way to write one.
#[derive(Clone, Copy)]
pub struct Article {
    path: &'static str,
    title: &'static str,
    html: RenderedHtml,
}

impl Article {
    /// The path the page is served at: a slash and its markdown file's stem.
    ///
    /// The router builds its segment from this, so a page is served where its
    /// own row says. The nav drawer spells its `href`s out instead, and
    /// `test_every_nav_drawer_link_reaches_a_real_page` is what catches one
    /// that names a page nothing serves.
    pub const fn path(self) -> &'static str {
        self.path
    }

    /// The heading the page renders under.
    pub const fn title(self) -> &'static str {
        self.title
    }

    /// The body the build script rendered from the page's markdown.
    pub const fn html(self) -> RenderedHtml {
        self.html
    }
}

/// The article pages, one `pub const` per row of `ARTICLES`.
///
/// A module of their own, so the only name an article stem can collide with is
/// another article stem. Included here rather than beside the rows it came from
/// because minting the [`RenderedHtml`] each constant carries takes the privacy
/// this module has, and a child module inherits it.
pub mod articles {
    use super::{Article, RenderedHtml};

    include!(concat!(env!("OUT_DIR"), "/articles.rs"));
}

/// A `# Category` heading and the questions under it.
///
/// This and [`FaqEntry`] are the shape `build.rs` emits into the table included
/// below, and the borrowed counterparts of the owned types the build-time
/// parser works in. What the compiler holds together is the emitter and these
/// declarations: the generated literals name every field, so a field renamed or
/// dropped here fails to compile. The owned types are checked against nothing
/// here, so a field added to one and never emitted goes unnoticed.
pub struct FaqCategory {
    title: &'static str,
    entries: &'static [FaqEntry],
}

impl FaqCategory {
    /// The `# Category` heading these questions sit under.
    pub const fn title(&self) -> &'static str {
        self.title
    }

    /// The questions under the heading, in document order.
    pub const fn entries(&self) -> &'static [FaqEntry] {
        self.entries
    }
}

/// One question, its anchor, and its answer already rendered to HTML at build
/// time.
///
/// Read through accessors for the reason [`Article`] is: the three describe one
/// question, and a literal pairing one question's text with another's answer
/// compiles just as well as the right one. Only the generated table below
/// builds these, each from a single parsed entry.
pub struct FaqEntry {
    question: &'static str,
    /// Anchor for this entry, unique across the whole page.
    slug: &'static str,
    answer_html: RenderedHtml,
}

impl FaqEntry {
    /// The `## Question` heading.
    pub const fn question(&self) -> &'static str {
        self.question
    }

    /// The anchor this entry is deep-linked by, unique across the whole page.
    pub const fn slug(&self) -> &'static str {
        self.slug
    }

    /// The answer the build script rendered from the markdown under the
    /// heading.
    pub const fn answer_html(&self) -> RenderedHtml {
        self.answer_html
    }
}

include!(concat!(env!("OUT_DIR"), "/faq.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    /// The suite in `web/build/render.rs` checks the emitted table against the
    /// parse it came from; this reads the compiled form, where an emptied or
    /// unparseable `faq.md` would arrive as a table that compiles fine and
    /// renders a page with nothing on it.
    #[test]
    fn the_compiled_faq_table_carries_populated_entries() {
        assert!(!FAQ.is_empty(), "the compiled FAQ table has no categories");
        let entries: Vec<&FaqEntry> = FAQ.iter().flat_map(|cat| cat.entries().iter()).collect();
        assert!(!entries.is_empty(), "the compiled FAQ table has no entries");
        for cat in FAQ {
            assert!(!cat.title().is_empty(), "a compiled category has no title");
        }
        for entry in entries {
            let slug = entry.slug();
            assert!(!entry.question().is_empty(), "#{slug} has no question");
            assert!(!slug.is_empty(), "an entry has no anchor");
            assert!(
                !entry.answer_html().as_str().is_empty(),
                "#{slug} has no answer"
            );
        }
    }
}
