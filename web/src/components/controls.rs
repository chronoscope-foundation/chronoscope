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
/// The `position` utility stays at those call sites rather than being folded in
/// here: a host that wanted a different one would then carry two, and which
/// wins is decided by stylesheet order rather than by anything visible where
/// the class is written.
pub const HIT_AREA: &str = "before:absolute before:content-[''] before:-inset-1.5";

/// How the things floating over the map stack, in three rungs.
///
/// The map's overlays grow and shrink with what they hold, so which of them
/// overlaps which changes with the viewport: on a phone the time card's legend
/// makes it tall enough to reach the About card, and the map's alert strip sits
/// under whatever height that card has taken. Two overlays sharing a rung are
/// ordered by where they happen to appear in the markup, which is invisible from
/// either one. Naming the rungs is what makes the order a decision.
///
/// Highest: an alert about the map itself, which stays readable whatever else is
/// open. [`OVERLAY_ALERT`]
///
/// Middle: the controls the reader is working. [`OVERLAY_CONTROL`]
///
/// Lowest: the panels that only explain things, which yield to both.
/// [`OVERLAY_INFO`]
pub const OVERLAY_ALERT: &str = "z-30";

/// The controls rung. See [`OVERLAY_ALERT`] for the ladder.
pub const OVERLAY_CONTROL: &str = "z-20";

/// The informational rung. See [`OVERLAY_ALERT`] for the ladder.
pub const OVERLAY_INFO: &str = "z-10";
