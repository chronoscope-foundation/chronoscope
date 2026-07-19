//! The grammar's field walks: `IdWalk` (id relabel + collect) and `DateWalk`
//! (the `UncertainDate` role-tagging walk).
//!
//! Every scheme-world grammar type (`<R: IdScheme>`) needs a `for_each_id`
//! (collect) and `try_map_ids` (fallible relabel) over its `R::Entity` /
//! `R::Event` / `R::Image` leaves. Hand-writing them per type meant a new field
//! or variant compiled fine while silently dropping out of the walk. These
//! derives emit the inherent methods from the field tokens, so a forgotten field
//! becomes a compile error: the emitted call lands on a type carrying no such
//! method, or a missing role annotation expands to `compile_error!`.
//!
//! Both id methods take all three closures in `fe, fv, fi` order regardless of
//! which kinds a type actually uses — the uniform shape lets one caller drive
//! every cluster's traversal and lets the assertion sums recurse without knowing
//! each cluster's id kinds. The relabel error is pinned to
//! `IdMapError<R2::Entity, R2::Event, R2::Image>`, so a `#[self_loop]` arm names
//! it inline and no per-cluster error generic is needed.
//!
//! `DateWalk` emits an inherent `visit_dates` method (symmetric with the id
//! walks), tagging each `UncertainDate` a value reaches with its
//! [`DateRole`](crate::submit::error::DateRole). `R`-parameterization marks an
//! interior node of the fact grammar — a sub-fact worth descending into — so the
//! walk recurses into a field whose type mentions the scheme param `R` (id
//! leaves excepted: a bare scheme id param or an `R::Entity` / `Event` / `Image`
//! projection host no dates) and, through the `#[traverse]` escape hatch, into
//! the rare interior node that isn't `R`-parametrized. Non-`R` types are leaves
//! where date-recursion stops. A `#[date_role]` field marks the date leaves; an
//! `UncertainDate` field with no `#[date_role]` fails to compile.

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

