use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use leptos_router::components::A;
use send_wrapper::SendWrapper;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

use crate::components::controls::{FOCUS_RING, SURFACE};
use crate::components::focus_trap::contain_tab;
use crate::components::motion::{SCRIM, SLIDE};

/// Top padding a scrolling page needs to clear the floating nav trigger.
///
/// Exported because the trigger is `fixed`: it reserves no space, so every route
/// that renders content at the top of the viewport has to leave room for it by
/// hand. Naming it here — where the trigger's own geometry lives — is what keeps
/// the next route from forgetting. Two of the three existing sites had already
/// been missed once, which is how a mistyped URL ended up rendering "Not found."
/// underneath an opaque pill.
pub const NAV_CLEARANCE: &str = "pt-20";

const NAV_LINK_CLASS: &str = "block px-3 py-2 rounded-md text-sm font-sans font-medium text-sepia hover:text-ink hover:bg-ink/5 transition-colors";

/// The document keydown handler, held for as long as it is registered: JS keeps
/// only a borrowed reference to it, so dropping it here unhooks the listener.
type KeyListener = Rc<RefCell<Option<Closure<dyn Fn(web_sys::KeyboardEvent)>>>>;

/// The site's navigation: a floating trigger and the drawer it opens.
///
/// One drawer at every width. The permanent sidebar this replaces spent 240 px
/// of every screen on three links, over a map that wants the room, and the
/// second copy it rendered for narrow screens was a whole parallel nav to keep
/// in step.
#[component]
pub fn Nav() -> impl IntoView {
    let open = RwSignal::new(false);
    let drawer_ref = NodeRef::<leptos::html::Aside>::new();
    let trigger_ref = NodeRef::<leptos::html::Button>::new();
    // The masthead holds both the toggle and the home link, so the focus cycle
    // spans it rather than the toggle alone.
    let masthead_ref = NodeRef::<leptos::html::Div>::new();
    let close = move |_| open.set(false);

    // Whether the wordmark's destination is the page already showing.
    let path = leptos_router::hooks::use_location().pathname;
    let on_home = Signal::derive(move || path.get() == "/");

    // Focus follows the drawer: into it on open, back to the trigger on close,
    // so a keyboard user is never left on a panel that slid off-screen. The
    // effect's return value is the previous `open`, which is how the close arm
    // knows not to steal focus on first render.
    Effect::new(move |was_open: Option<bool>| {
        let is_open = open.get();
        if is_open && let Some(el) = drawer_ref.get() {
            let _ = el.focus();
        } else if was_open == Some(true)
            && let Some(el) = trigger_ref.get()
        {
            let _ = el.focus();
        }
        is_open
    });

    let on_keydown = move |ev: leptos::ev::KeyboardEvent| match ev.key().as_str() {
        "Escape" => open.set(false),
        // The masthead is part of the cycle, not outside it: it holds this
        // dialog's only close control, so a trap spanning the drawer alone would
        // confine a screen-reader user with no way out but unannounced Escape.
        "Tab" => {
            if let (Some(masthead), Some(drawer)) = (masthead_ref.get(), drawer_ref.get()) {
                contain_tab(&ev, &[masthead.as_ref(), drawer.as_ref()]);
            }
        }
        _ => {}
    };

    // The keys are watched on the document for as long as the drawer is open,
    // because the cycle spans the masthead and the masthead is the panel's
    // sibling: a listener on the panel hears nothing once Tab wraps onto the
    // trigger, which is where Shift+Tab walked out of the dialog and Escape went
    // quiet. Registered only while open, so the page behind the scrim keeps its
    // own keys the rest of the time.
    let key_listener: KeyListener = Rc::new(RefCell::new(None));
    let listener_for_effect = Rc::clone(&key_listener);
    Effect::new(move |_| {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        if let Some(previous) = listener_for_effect.borrow_mut().take() {
            let _ = document
                .remove_event_listener_with_callback("keydown", previous.as_ref().unchecked_ref());
        }
        if open.get() {
            let handler = Closure::<dyn Fn(web_sys::KeyboardEvent)>::new(on_keydown);
            let _ = document
                .add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref());
            *listener_for_effect.borrow_mut() = Some(handler);
        }
    });

    let listener_for_cleanup = SendWrapper::new(key_listener);
    on_cleanup(move || {
        if let Some(handler) = listener_for_cleanup.borrow_mut().take()
            && let Some(document) = web_sys::window().and_then(|w| w.document())
        {
            let _ = document
                .remove_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref());
        }
    });

    view! {
        // Two controls in one surface, with a rule between them saying so. The
        // burger opens the drawer; the wordmark goes home, which is the
        // convention every site has and which a `<span>` inside a button was
        // swallowing. One pill silently doing both would mean the same pixels
        // acting differently depending on state the reader cannot see.
        //
        // Neither zone moves between states, which is the point: the surface
        // withdraws over the open drawer, where it would otherwise draw a second
        // background across the drawer's own corner, but the boxes do not shift.
        <div
            node_ref=masthead_ref
            class=move || {
                let surface = if open.get() { "" } else { SURFACE };
                format!("fixed top-3 left-3 z-50 flex items-stretch h-9 \
                         rounded-full text-ink {surface}")
            }
        >
            <button
                node_ref=trigger_ref
                id="site-nav-toggle"
                type="button"
                class=format!("flex items-center px-3.5 rounded-l-full cursor-pointer \
                               hover:bg-ink/5 {FOCUS_RING}")
                aria-label="Toggle menu"
                aria-expanded=move || open.get().to_string()
                aria-controls="site-nav-drawer"
                on:click=move |_| open.update(|shown| *shown = !*shown)
            >
                // An SVG rather than the ☰ glyph: a text icon's box comes from
                // whatever the font says about that codepoint, so it cannot be
                // centred against the wordmark reliably. The 16 px box inside
                // this 44x36 button is the whole hit target, so it needs no
                // pseudo-element and cannot leak one to the wrong ancestor.
                //
                // The bars stay drawn while the drawer is open. Hiding them left
                // this zone blank, so hovering it lit an empty lozenge, and the
                // rule beside it appeared to divide the name from nothing. A
                // close mark instead would read as negative on a menu that is
                // merely standing open.
                <svg
                    aria-hidden="true"
                    class="w-4 h-4 shrink-0"
                    viewBox="0 0 16 16"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                >
                    <path d="M2 4.5h12M2 8h12M2 11.5h12"/>
                </svg>
            </button>

            // The rule is what makes the split legible. Decorative, so it is
            // hidden from the accessibility tree, where the two controls already
            // announce themselves separately.
            <span aria-hidden="true" class="self-center w-px h-4 bg-sepia/25"></span>

            // On the map itself this leads where the reader already is, so it
            // offers no hover and no pointer: a highlight that lights up and
            // then does nothing when clicked is a promise the link cannot keep.
            // `aria-current` says the same thing to a screen reader.
            <A
                href="/"
                attr:class=move || {
                    let affordance = if on_home.get() {
                        "cursor-default"
                    } else {
                        "cursor-pointer hover:bg-ink/5"
                    };
                    format!("flex items-center px-4 rounded-r-full font-display \
                             text-sm font-bold tracking-[0.12em] uppercase \
                             {affordance} {FOCUS_RING}")
                }
                attr:aria-current=move || on_home.get().then_some("page")
                on:click=close
            >
                "Chronoscope"
            </A>
        </div>

        // Scrim + drawer. Both stay mounted so the slide animates in and out;
        // `pointer-events-none` keeps the closed overlay from eating map clicks.
        <div
            class=format!("fixed inset-0 z-40 {SCRIM}")
            class:pointer-events-none=move || !open.get()
            style=move || if open.get() { "opacity: 1" } else { "opacity: 0" }
        >
            <div class="absolute inset-0 bg-ink/20" on:click=close></div>
            <aside
                node_ref=drawer_ref
                id="site-nav-drawer"
                tabindex="-1"
                role="dialog"
                aria-modal="true"
                aria-label="Site navigation"
                // Mounted even when closed, so the slide has something to
                // animate. Without `inert` that leaves a permanent
                // `aria-modal` dialog in the accessibility tree and four
                // off-screen links in the tab order: `opacity: 0` hides an
                // element from the eye, not from the keyboard.
                inert=move || !open.get()
                class=format!("absolute top-0 left-0 bottom-0 w-60 bg-parchment shadow-lg \
                       outline-none {SLIDE}")
                style=move || if open.get() {
                    "transform: translateX(0)"
                } else {
                    "transform: translateX(-100%)"
                }
            >
                // `pt-12` clears the trigger, which overlays this panel's top
                // corner and serves as its masthead. The drawer carries no
                // wordmark of its own: a second copy at a different size would
                // make the brand appear to jump on open.
                <div class="flex flex-col h-full pt-12">
                    // Inset to the wordmark's own span rather than the drawer's,
                    // so the ornament centres under the name instead of off to
                    // one side. The left inset adds up the masthead ahead of the
                    // text: `left-3` (12), the burger's 44, the 1 px rule, and
                    // the link's `px-4` (16). The right inset is the only figure
                    // that depends on how wide the name renders, which is
                    // tolerable for an ornament and nothing else.
                    // `leading-none` so the line box is the glyph, not the glyph
                    // plus half a line of air above and below it — that leading
                    // is what made the ornament read as a menu row. The margin
                    // below is in `em`, so it stays proportional to the ornament
                    // if its size is ever changed.
                    //
                    // Decorative: hidden from the accessibility tree, where it
                    // would otherwise be announced as stray punctuation, and
                    // unselectable, since dragging across the menu catching a
                    // stray "— ❧ —" is just untidy.
                    <p
                        aria-hidden="true"
                        class="text-center text-sepia/40 text-base leading-none mb-[0.75em] \
                               tracking-[0.35em] pl-[4.5625rem] pr-[2.8rem] select-none \
                               font-ornament"
                    >
                        "\u{2014}\u{00a0}\u{2767}\u{00a0}\u{2014}"
                    </p>

                    // Ordered for a reader arriving cold, so the entries are
                    // written out rather than generated: an article's stem is
                    // its URL, and a rename that stranded one of these would
                    // fail `test_every_nav_drawer_link_reaches_a_real_page`.
                    <nav class="flex-1 px-3">
                        <A href="/" attr:class=NAV_LINK_CLASS on:click=close>"Explore"</A>
                        <A href="/about" attr:class=NAV_LINK_CLASS on:click=close>"About"</A>
                        <A href="/faq" attr:class=NAV_LINK_CLASS on:click=close>"FAQ"</A>
                        <A href="/related-work" attr:class=NAV_LINK_CLASS on:click=close>"Related work"</A>
                    </nav>

                    // Footer area
                    <div class="px-5 py-4 border-t border-sepia/15">
                        <a
                            href="https://github.com/copumpkin/chronoscope"
                            class="text-sepia/60 hover:text-ink text-xs font-sans transition-colors"
                            target="_blank"
                            rel="noopener noreferrer"
                        >
                            "GitHub"
                        </a>
                        <span class="text-sepia/30 text-xs font-sans">" \u{00b7} "</span>
                        <span class="text-sepia/40 text-xs font-sans">"MIT code \u{00b7} CC BY 4.0 data"</span>
                    </div>
                </div>
            </aside>
        </div>
    }
}
