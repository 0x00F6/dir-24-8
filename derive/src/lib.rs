//! `#[derive(DeepMerge)]` for [`dir-24-8`](https://crates.io/crates/dir-24-8).
//!
//! Do not depend on this crate directly: enable the `derive` feature of
//! `dir-24-8`, which re-exports the macro next to the trait it implements.
//!
//! ```toml
//! [dependencies]
//! dir-24-8 = { version = "0.1", features = ["derive"] }
//! ```
//!
//! ```
//! use dir_24_8::DeepMerge;
//!
//! #[derive(DeepMerge, Debug, PartialEq)]
//! struct Info {
//!     tags: Vec<String>, // concatenated
//!     asn: u32,          // incoming value wins
//! }
//!
//! let mut info = Info { tags: vec!["cdn".into()], asn: 64500 };
//! info.deep_merge(Info { tags: vec!["anycast".into()], asn: 64501 });
//! assert_eq!(info, Info { tags: vec!["cdn".into(), "anycast".into()], asn: 64501 });
//! ```
//!
//! See [`DeepMerge`](macro@DeepMerge) for the generated code and the
//! attributes.

use proc_macro::TokenStream;
use proc_macro2::{TokenStream as TokenStream2, TokenTree};
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Field, Fields, Ident, Path, WherePredicate, parse_macro_input, parse_quote,
};

/// Derives `dir_24_8::DeepMerge`: every field of the incoming value is
/// merged into the same field of the existing value with that field's own
/// `DeepMerge` implementation (maps merge per key, vectors concatenate,
/// `Option` keeps or adopts, scalars and strings are replaced, nested
/// structures recurse).
///
/// * **Structs** (named, tuple or unit fields): field by field.
/// * **Enums**: if both values are the same variant, field by field;
///   otherwise the incoming value replaces the existing one.
/// * **Generics**: a `T: DeepMerge` bound is added for every merged field
///   whose type uses a type parameter.
///
/// Field attributes:
///
/// | Attribute | Effect |
/// |---|---|
/// | *(none)* | `existing.field.deep_merge(incoming.field)` |
/// | `#[deep_merge(replace)]` | the incoming value replaces the existing one (no `DeepMerge` needed) |
/// | `#[deep_merge(skip)]` | the existing value is kept, the incoming one is dropped |
///
/// Container attribute `#[deep_merge(crate = "path")]` sets the path of the
/// `dir_24_8` crate when it is re-exported under another name.
///
/// ```
/// use std::collections::BTreeMap;
/// use dir_24_8::DeepMerge;
///
/// #[derive(DeepMerge, Debug, PartialEq)]
/// struct Route {
///     communities: Vec<u32>,
///     attributes: BTreeMap<String, String>,
///     #[deep_merge(skip)]
///     first_seen: u64,
///     origin: Origin,
/// }
///
/// #[derive(DeepMerge, Debug, PartialEq)]
/// enum Origin {
///     Igp { metric: u32 },
///     Egp(u32),
///     Incomplete,
/// }
///
/// let mut r = Route {
///     communities: vec![1],
///     attributes: BTreeMap::from([("region".into(), "eu".into())]),
///     first_seen: 100,
///     origin: Origin::Igp { metric: 10 },
/// };
/// r.deep_merge(Route {
///     communities: vec![2],
///     attributes: BTreeMap::from([("pop".into(), "par".into())]),
///     first_seen: 200,
///     origin: Origin::Egp(64500),
/// });
/// assert_eq!(r.communities, [1, 2]);
/// assert_eq!(r.attributes.len(), 2);
/// assert_eq!(r.first_seen, 100); // skipped
/// assert_eq!(r.origin, Origin::Egp(64500)); // other variant: replaced
/// ```
#[proc_macro_derive(DeepMerge, attributes(deep_merge))]
pub fn derive_deep_merge(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Strategy {
    Merge,
    Replace,
    Skip,
}

fn field_strategy(f: &Field) -> syn::Result<Strategy> {
    let mut strategy = Strategy::Merge;
    let mut seen = false;
    for attr in f.attrs.iter().filter(|a| a.path().is_ident("deep_merge")) {
        attr.parse_nested_meta(|meta| {
            let s = if meta.path.is_ident("replace") {
                Strategy::Replace
            } else if meta.path.is_ident("skip") {
                Strategy::Skip
            } else {
                return Err(meta.error("expected `replace` or `skip`"));
            };
            if seen {
                return Err(meta.error("at most one of `replace` / `skip` per field"));
            }
            seen = true;
            strategy = s;
            Ok(())
        })?;
    }
    Ok(strategy)
}

fn crate_path(input: &DeriveInput) -> syn::Result<Path> {
    let mut path: Path = parse_quote!(::dir_24_8);
    for attr in input
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("deep_merge"))
    {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                let lit: syn::LitStr = meta.value()?.parse()?;
                path = lit.parse()?;
                Ok(())
            } else {
                Err(meta.error("expected `crate = \"path\"`"))
            }
        })?;
    }
    Ok(path)
}

