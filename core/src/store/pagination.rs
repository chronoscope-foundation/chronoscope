//! Cursor pagination as a stream, and the backend-shared page cutters.
//!
//! A [`FactStore`](crate::store::FactStore) backlink or class read comes
//! back one page at a time behind a cursor. `paginate` turns any such read
//! into a flat [`TryStream`] of rows, leaving the terminal choice — collect,
//! fold, count — to each caller's own stream combinators.
//!
//! The class-walk cursor semantics are a cross-backend contract (the
//! conformance suite pins them), so the page cut over a materialized
//! `(representative, fact_id)` set lives here once: [`class_page`] for the
//! row-budgeted class walks, [`grouped_class_page`] for the depiction walk's
//! whole-representative budget, and [`class_cursor_past`] for the
//! skip-a-representative sentinel they share.

use std::collections::BTreeSet;
use std::future::Future;
use std::num::NonZeroUsize;
use std::ops::Bound;

use futures_util::TryStreamExt;
use futures_util::stream::{self, TryStream};

use crate::grammar::ids::FactId;
use crate::store::schema::{ClassPage, ClassRow};

/// Page size for a paginated read. The in-memory backend serves any size in one
/// page; a SQL backend reads indexed pages of this width. Pagination is an
/// internal detail — one page covers most subjects, and the stream handles the rest.
pub(crate) const PAGE_SIZE: NonZeroUsize = match NonZeroUsize::new(256) {
    Some(n) => n,
    None => NonZeroUsize::MIN,
};

/// Stream a cursor-paginated read to exhaustion, flattening each page to its
/// rows.
///
/// `fetch` returns each page as its rows and the cursor to resume past, or
/// `None` when the walk is exhausted. It opens the walk with `None`, then
/// resumes each later page at the prior page's cursor, stopping when a page
/// reports `None`. Termination is on the cursor alone: a backend filtering
/// after the page cut may return a short or empty page that still carries a
/// cursor, and the walk continues past it.
///
/// Views are exclusive (`&mut`) read handles, so `fetch` takes the walk state
/// — typically the `&mut` view — by value and returns it beside each page.
/// The state rides in the stream's own unfold state rather than being lent
/// out of a capture; dropping the stream releases it.
pub(crate) fn paginate<St, Row, Cur, E, F, Fut>(
    state: St,
    mut fetch: F,
) -> impl TryStream<Ok = Row, Error = E>
where
    F: FnMut(St, Option<Cur>) -> Fut,
    Fut: Future<Output = Result<(Vec<Row>, Option<Cur>, St), E>>,
{
    // Unfold state `Some((state, cursor))` fetches the next page; `None`
    // stops. `Some((state, None))` opens the walk with no cursor. A page's
    // `Some(c)` cursor becomes the `Some((state, Some(c)))` state, its `None`
    // cursor the stopping `None`.
    let init: Option<(St, Option<Cur>)> = Some((state, None));
    stream::try_unfold(init, move |step| {
        let pending = step.map(|(state, cursor)| fetch(state, cursor));
        async move {
            match pending {
                None => Ok(None),
                Some(fut) => {
                    let (rows, next, state) = fut.await?;
                    let rows = stream::iter(rows.into_iter().map(Ok::<_, E>));
                    Ok(Some((rows, next.map(|cursor| (state, Some(cursor))))))
                }
            }
        }
    })
    .try_flatten()
}

/// The class cursor resuming strictly past every row of `rep`: an opaque
/// `(rep, u64::MAX)` that sorts after every real `(rep, fact_id)`, so a
/// resume strictly past it lands on the first row of the next
/// representative.
pub fn class_cursor_past<S>(rep: S) -> (S, FactId) {
    (rep, FactId::new(u64::MAX))
}

/// The `next_class` cursor past `last`: [`class_cursor_past`], live only
/// while a greater representative remains in the set.
fn next_class_after<S: Copy + Ord>(rows: &BTreeSet<(S, FactId)>, last: S) -> Option<(S, FactId)> {
    rows.iter()
        .next_back()
        .filter(|(max_rep, _)| *max_rep > last)
        .map(|_| class_cursor_past(last))
}

/// One page of a materialized, ordered `(representative, fact_id)` row set —
/// the tail every class walk shares, so cursor semantics cannot drift
/// between backends.
///
/// The page holds up to `limit` rows strictly past `after` (`None` opens the
/// walk). `next` is the last emitted row while rows remain past the page,
/// else `None`. `next_class` resumes past the last row's representative and
/// is live only while a greater representative remains in the set.
pub fn class_page<S: Copy + Ord>(
    rows: &BTreeSet<(S, FactId)>,
    after: Option<(S, FactId)>,
    limit: NonZeroUsize,
) -> ClassPage<S, (S, FactId)> {
    let lower = after.map_or(Bound::Unbounded, Bound::Excluded);
    let mut remaining = rows.range((lower, Bound::Unbounded)).copied();
    let page: Vec<ClassRow<S>> = remaining
        .by_ref()
        .take(limit.get())
        .map(|(representative, fact_id)| ClassRow {
            representative,
            fact_id,
        })
        .collect();
    let next = if remaining.next().is_some() {
        page.last().map(|row| (row.representative, row.fact_id))
    } else {
        None
    };
    let next_class = page
        .last()
        .and_then(|last| next_class_after(rows, last.representative));
    ClassPage {
        rows: page,
        next,
        next_class,
    }
}

/// A whole-representative page cut: the page's `(representative, fact_id)`
/// pairs and the `next_class` cursor. [`grouped_class_page`]'s output.
pub type GroupedClassPage<S> = (Vec<(S, FactId)>, Option<(S, FactId)>);

/// One page of the same row set budgeted by whole representatives — the
/// depiction walk's cut: `limit` counts distinct representatives and every
/// row of an included representative enters the page, so none straddles the
/// boundary. Returns the page's `(representative, fact_id)` pairs — the
/// caller attaches its fact payloads — and the `next_class` cursor, live
/// only while a greater representative remains in the set.
pub fn grouped_class_page<S: Copy + Ord>(
    rows: &BTreeSet<(S, FactId)>,
    after: Option<(S, FactId)>,
    limit: NonZeroUsize,
) -> GroupedClassPage<S> {
    let lower = after.map_or(Bound::Unbounded, Bound::Excluded);
    let mut page: Vec<(S, FactId)> = Vec::new();
    let mut last_rep: Option<S> = None;
    let mut distinct = 0usize;
    for (representative, fact_id) in rows.range((lower, Bound::Unbounded)).copied() {
        if last_rep != Some(representative) {
            // Starting a further representative would overrun the page's
            // budget.
            if distinct == limit.get() {
                break;
            }
            distinct += 1;
            last_rep = Some(representative);
        }
        page.push((representative, fact_id));
    }
    let next_class = last_rep.and_then(|last| next_class_after(rows, last));
    (page, next_class)
}
