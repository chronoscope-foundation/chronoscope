//! The project's transition language.
//!
//! Three motions, and a rule for choosing between them: **how a thing moves says
//! where it came from.** A sheet slides because it belongs to an edge; a scrim
//! fades because it has presence but no position; anything that simply arrives
//! in place fades, because it has no origin to express.
//!
//! Pick from this set. A one-off duration or easing spelled at a call site is
//! how a UI ends up with five nearly-identical fades that feel subtly unalike.
//!
//! ## Transitions vs entry animations
//!
//! The first two are CSS *transitions*: the element stays mounted and animates
//! between states, so it moves both on the way in and on the way out. The last
//! is an *entry animation* on elements that mount and unmount, which can only
//! animate in — CSS cannot animate an element that is being removed. That is a
//! real asymmetry, not an oversight: keeping a large panel mounted purely to
//! animate its exit costs an `inert`/`aria-hidden` dance to keep it out of the
//! accessibility tree, which is not worth 150 ms of polish.
//!
//! ## What deliberately does not move
//!
//! A map overlay card's *surface* is not animated at all, and neither available
//! property can be made to work on it:
//!
//! - **Opacity** blinks the chip out. The chip hands its own background to the
//!   card the instant it expands, so a surface fading up from transparent leaves
//!   the chip's area unpainted for a frame.
//! - **Transform** distorts the shape. These cards are stadiums (`rounded-full`
//!   at the bar's height), so a `scale` shrinks the height and the cap radius
//!   with it — the bar flattens mid-animation and its caps stop matching the
//!   collapsed chip's.
//!
//! So the surface arrives at its final geometry and only its contents fade. That
//! also keeps the promise the toggle makes: nothing already on screen moves when
//! a card opens.

/// Edge-anchored sheets that travel in from off-screen: the nav drawer, the
/// entity detail panel. The slide is the point — it shows which edge the sheet
/// belongs to, so dismissing it has an obvious direction.
///
/// Stays mounted, so this animates both directions.
pub const SLIDE: &str = "transition-transform duration-200 ease-out";

/// Scrims: the dimming behind a sheet. A scrim has no position of its own, only
/// presence, so it fades. Matched to [`SLIDE`]'s duration so the pair reads as
/// one gesture rather than two overlapping ones.
pub const SCRIM: &str = "transition-opacity duration-200 ease-out";

/// Something arriving in place, with no edge to come from: a map overlay card's
/// contents once its surface is there, and the image lightbox.
///
/// Shorter than [`SLIDE`] because nothing travels — there is no distance for the
/// eye to follow, so the same duration would only feel slow.
pub const REVEAL: &str = "animate-[fadeIn_150ms_ease-out]";