/// Read a `#[name = "Ident"]` string attribute off a field, turning its string
/// literal into an [`Ident`]. `None` when absent; an `Err` on a duplicate or a
/// non-string-literal value. Shared by `#[self_loop]` and `#[date_role]`.
fn string_ident_attr(field: &syn::Field, attr_name: &str) -> Result<Option<Ident>, syn::Error> {
    let mut found = None;
    for attr in &field.attrs {
        if attr.path().is_ident(attr_name) {
            if found.is_some() {
                return Err(syn::Error::new_spanned(
                    attr,
                    format!("duplicate #[{attr_name}] attribute"),
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

/// Read a `#[self_loop = "Variant"]` attribute off a field: the `SelfLoop`
/// variant its distinct-pair collapse maps to. `None` when absent.
fn self_loop_variant(field: &syn::Field) -> Result<Option<Ident>, syn::Error> {
    string_ident_attr(field, "self_loop")
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

// ============================================================================
// DateWalk
// ============================================================================

/// If a type is `Option<Inner>`, return `Inner`.
fn option_inner(ty: &Type) -> Option<&Type> {
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
        && seg.ident == "Option"
        && let PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(GenericArgument::Type(inner)) = args.args.first()
    {
        return Some(inner);
    }
    None
}

/// Whether a type's tokens mention `UncertainDate` anywhere — the "field carries
/// a date" test.
fn mentions_date(ty: &Type) -> bool {
    mentions(ty, &format_ident!("UncertainDate"))
}

/// Whether `ty` is exactly one of the deriving type's generic parameters (a
/// bare `T` field, no path or arguments) — a scheme's id type in this grammar,
/// so it hosts no dates and the walk skips it.
fn is_bare_type_param(ty: &Type, type_params: &[Ident]) -> bool {
    let Type::Path(p) = ty else { return false };
    p.qself.is_none()
        && p.path.segments.len() == 1
        && matches!(p.path.segments[0].arguments, PathArguments::None)
        && type_params.iter().any(|tp| *tp == p.path.segments[0].ident)
}

/// Whether a field carries the bare `#[traverse]` marker — the escape hatch
/// forcing date-recursion into an interior node that isn't `R`-parametrized.
fn has_traverse(field: &syn::Field) -> bool {
    field
        .attrs
        .iter()
        .any(|attr| attr.path().is_ident("traverse"))
}

/// Per-field `visit_dates` code for one named field, given its accessor.
///
/// A `#[date_role]` field is an `UncertainDate` (or `Option<UncertainDate>`)
/// visited with its role. Recursion descends only into interior nodes of the
/// grammar: a field whose type mentions the scheme param `R` (an id-leaf
/// projection or a bare id param excepted — those are ids, not sub-facts), or
/// one flagged `#[traverse]`. Every other field is a non-`R` leaf where the walk
/// stops; it's elided (`None`) so it falls under the binding's `..` and needs no
/// `visit_dates` method.
fn date_field_emit(
    field: &syn::Field,
    access: &TokenStream2,
    scheme: Option<&Ident>,
    type_params: &[Ident],
) -> Result<Option<TokenStream2>, syn::Error> {
    let ty = &field.ty;

    if let Some(role) = string_ident_attr(field, "date_role")? {
        let role_path = quote! { crate::submit::error::DateRole::#role };
        if let Some(inner) = option_inner(ty) {
            if !mentions_date(inner) {
                return Err(syn::Error::new_spanned(
                    field,
                    "Option-wrapped date field must wrap UncertainDate directly",
                ));
            }
            return Ok(Some(quote! {
                if let ::core::option::Option::Some(d) = #access {
                    f(#role_path, d);
                }
            }));
        }
        if !mentions_date(ty) {
            return Err(syn::Error::new_spanned(
                field,
                "#[date_role] field must be UncertainDate or Option<UncertainDate>",
            ));
        }
        return Ok(Some(quote! { f(#role_path, #access); }));
    }

    // Recurse into an interior node: a field whose type mentions the scheme
    // param, id leaves excepted (a bare scheme id param or an `R::Assoc`
    // projection host no dates), or one flagged `#[traverse]`. The call resolves
    // to the field type's own inherent `visit_dates`.
    let recurse_scheme = scheme.is_some_and(|s| {
        mentions(ty, s) && leaf_kind(ty, s).is_none() && !is_bare_type_param(ty, type_params)
    });
    if has_traverse(field) || recurse_scheme {
        return Ok(Some(quote! { #access.visit_dates(f); }));
    }

    // A date-typed field with no `#[date_role]` is the forgotten-annotation bug
    // the derive exists to catch.
    if mentions_date(ty) {
        return Err(syn::Error::new_spanned(
            field,
            "UncertainDate field needs #[date_role = \"...\"]",
        ));
    }

    // A non-`R` leaf: the walk stops here.
    Ok(None)
}

/// Derive `DateWalk` from a type's field tokens: visit each `#[date_role]`
/// `UncertainDate`, recurse into every interior node (an `R`-mentioning field
/// or a `#[traverse]` one), and stop at every non-`R` leaf.
pub fn derive_date_walk(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    let name = &input.ident;
    let scheme = scheme_param(&input);
    let type_params: Vec<Ident> = input
        .generics
        .type_params()
        .map(|tp| tp.ident.clone())
        .collect();

    let body = match &input.data {
        Data::Enum(data) => {
            let mut arms = Vec::new();
            for variant in &data.variants {
                let vname = &variant.ident;
                match &variant.fields {
                    Fields::Named(fields) => {
                        let mut binds = Vec::new();
                        let mut emits = Vec::new();
                        let field_count = fields.named.len();
                        for field in &fields.named {
                            let Some(fname) = &field.ident else { continue };
                            let access = quote! { #fname };
                            match date_field_emit(field, &access, scheme.as_ref(), &type_params) {
                                // Bind only the fields that emit; the rest fall
                                // under `..` so an id-leaf field isn't unused.
                                Ok(Some(e)) => {
                                    binds.push(fname.clone());
                                    emits.push(e);
                                }
                                Ok(None) => {}
                                Err(e) => return e.to_compile_error().into(),
                            }
                        }
                        // `{ a, b, .. }` when some fields are elided; `{ .. }`
                        // when none bind; `{ a, b }` when all do. A leading comma
                        // (`{ , .. }`) is a syntax error, so the `..` form
                        // branches on whether anything binds.
                        let pat = if binds.is_empty() {
                            quote! { .. }
                        } else if binds.len() < field_count {
                            quote! { #(#binds),* , .. }
                        } else {
                            quote! { #(#binds),* }
                        };
                        arms.push(quote! {
                            Self::#vname { #pat } => { #(#emits)* }
                        });
                    }
                    Fields::Unit => arms.push(quote! { Self::#vname => {} }),
                    Fields::Unnamed(_) => {
                        return fail(variant, "DateWalk requires named-field variants");
                    }
                }
            }
            quote! { match self { #(#arms)* } }
        }
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => {
                let mut emits = Vec::new();
                for field in &fields.named {
                    let Some(fname) = &field.ident else { continue };
                    // Borrow the field so a bare-`UncertainDate` field passes `&`
                    // to the visitor, matching the reference an enum binding
                    // yields. Parens keep the `&` on the field, not a trailing
                    // method call.
                    let access = quote! { (&self.#fname) };
                    match date_field_emit(field, &access, scheme.as_ref(), &type_params) {
                        Ok(Some(e)) => emits.push(e),
                        Ok(None) => {}
                        Err(e) => return e.to_compile_error().into(),
                    }
                }
                quote! { #(#emits)* }
            }
            // A unit struct reaches no dates — `grammar_type` accepts it, so
            // `DateWalk` must too (an empty walk).
            Fields::Unit => quote! {},
            Fields::Unnamed(_) => {
                return fail(&data.fields, "DateWalk requires named struct fields");
            }
        },
        Data::Union(_) => return fail(&input, "DateWalk cannot derive on a union"),
    };

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    quote! {
        impl #impl_generics #name #ty_generics #where_clause {
            /// Visit every `UncertainDate` this value reaches, tagging each with
            /// its `DateRole`. Generated by `#[derive(DateWalk)]`.
            pub fn visit_dates(
                &self,
                f: &mut dyn ::core::ops::FnMut(
                    crate::submit::error::DateRole,
                    &crate::date::UncertainDate,
                ),
            ) {
                #body
            }
        }
    }
    .into()
}
