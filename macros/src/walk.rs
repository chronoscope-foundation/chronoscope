//! `IdWalk` derive: the grammar's id-relabel + id-collect walks.
//!
//! Every scheme-world grammar type (`<R: IdScheme>`) needs a `for_each_id`
//! (collect) and `try_map_ids` (fallible relabel) over its `R::Entity` /
//! `R::Event` / `R::Image` leaves. Hand-writing them per type meant a new field
//! or variant compiled fine while silently dropping out of the walk. This derive
//! emits both methods from the field tokens, so a forgotten field becomes a
//! compile error: the emitted call lands on a type carrying no such method.
//!
//! Both methods take all three closures in `fe, fv, fi` order regardless of
//! which kinds a type actually uses — the uniform shape lets one caller drive
//! every cluster's traversal and lets the assertion sums recurse without knowing
//! each cluster's id kinds. The relabel error is pinned to
//! `IdMapError<R2::Entity, R2::Event, R2::Image>`, so a `#[self_loop]` arm names
//! it inline and no per-cluster error generic is needed.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, format_ident, quote};
use syn::{
    Data, DeriveInput, Fields, GenericArgument, Ident, LitStr, PathArguments, Type, TypeParamBound,
    WherePredicate, parse_macro_input,
};

/// One of the three scheme id kinds, in canonical `fe, fv, fi` order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Entity,
    Event,
    Image,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::Entity, Kind::Event, Kind::Image];

    /// The `IdScheme` associated-type name this kind projects (`R::Entity`).
    fn assoc(self) -> &'static str {
        match self {
            Kind::Entity => "Entity",
            Kind::Event => "Event",
            Kind::Image => "Image",
        }
    }

    /// The closure parameter name for this kind.
    fn closure(self) -> &'static str {
        match self {
            Kind::Entity => "fe",
            Kind::Event => "fv",
            Kind::Image => "fi",
        }
    }
}

/// Emit a fatal `compile_error!` spanned at `tokens`.
fn fail(tokens: impl ToTokens, msg: &str) -> TokenStream {
    syn::Error::new_spanned(tokens, msg)
        .to_compile_error()
        .into()
}

/// Whether a trait-bound path names `IdScheme` (by its final segment).
fn bound_is_id_scheme(bound: &TypeParamBound) -> bool {
    matches!(bound, TypeParamBound::Trait(tb)
        if tb.path.segments.last().is_some_and(|s| s.ident == "IdScheme"))
}

/// The type's generic parameter bounded by `IdScheme`, inline or in the
/// `where` clause. The scheme param off which the id kinds are projected.
pub(crate) fn scheme_param(input: &DeriveInput) -> Option<Ident> {
    for tp in input.generics.type_params() {
        if tp.bounds.iter().any(bound_is_id_scheme) {
            return Some(tp.ident.clone());
        }
    }
    if let Some(wc) = &input.generics.where_clause {
        for pred in &wc.predicates {
            if let WherePredicate::Type(pt) = pred
                && let Type::Path(p) = &pt.bounded_ty
                && let Some(ident) = p.path.get_ident()
                && input.generics.type_params().any(|tp| tp.ident == *ident)
                && pt.bounds.iter().any(bound_is_id_scheme)
            {
                return Some(ident.clone());
            }
        }
    }
    None
}

/// If `ty` is exactly `<scheme>::Entity` / `Event` / `Image` (a bare two-segment
/// projection off the scheme param), the kind it names — an id leaf.
fn leaf_kind(ty: &Type, scheme: &Ident) -> Option<Kind> {
    let Type::Path(p) = ty else { return None };
    if p.qself.is_some() || p.path.segments.len() != 2 {
        return None;
    }
    let first = &p.path.segments[0];
    let second = &p.path.segments[1];
    if first.ident != *scheme
        || !matches!(first.arguments, PathArguments::None)
        || !matches!(second.arguments, PathArguments::None)
    {
        return None;
    }
    Kind::ALL.into_iter().find(|k| second.ident == k.assoc())
}

