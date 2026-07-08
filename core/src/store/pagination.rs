//! Cursor pagination as a stream.
//!
//! A [`FactStore`](crate::store::FactStore) backlink or class read comes
//! back one page at a time behind a cursor. [`paginate`] turns any such read
//! into a flat [`TryStream`] of rows, leaving the terminal choice — collect,
//! fold, count — to each caller's own stream combinators.

use std::future::Future;
use std::num::NonZeroUsize;

use futures_util::TryStreamExt;
use futures_util::stream::{self, TryStream};

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
pub(crate) fn paginate<Row, Cur, E, F, Fut>(mut fetch: F) -> impl TryStream<Ok = Row, Error = E>
where
    F: FnMut(Option<Cur>) -> Fut,
    Fut: Future<Output = Result<(Vec<Row>, Option<Cur>), E>>,
{
    // State `Some(cursor)` fetches the next page; `None` stops. `Some(None)`
    // opens the walk with no cursor. A page's `Some(c)` cursor becomes the
    // `Some(Some(c))` state, its `None` cursor the stopping `None`.
    let init: Option<Option<Cur>> = Some(None);
    stream::try_unfold(init, move |state| {
        let pending = state.map(&mut fetch);
        async move {
            match pending {
                None => Ok(None),
                Some(fut) => {
                    let (rows, next) = fut.await?;
                    let rows = stream::iter(rows.into_iter().map(Ok::<_, E>));
                    Ok(Some((rows, next.map(Some))))
                }
            }
        }
    })
    .try_flatten()
}
