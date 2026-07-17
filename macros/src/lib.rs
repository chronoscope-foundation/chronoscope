//! Procedural macros for the Chronoscope fact-store grammar.
//!
//! [`grammar_type`] declares a serializable grammar sum (enum) or product
//! (struct). It bundles the project's serde conventions and forbids
//! tuple/newtype shapes, which serde internal tagging would flatten beside
//! the tag and collide.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, parse_macro_input};

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

    quote! {
        #[derive(::serde::Serialize, ::serde::Deserialize, ::schemars::JsonSchema)]
        #serde_attrs
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