/// The first `<scheme>::Assoc` projection appearing anywhere inside `ty`'s type
/// arguments — the id kind a distinct-pair (`DistinctPair<R::Entity>`) relates.
fn contained_assoc_kind(ty: &Type, scheme: &Ident) -> Option<Kind> {
    if let Some(k) = leaf_kind(ty, scheme) {
        return Some(k);
    }
    let Type::Path(p) = ty else { return None };
    for seg in &p.path.segments {
        if let PathArguments::AngleBracketed(args) = &seg.arguments {
            for arg in &args.args {
                if let GenericArgument::Type(inner) = arg
                    && let Some(k) = contained_assoc_kind(inner, scheme)
                {
                    return Some(k);
                }
            }
        }
    }
    None
}

/// Whether `ty`'s tokens mention `scheme` anywhere — decides a "recurse into
/// this field" from a "carry verbatim".
fn mentions(ty: &Type, scheme: &Ident) -> bool {
    match ty {
        Type::Path(p) => p.path.segments.iter().any(|seg| {
            seg.ident == *scheme
                || matches!(&seg.arguments, PathArguments::AngleBracketed(args)
                    if args.args.iter().any(|arg| matches!(arg,
                        GenericArgument::Type(inner) if mentions(inner, scheme))))
        }),
        Type::Reference(r) => mentions(&r.elem, scheme),
        Type::Tuple(t) => t.elems.iter().any(|e| mentions(e, scheme)),
        Type::Group(g) => mentions(&g.elem, scheme),
        Type::Paren(p) => mentions(&p.elem, scheme),
        _ => false,
    }
}

/// Read a `#[self_loop = "Variant"]` attribute off a field: the `SelfLoop`
/// variant its distinct-pair collapse maps to. `None` when absent.
fn self_loop_variant(field: &syn::Field) -> Result<Option<Ident>, syn::Error> {
    let mut found = None;
    for attr in &field.attrs {
        if attr.path().is_ident("self_loop") {
            if found.is_some() {
                return Err(syn::Error::new_spanned(
                    attr,
                    "duplicate #[self_loop] attribute",
                ));
            }
            let lit: LitStr = attr
                .meta
                .require_name_value()
                .and_then(|nv| syn::parse2::<LitStr>(nv.value.to_token_stream()))?;
            found = Some(Ident::new(&lit.value(), lit.span()));
        }
    }
    Ok(found)
}

/// Per-field walk code.
struct FieldEmit {
    /// `for_each_id` statement; empty for a verbatim field.
    visit: TokenStream2,
    /// Whether the field is bound + visited in `for_each_id` (verbatim fields
    /// fall under `..` so they don't trip `unused_variables`).
    visited: bool,
    /// `try_map_ids` field initializer (`name: <expr>`).
    rebuild: TokenStream2,
}

