//! Cursor pagination as a stream.
//!
//! A [`FactStore`](crate::facts::store::FactStore) backlink or class read comes
//! back one page at a time behind a cursor. [`paginate`] turns any such read
//! into a flat [`TryStream`] of rows, leaving the terminal choice — collect,
//! fold, count — to each caller's own stream combinators.

use std::collections::BTreeSet;
use std::future::{Future, poll_fn};
use std::num::NonZeroUsize;
use std::pin::Pin;

use futures_util::TryStreamExt;
use futures_util::stream::{self, TryStream};

use crate::facts::schema::{Class, ClassRow};

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

/// Fold a class walk's `(representative, fact_id)` rows into whole
/// [`Class`]es: accumulate a `BTreeSet<FactId>` while the representative holds,
/// emit the held class when the representative changes, and flush the last held
/// class at stream end.
///
/// Contract: `stream` delivers rows globally ordered by representative, so every
/// row of one class is adjacent even across page boundaries. That contiguity
/// folds a class into a single emission and keeps a representative from being
/// emitted twice; a stream ordered any other way splits one class across
/// several [`Class`] emissions.
pub(crate) fn group_classes<Rep, E, St>(stream: St) -> impl TryStream<Ok = Class<Rep>, Error = E>
where
    St: TryStream<Ok = ClassRow<Rep>, Error = E>,
    Rep: Clone + PartialEq,
{
    // The held class being accumulated: its representative and the fact ids seen
    // under it so far. `Pin<Box<St>>` is `Unpin`, so the pinned stream drives
    // from the unfold state. The outer `Option` is the terminal marker: once the
    // upstream drains and the last held class is flushed, the state is `None`, so
    // the exhausted stream is dropped and the next call short-circuits without a
    // further poll.
    type Held<Rep> = (Rep, BTreeSet<crate::facts::ids::FactId>);
    type State<St, Rep> = Option<(Pin<Box<St>>, Option<Held<Rep>>)>;
    let init: State<St, Rep> = Some((Box::pin(stream), None));
    stream::try_unfold(init, |state| async move {
        let Some((mut stream, mut held)) = state else {
            return Ok(None);
        };
        loop {
            // Drive the upstream via `try_poll_next`, whose `Ok`/`Error` types
            // are the `TryStream`'s own — `try_next` would need the pinned
            // stream's `Stream::Item` to be provably a `Result`, which is opaque
            // behind the `TryStream` bound.
            let row = poll_fn(|cx| stream.as_mut().try_poll_next(cx)).await;
            match row {
                Some(row) => {
                    let row = row?;
                    match &mut held {
                        // Same representative: fold this row into the held class.
                        Some((rep, fact_ids)) if *rep == row.representative => {
                            fact_ids.insert(row.fact_id);
                        }
                        // A new representative (or the first row): seed the next
                        // held class, flushing the finished one if there is one.
                        _ => {
                            let seed = (row.representative.clone(), BTreeSet::from([row.fact_id]));
                            if let Some((representative, fact_ids)) = held.replace(seed) {
                                let done = Class {
                                    representative,
                                    fact_ids,
                                };
                                return Ok(Some((done, Some((stream, held)))));
                            }
                        }
                    }
                }
                None => {
                    // Upstream drained: emit the last held class under the
                    // terminal `None` state, or end the stream when none is held.
                    return Ok(held.map(|(representative, fact_ids)| {
                        (
                            Class {
                                representative,
                                fact_ids,
                            },
                            None,
                        )
                    }));
                }
            }
        }
    })
}
