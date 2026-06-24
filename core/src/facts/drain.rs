//! The shared cursor-drain primitive behind every backlink read.
//!
//! Both the read-side projection and the submit-side cluster rules / matcher
//! page a [`FactStore`](crate::facts::store::FactStore) backlink source to
//! exhaustion. The loop is identical; callers differ only in which read they
//! call and what they keep from each row. [`drain_pages`] is that one loop,
//! and [`drain_facts`] / [`drain_id_facts`] are its two list-building views.

use std::future::Future;
use std::num::NonZeroUsize;

use crate::facts::ids::FactId;
use crate::facts::schema::{FactPage, PageItem};

/// Page size for backlink drains. The in-memory backend serves any size in one
/// page; a SQL backend reads indexed pages of this width. Pagination is an
/// internal detail — one page covers most subjects, and the loop handles the rest.
pub(crate) const DRAIN_PAGE: NonZeroUsize = match NonZeroUsize::new(256) {
    Some(n) => n,
    None => NonZeroUsize::MIN,
};

/// Drain a paged read to exhaustion, following [`FactPage::next_cursor`] and
/// handing every row to `on_item`.
///
/// Resumes at `next_cursor` verbatim — the cursor is an inclusive lower bound
/// and `next_cursor` is the first un-yielded id, so a `+1` would drop a fact at
/// every page boundary. Termination is on `next_cursor == None` alone, never on
/// page length: a backend filtering after the page cut may return a short page
/// that still carries a cursor, and stopping on a short page would truncate.
pub(crate) async fn drain_pages<Item, Subj, E, F, Fut>(
    mut fetch: F,
    mut on_item: impl FnMut(PageItem<Item, Subj>),
) -> Result<(), E>
where
    F: FnMut(FactId) -> Fut,
    Fut: Future<Output = Result<FactPage<Item, Subj>, E>>,
{
    let mut cursor = FactId::new(0);
    loop {
        let page = fetch(cursor).await?;
        page.items.into_iter().for_each(&mut on_item);
        match page.next_cursor {
            Some(c) => cursor = c,
            None => break,
        }
    }
    Ok(())
}

/// Every fact a `fetch` closure reports, drained to exhaustion. The id-dropped
/// view of [`drain_pages`] — submit's cluster rules key off the fact content
/// alone, never the introducing fact id.
pub(crate) async fn drain_facts<Item, Subj, E, F, Fut>(fetch: F) -> Result<Vec<Item>, E>
where
    F: FnMut(FactId) -> Fut,
    Fut: Future<Output = Result<FactPage<Item, Subj>, E>>,
{
    let mut out = Vec::new();
    drain_pages(fetch, |item| out.push(item.fact)).await?;
    Ok(out)
}

/// Every `(FactId, fact)` pair a `fetch` closure reports, drained to
/// exhaustion. The id-keeping view of [`drain_pages`] — the projection retains
/// each fact's introducing id to address citations and dedup across members.
pub(crate) async fn drain_id_facts<Item, Subj, E, F, Fut>(
    fetch: F,
) -> Result<Vec<(FactId, Item)>, E>
where
    F: FnMut(FactId) -> Fut,
    Fut: Future<Output = Result<FactPage<Item, Subj>, E>>,
{
    let mut out = Vec::new();
    drain_pages(fetch, |item| out.push((item.fact_id, item.fact))).await?;
    Ok(out)
}