/// Build one field's walk emit, recording which id kinds it touches into `used`
/// (a recurse field touches all three, since it is handed every closure).
fn field_emit(
    field: &syn::Field,
    access: &TokenStream2,
    name: &Ident,
    scheme: &Ident,
    used: &mut [bool; 3],
) -> Result<FieldEmit, syn::Error> {
    let ty = &field.ty;

    // A `#[self_loop]` field is a distinct pair: visit its ids through the one
    // closure matching the pair's kind, rebuild through the pair's two-closure
    // `try_map_ids` wrapping a collision as `SelfLoop::Variant`.
    if let Some(variant) = self_loop_variant(field)? {
        let Some(kind) = contained_assoc_kind(ty, scheme) else {
            return Err(syn::Error::new_spanned(
                field,
                "#[self_loop] field must reference a scheme id (e.g. DistinctPair<R::Entity>)",
            ));
        };
        used[kind as usize] = true;
        let closure = format_ident!("{}", kind.closure());
        return Ok(FieldEmit {
            visit: quote! { #access.for_each_id(#closure); },
            visited: true,
            rebuild: quote! {
                #name: #access.try_map_ids(#closure, |id| {
                    crate::grammar::identity::IdMapError::SelfLoop(
                        crate::grammar::identity::SelfLoop::#variant(id),
                    )
                })?
            },
        });
    }

    // Field IS a scheme id leaf: dispatch directly to its kind's closure.
    if let Some(kind) = leaf_kind(ty, scheme) {
        used[kind as usize] = true;
        let closure = format_ident!("{}", kind.closure());
        return Ok(FieldEmit {
            visit: quote! { #closure(#access); },
            visited: true,
            rebuild: quote! { #name: #closure(#access)? },
        });
    }

    // Field mentions the scheme param: recurse, handing it all three closures.
    if mentions(ty, scheme) {
        for u in used.iter_mut() {
            *u = true;
        }
        return Ok(FieldEmit {
            visit: quote! { #access.for_each_id(fe, fv, fi); },
            visited: true,
            rebuild: quote! { #name: #access.try_map_ids(fe, fv, fi)? },
        });
    }

    // No scheme id: carry verbatim. UFCS `Clone::clone` so a `Copy` field
    // doesn't trip `clippy::clone_on_copy` (it sees a function call).
    Ok(FieldEmit {
        visit: quote! {},
        visited: false,
        rebuild: quote! { #name: ::core::clone::Clone::clone(#access) },
    })
}

/// The `for_each_id` and `try_map_ids` match/statement bodies plus the set of id
/// kinds any field touches.
fn build_bodies(
    input: &DeriveInput,
    name: &Ident,
    scheme: &Ident,
) -> Result<(TokenStream2, TokenStream2, [bool; 3]), syn::Error> {
    let mut used = [false; 3];
    match &input.data {
        Data::Enum(data) => {
            let mut visit_arms = Vec::new();
            let mut rebuild_arms = Vec::new();
            for variant in &data.variants {
                let vname = &variant.ident;
                let fields = match &variant.fields {
                    Fields::Named(fields) => fields,
                    Fields::Unit => {
                        visit_arms.push(quote! { Self::#vname => {} });
                        rebuild_arms.push(quote! { Self::#vname => #name::#vname, });
                        continue;
                    }
                    Fields::Unnamed(_) => {
                        return Err(syn::Error::new_spanned(
                            variant,
                            "IdWalk requires named-field or unit variants",
                        ));
                    }
                };
                let mut all_binds = Vec::new();
                let mut visit_binds = Vec::new();
                let mut visits = Vec::new();
                let mut rebuilds = Vec::new();
                let field_count = fields.named.len();
                for field in &fields.named {
                    let Some(fname) = &field.ident else { continue };
                    all_binds.push(fname.clone());
                    let access = quote! { #fname };
                    let emit = field_emit(field, &access, fname, scheme, &mut used)?;
                    if emit.visited {
                        visit_binds.push(fname.clone());
                        visits.push(emit.visit);
                    }
                    rebuilds.push(emit.rebuild);
                }
                // `for_each_id` binds only the visited fields; the rest fall
                // under `..`. A leading comma (`{ , .. }`) is a syntax error, so
                // the `..` form branches on whether anything binds.
                let visit_pat = if visit_binds.is_empty() {
                    quote! { .. }
                } else if visit_binds.len() < field_count {
                    quote! { #(#visit_binds),* , .. }
                } else {
                    quote! { #(#visit_binds),* }
                };
                visit_arms.push(quote! {
                    Self::#vname { #visit_pat } => { #(#visits)* }
                });
                rebuild_arms.push(quote! {
                    Self::#vname { #(#all_binds),* } => #name::#vname { #(#rebuilds),* },
                });
            }
            Ok((
                quote! { match self { #(#visit_arms)* } },
                quote! { ::core::result::Result::Ok(match self { #(#rebuild_arms)* }) },
                used,
            ))
        }
        Data::Struct(data) => {
            let Fields::Named(fields) = &data.fields else {
                return Err(syn::Error::new_spanned(
                    &data.fields,
                    "IdWalk requires named struct fields",
                ));
            };
            let mut visits = Vec::new();
            let mut rebuilds = Vec::new();
            for field in &fields.named {
                let Some(fname) = &field.ident else { continue };
                // A struct field is a place, not a match binding, so borrow it
                // for the closures. Parens keep the `&` on the field, not on a
                // trailing method call.
                let access = quote! { (&self.#fname) };
                let emit = field_emit(field, &access, fname, scheme, &mut used)?;
                visits.push(emit.visit);
                rebuilds.push(emit.rebuild);
            }
            Ok((
                quote! { #(#visits)* },
                quote! { ::core::result::Result::Ok(#name { #(#rebuilds),* }) },
                used,
            ))
        }
        Data::Union(_) => Err(syn::Error::new_spanned(
            input,
            "IdWalk cannot derive on a union",
        )),
    }
}

/// A fresh id-scheme output-param ident that doesn't collide with an existing
/// generic param.
fn output_scheme_ident(input: &DeriveInput) -> Ident {
    let mut candidate = String::from("R2");
    while input.generics.type_params().any(|tp| tp.ident == candidate) {
        candidate.push('_');
    }
    format_ident!("{}", candidate)
}

/// The output type's generic-argument list: the scheme param replaced by `r2`,
/// every other param carried through unchanged.
fn output_type_args(input: &DeriveInput, scheme: &Ident, r2: &Ident) -> Vec<TokenStream2> {
    input
        .generics
        .params
        .iter()
        .map(|p| match p {
            syn::GenericParam::Type(tp) if tp.ident == *scheme => quote! { #r2 },
            syn::GenericParam::Type(tp) => {
                let id = &tp.ident;
                quote! { #id }
            }
            syn::GenericParam::Lifetime(lt) => {
                let l = &lt.lifetime;
                quote! { #l }
            }
            syn::GenericParam::Const(c) => {
                let id = &c.ident;
                quote! { #id }
            }
        })
        .collect()
}

/// Derive `for_each_id` + `try_map_ids` from a grammar type's field tokens.
pub fn derive_id_walk(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);

    let Some(scheme) = scheme_param(&input) else {
        return fail(
            &input,
            "IdWalk needs a generic parameter bounded by IdScheme (expected `R: IdScheme`)",
        );
    };

    let name = &input.ident;
    let (visit_body, rebuild_body, used) = match build_bodies(&input, name, &scheme) {
        Ok(bodies) => bodies,
        Err(e) => return e.to_compile_error().into(),
    };

    let r2 = output_scheme_ident(&input);

    // Every method takes all three closures. A closure for an id kind the type
    // never touches is named `_fe` / `_fv` / `_fi` so a single-kind type doesn't
    // trip `unused_variables`; the body only references the used (non-underscore)
    // names.
    let closure_names: Vec<Ident> = Kind::ALL
        .iter()
        .enumerate()
        .map(|(i, k)| {
            if used[i] {
                format_ident!("{}", k.closure())
            } else {
                format_ident!("_{}", k.closure())
            }
        })
        .collect();

    let err_ty = quote! {
        crate::grammar::identity::IdMapError<#r2::Entity, #r2::Event, #r2::Image>
    };

    let visit_params = Kind::ALL.iter().zip(&closure_names).map(|(k, nm)| {
        let assoc = format_ident!("{}", k.assoc());
        quote! { #nm: &mut impl ::core::ops::FnMut(&#scheme::#assoc) }
    });
    let map_params = Kind::ALL.iter().zip(&closure_names).map(|(k, nm)| {
        let assoc = format_ident!("{}", k.assoc());
        quote! {
            #nm: &mut impl ::core::ops::FnMut(&#scheme::#assoc)
                -> ::core::result::Result<#r2::#assoc, #err_ty>
        }
    });

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let out_args = output_type_args(&input, &scheme, &r2);

    quote! {
        impl #impl_generics #name #ty_generics #where_clause {
            /// Visit every scheme id this value mentions, dispatching each to its
            /// kind's closure. Generated by `#[derive(IdWalk)]`.
            pub fn for_each_id(
                &self,
                #(#visit_params),*
            ) {
                #visit_body
            }

            /// Relabel every scheme id through the kind-matching fallible
            /// closure, producing the same shape over the output scheme `R2`.
            /// Generated by `#[derive(IdWalk)]`.
            pub fn try_map_ids<#r2: crate::grammar::ids::IdScheme>(
                &self,
                #(#map_params),*
            ) -> ::core::result::Result<#name<#(#out_args),*>, #err_ty> {
                #rebuild_body
            }
        }
    }
    .into()
}
