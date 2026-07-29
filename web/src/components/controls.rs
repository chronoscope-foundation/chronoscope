//! Interaction affordances the app's controls share.
//!
//! Both of these are the kind of thing that gets applied to one button and
//! forgotten on the next, so they are named here rather than spelled out at
//! each site.

/// The surface a floating control wears over the map: the nav trigger, and the
/// overlay cards and their collapsed chips. Named once so they cannot drift.
///
/// Outlined with a ring rather than a border, because a ring paints outside the
/// box instead of inside it. A border gets counted two different ways: an
/// element with an explicit height keeps its border *within* that height under
/// `box-sizing: border-box`, while an element sized by its content has the
/// border added on top. Same 1 px, and two controls that should match end up
/// 2 px apart. A ring costs no layout at all.
pub const SURFACE: &str = "bg-parchment/95 backdrop-blur-sm shadow-lg ring-1 ring-sepia/20";

/// Keyboard focus indication. The browser's default outline is too faint
/// against parchment to find.
pub const FOCUS_RING: &str = "focus-visible:outline-none focus-visible:ring-2 \
                              focus-visible:ring-copper/60 focus-visible:ring-offset-2 \
                              focus-visible:ring-offset-parchment";

/// Carries the pointer target out past the visual box.
///
/// WCAG 2.5.8 (level AA) asks for 24x24 CSS px, which these controls clear on
/// their own; the 44 px figure is 2.5.5, level AAA. Rather than draw every
/// control at 44 px, this pseudo-element extends the *clickable* area to roughly
/// that while the drawn box stays small.
///
/// The element's own border box is unchanged — an absolutely positioned
/// `::before` does not grow it — so layout and the overlay toggles' position
/// invariant still measure what the reader sees.
///
/// The host must establish a containing block, or the `::before` escapes to the
/// nearest positioned ancestor and the extra area lands somewhere else — as a
/// viewport-wide click target, if that ancestor happens to be a `fixed` strip.
/// A reusable component that applies this must therefore position *itself*
/// rather than trust its caller, since it cannot see where it will be placed.
/// This
/// deliberately carries no `position` utility of its own: the overlay toggles
/// are already `absolute`, and adding `relative` here would put two `position`
/// classes on one element, where the winner is decided by stylesheet order
/// rather than by anything visible at the call site.
pub const HIT_AREA: &str = "before:absolute before:content-[''] before:-inset-1.5";
