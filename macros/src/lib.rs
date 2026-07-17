//! Procedural macros for the Chronoscope fact-store grammar.
//!
//! This is `chronoscope-core`'s dedicated companion crate: Rust keeps a
//! proc-macro (`proc-macro = true`) crate separate from the one exporting a
//! normal API, so core's grammar derives live here. [`macro@IdWalk`] expands to
//! `crate::grammar::{identity, ids}` paths that resolve in the deriving crate,
//! making it derivable within `chronoscope-core`.
//!
//! [`grammar_type`] declares a serializable grammar sum (enum) or product
//! (struct). It bundles the project's serde conventions and forbids
//! tuple/newtype shapes, which serde internal tagging would flatten beside
//! the tag and collide.
//!
//! [`macro@IdWalk`] derives the grammar's id-relabel + id-collect walks
//! (`for_each_id` / `try_map_ids`) from a scheme-world type's field tokens.

mod walk;

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, parse_macro_input};

/// Derive `for_each_id` + `try_map_ids` over a grammar type's scheme id leaves.
///
/// The type must carry a generic parameter bounded by `IdScheme` (spelled `R`
/// by convention). Fields typed exactly `R::Entity` / `R::Event` / `R::Image`
/// are id leaves; fields whose type otherwise mentions `R` are recursed into;
/// everything else is cloned verbatim. Both generated methods take all three
/// closures in `fe, fv, fi` order, and the relabel pins its error to
/// `IdMapError<R2::Entity, R2::Event, R2::Image>`.
///
/// A distinct-pair field annotated with `#[self_loop = "Variant"]` is rebuilt
/// through the pair's smart constructor, wrapping a post-map collision as
/// `SelfLoop::Variant`.
///
/// The expansion names `crate::grammar::identity::{IdMapError, SelfLoop}` and
/// `crate::grammar::ids::IdScheme`, which resolve in `chronoscope-core`.
#[proc_macro_derive(IdWalk, attributes(self_loop))]
pub fn derive_id_walk(item: TokenStream) -> TokenStream {
    walk::derive_id_walk(item)
}

/// Declare a grammar sum or product with the standard serde bundle.
///
/// Derives `Serialize`, `Deserialize`, `JsonSchema`, and applies
/// `deny_unknown_fields`. Enums also get internal tagging on `"type"` with
/// `snake_case` variant names.
///
/// Every variant must be a struct or unit variant; every struct must use
/// named fields. Tuple/newtype shapes are a compile error: under internal
/// tagging an unnamed payload flattens beside the tag, colliding when it wraps
/// another tagged type and failing for scalars. For a validated single-field
/// wrapper, use the `*_newtype!` macros.
///
/// `Debug`, `Clone`, and the comparison derives are left to the caller (write
/// `#[derive(..)]` alongside): the float-bearing grammar types carry
/// hand-written `Eq`/`Hash`/`Ord` for `-0.0` normalisation that a blanket
/// derive would clash with. A missing comparison derive surfaces at the use
/// site (e.g. a `BTreeSet` key).
///
/// Generics, `where`-clauses, `#[serde(bound(..))]`, and per-variant
/// `#[serde(rename = "..")]` pass through untouched.
///
/// A type generic over `IdScheme` (a param bounded `R: IdScheme`) additionally
/// derives [`macro@IdWalk`] and emits the uniform `R: IdScheme` serde/schemars
/// bounds, so the grammar types stop repeating them. Validated products that
/// can't be `grammar_type` spell `#[derive(IdWalk)]` themselves.
///
/// ```compile_fail
/// use chronoscope_macros::grammar_type;
/// #[grammar_type]
/// enum Bad {
///     Wraps(u32), // tuple/newtype variant — rejected at compile time
/// }
/// ```
#[proc_macro_attribute]
pub fn grammar_type(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);

    if let Some(err) = reject_unnamed(&input) {
        return err.to_compile_error().into();
    }

    let serde_attrs = match &input.data {
        Data::Enum(_) => quote! {
            #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
        },
        _ => quote! {
            #[serde(deny_unknown_fields)]
        },
    };

    // A type generic over `R: IdScheme` also derives `IdWalk` and carries the
    // uniform scheme bounds, so the grammar types stop repeating them. Full
    // paths keep both resolving regardless of the module's imports.
    let (id_walk_derive, scheme_bounds) = match walk::scheme_param(&input) {
        Some(scheme) => {
            let scheme = scheme.to_string();
            let serde_bound = format!("{scheme}: crate::grammar::ids::IdScheme");
            let schemars_bound =
                format!("{scheme}: crate::grammar::ids::IdScheme + ::schemars::JsonSchema");
            (
                quote! { , ::chronoscope_macros::IdWalk },
                quote! {
                    #[serde(bound(serialize = #serde_bound, deserialize = #serde_bound))]
                    #[schemars(bound = #schemars_bound)]
                },
            )
        }
        None => (quote! {}, quote! {}),
    };

    quote! {
        #[derive(::serde::Serialize, ::serde::Deserialize, ::schemars::JsonSchema #id_walk_derive)]
        #serde_attrs
        #scheme_bounds
        #input
    }
    .into()
}

/// Reject the tuple/newtype shapes that serde internal tagging flattens.
fn reject_unnamed(input: &DeriveInput) -> Option<syn::Error> {
    const ENUM_MSG: &str = "grammar_type enums must use struct or unit variants, not tuple/newtype \
        variants: serde internal tagging flattens an unnamed payload beside the tag, which collides \
        when it wraps another tagged type and fails for scalars. Use `Variant { field: T }`.";
    const STRUCT_MSG: &str = "grammar_type structs must use named fields, not a tuple struct. For a \
        validated single-field wrapper, use the dedicated `*_newtype!` macros.";
    const UNION_MSG: &str = "grammar_type cannot be applied to a union.";

    match &input.data {
        Data::Enum(data) => data
            .variants
            .iter()
            .find_map(|variant| match variant.fields {
                Fields::Unnamed(_) => Some(syn::Error::new_spanned(variant, ENUM_MSG)),
                Fields::Named(_) | Fields::Unit => None,
            }),
        Data::Struct(data) => match data.fields {
            Fields::Unnamed(_) => Some(syn::Error::new_spanned(input, STRUCT_MSG)),
            Fields::Named(_) | Fields::Unit => None,
        },
        Data::Union(_) => Some(syn::Error::new_spanned(input, UNION_MSG)),
    }
}