/// `true` when `tokens` mention one of `params`.
fn uses_param(tokens: TokenStream2, params: &[Ident]) -> bool {
    tokens.into_iter().any(|t| match t {
        TokenTree::Ident(i) => params.contains(&i),
        TokenTree::Group(g) => uses_param(g.stream(), params),
        _ => false,
    })
}

/// Bindings and per-field statements for one set of fields.
struct FieldPlan {
    /// Pattern of the fields (`{ a: inc_a, .. }`, `(inc_0, ..)` or empty).
    incoming: TokenStream2,
    /// Pattern binding the existing fields (`{ a: cur_a }` ...).
    existing: TokenStream2,
    /// Statements merging `cur_*` (mutable references) with `inc_*`.
    merge: Vec<TokenStream2>,
    /// Constructor arguments rebuilding the incoming value.
    rebuild: TokenStream2,
}

fn plan(
    fields: &Fields,
    krate: &Path,
    params: &[Ident],
    bounds: &mut Vec<WherePredicate>,
) -> syn::Result<FieldPlan> {
    let mut inc = Vec::new();
    let mut cur = Vec::new();
    let mut merge = Vec::new();
    let mut names = Vec::new();
    for (i, f) in fields.iter().enumerate() {
        let strategy = field_strategy(f)?;
        let i_bind = format_ident!("__incoming_{}", i);
        let c_bind = format_ident!("__existing_{}", i);
        match &f.ident {
            Some(name) => {
                inc.push(quote!(#name: #i_bind));
                cur.push(quote!(#name: #c_bind));
                names.push(quote!(#name: #i_bind));
            }
            None => {
                inc.push(quote!(#i_bind));
                cur.push(quote!(#c_bind));
                names.push(quote!(#i_bind));
            }
        }
        merge.push(match strategy {
            Strategy::Merge => {
                let ty = &f.ty;
                if uses_param(quote!(#ty), params) {
                    bounds.push(parse_quote!(#ty: #krate::DeepMerge));
                }
                quote!(#krate::DeepMerge::deep_merge(#c_bind, #i_bind);)
            }
            Strategy::Replace => quote!(*#c_bind = #i_bind;),
            Strategy::Skip => quote!(let _ = (#c_bind, #i_bind);),
        });
    }
    let (incoming, existing, rebuild) = match fields {
        Fields::Named(_) => (
            quote!({ #(#inc),* }),
            quote!({ #(#cur),* }),
            quote!({ #(#names),* }),
        ),
        Fields::Unnamed(_) => (
            quote!(( #(#inc),* )),
            quote!(( #(#cur),* )),
            quote!(( #(#names),* )),
        ),
        Fields::Unit => (quote!(), quote!(), quote!()),
    };
    Ok(FieldPlan {
        incoming,
        existing,
        merge,
        rebuild,
    })
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let krate = crate_path(&input)?;
    let name = &input.ident;
    let params: Vec<Ident> = input
        .generics
        .type_params()
        .map(|p| p.ident.clone())
        .collect();
    let mut bounds = Vec::new();
    let body = match &input.data {
        Data::Struct(s) => {
            let p = plan(&s.fields, &krate, &params, &mut bounds)?;
            let (inc, cur, merge) = (p.incoming, p.existing, p.merge);
            quote! {
                let Self #inc = incoming;
                let Self #cur = self;
                #(#merge)*
            }
        }
        Data::Enum(e) => {
            let mut arms = Vec::new();
            for v in &e.variants {
                let vname = &v.ident;
                let p = plan(&v.fields, &krate, &params, &mut bounds)?;
                let (inc, cur, merge, rebuild) = (p.incoming, p.existing, p.merge, p.rebuild);
                arms.push(quote! {
                    Self::#vname #inc => {
                        #[allow(unreachable_patterns)]
                        match self {
                            Self::#vname #cur => { #(#merge)* }
                            __other => *__other = Self::#vname #rebuild,
                        }
                    }
                });
            }
            if arms.is_empty() {
                quote!(match incoming {})
            } else {
                quote!(match incoming { #(#arms)* })
            }
        }
        Data::Union(u) => {
            return Err(syn::Error::new(
                u.union_token.span,
                "DeepMerge cannot be derived for unions",
            ));
        }
    };
    let mut generics = input.generics.clone();
    generics.make_where_clause().predicates.extend(bounds);
    let (impl_g, ty_g, where_c) = generics.split_for_impl();
    Ok(quote! {
        #[automatically_derived]
        impl #impl_g #krate::DeepMerge for #name #ty_g #where_c {
            #[inline]
            fn deep_merge(&mut self, incoming: Self) {
                #body
            }
        }
    })
}

// The crates.io page (README.md) examples are compiled and run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
