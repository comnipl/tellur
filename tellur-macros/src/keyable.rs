//! Codegen for the `#[derive(Keyable)]` macro and the float-aware
//! `PartialEq`/`Eq`/`Hash` bodies the `#[component]` macro reuses.
//!
//! `f32`/`f64` implement neither `Eq` nor `Hash` because their `==` is not
//! reflexive (`NaN != NaN`). This derive sidesteps that by defining a type's
//! equality and hashing in terms of the raw **bit pattern** of every float it
//! contains (via [`to_bits`](f32::to_bits) and
//! [`hash_f32`](::tellur_core::dyn_compare::hash_f32)). Bit equality *is*
//! reflexive — a `NaN`'s bits equal themselves — so the generated trio is
//! mutually consistent (`a == b` ⟹ `hash(a) == hash(b)`) and a sound `Eq`,
//! which the stock derives cannot produce for float-bearing types.
//!
//! Floats are matched through the transparent containers `Option<_>`, `Vec<_>`,
//! `[_]`, `[_; N]`, and tuples; every other field type is delegated to its own
//! `PartialEq`/`Hash`, so nested value types handle their own floats and stay
//! byte-for-byte compatible with the hand-written impls this replaces.
//!
//! `impl Eq` is emitted unconditionally (it is a marker, so the compiler does
//! not check the fields). That is sound as long as every delegated, non-float
//! field has a reflexive `PartialEq` — which holds for any `Eq` field, for the
//! project's bit-identity value types, and for `Font`'s pointer-identity eq. For
//! a `Box<dyn _Component>` field it rests on every concrete component being
//! reflexive, the same assumption the render cache already makes. Do not derive
//! `Keyable` for a type holding a field whose equality is non-reflexive.

use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Data, DataEnum, DeriveInput, Fields, GenericArgument, Ident, Index, PathArguments, Type,
};

/// A field term: its type plus the place expressions to reach it on `self`
/// (and, for equality, on `other`).
pub(crate) type EqTerm = (Type, TokenStream2, TokenStream2);
pub(crate) type HashTerm = (Type, TokenStream2);

/// Entry point for `#[derive(Keyable)]`.
pub(crate) fn derive(input: DeriveInput) -> syn::Result<TokenStream2> {
    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let (eq_body, hash_body) = match &input.data {
        Data::Struct(s) => (
            struct_eq_body(&s.fields),
            struct_hash_body(&s.fields, &quote!(state)),
        ),
        Data::Enum(e) => enum_bodies(ident, e),
        Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                ident,
                "Keyable cannot be derived for unions",
            ));
        }
    };

    Ok(quote! {
        impl #impl_generics ::core::cmp::PartialEq for #ident #ty_generics #where_clause {
            fn eq(&self, other: &Self) -> bool {
                #eq_body
            }
        }

        impl #impl_generics ::core::cmp::Eq for #ident #ty_generics #where_clause {}

        impl #impl_generics ::core::hash::Hash for #ident #ty_generics #where_clause {
            fn hash<__H: ::core::hash::Hasher>(&self, state: &mut __H) {
                #hash_body
            }
        }
    })
}

/// Equality body over a struct's fields. Each place is borrowed (`&self.x`)
/// before being handed to [`eq_for`], which expects a `&T` expression.
pub(crate) fn struct_eq_body(fields: &Fields) -> TokenStream2 {
    let terms = eq_terms(fields);
    eq_body_from(&terms)
}

/// Hash body over a struct's fields, hashing into `state`.
pub(crate) fn struct_hash_body(fields: &Fields, state: &TokenStream2) -> TokenStream2 {
    let terms = hash_terms(fields);
    hash_body_from(&terms, state)
}

/// Builds the `&&`-joined equality expression from pre-collected terms. Used
/// by both the derive and the `#[component]` fn-form (which assembles its own
/// terms from the synthesized struct fields).
pub(crate) fn eq_body_from(terms: &[EqTerm]) -> TokenStream2 {
    if terms.is_empty() {
        return quote!(true);
    }
    let parts = terms
        .iter()
        .map(|(ty, a, b)| eq_for(ty, &quote!(&#a), &quote!(&#b)));
    quote!( true #( && (#parts) )* )
}

/// Builds the sequence of hashing statements from pre-collected terms.
pub(crate) fn hash_body_from(terms: &[HashTerm], state: &TokenStream2) -> TokenStream2 {
    let parts = terms
        .iter()
        .map(|(ty, v)| hash_for(ty, &quote!(&#v), state));
    quote!( #( #parts )* )
}

fn eq_terms(fields: &Fields) -> Vec<EqTerm> {
    match fields {
        Fields::Named(named) => named
            .named
            .iter()
            .map(|f| {
                let id = f.ident.as_ref().unwrap();
                (f.ty.clone(), quote!(self.#id), quote!(other.#id))
            })
            .collect(),
        Fields::Unnamed(unnamed) => unnamed
            .unnamed
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let idx = Index::from(i);
                (f.ty.clone(), quote!(self.#idx), quote!(other.#idx))
            })
            .collect(),
        Fields::Unit => Vec::new(),
    }
}

fn hash_terms(fields: &Fields) -> Vec<HashTerm> {
    match fields {
        Fields::Named(named) => named
            .named
            .iter()
            .map(|f| {
                let id = f.ident.as_ref().unwrap();
                (f.ty.clone(), quote!(self.#id))
            })
            .collect(),
        Fields::Unnamed(unnamed) => unnamed
            .unnamed
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let idx = Index::from(i);
                (f.ty.clone(), quote!(self.#idx))
            })
            .collect(),
        Fields::Unit => Vec::new(),
    }
}

fn enum_bodies(ident: &Ident, e: &DataEnum) -> (TokenStream2, TokenStream2) {
    let mut eq_arms = Vec::new();
    let mut hash_arms = Vec::new();

    for (vi, variant) in e.variants.iter().enumerate() {
        let vident = &variant.ident;
        // Hash the variant index as a stable, platform-independent discriminant.
        let disc = vi as u64;

        match &variant.fields {
            Fields::Unit => {
                eq_arms.push(quote!( (#ident::#vident, #ident::#vident) => true ));
                hash_arms.push(quote!(
                    #ident::#vident => { ::core::hash::Hash::hash(&#disc, state); }
                ));
            }
            Fields::Unnamed(unnamed) => {
                let n = unnamed.unnamed.len();
                let a_bind: Vec<Ident> = (0..n).map(|i| format_ident!("__a{}", i)).collect();
                let b_bind: Vec<Ident> = (0..n).map(|i| format_ident!("__b{}", i)).collect();
                // Variant bindings come from matching `&Self`, so each is `&T`.
                let cmp = unnamed.unnamed.iter().enumerate().map(|(i, f)| {
                    let (a, b) = (&a_bind[i], &b_bind[i]);
                    eq_for(&f.ty, &quote!(#a), &quote!(#b))
                });
                eq_arms.push(quote!(
                    (#ident::#vident( #(#a_bind),* ), #ident::#vident( #(#b_bind),* ))
                        => { true #( && (#cmp) )* }
                ));
                let hashes = unnamed.unnamed.iter().enumerate().map(|(i, f)| {
                    let a = &a_bind[i];
                    hash_for(&f.ty, &quote!(#a), &quote!(state))
                });
                hash_arms.push(quote!(
                    #ident::#vident( #(#a_bind),* ) => {
                        ::core::hash::Hash::hash(&#disc, state);
                        #( #hashes )*
                    }
                ));
            }
            Fields::Named(named) => {
                let names: Vec<&Ident> = named
                    .named
                    .iter()
                    .map(|f| f.ident.as_ref().unwrap())
                    .collect();
                let a_bind: Vec<Ident> = names.iter().map(|n| format_ident!("__a_{}", n)).collect();
                let b_bind: Vec<Ident> = names.iter().map(|n| format_ident!("__b_{}", n)).collect();
                let cmp = named.named.iter().enumerate().map(|(i, f)| {
                    let (a, b) = (&a_bind[i], &b_bind[i]);
                    eq_for(&f.ty, &quote!(#a), &quote!(#b))
                });
                eq_arms.push(quote!(
                    (
                        #ident::#vident { #( #names: #a_bind ),* },
                        #ident::#vident { #( #names: #b_bind ),* }
                    ) => { true #( && (#cmp) )* }
                ));
                let hashes = named.named.iter().enumerate().map(|(i, f)| {
                    let a = &a_bind[i];
                    hash_for(&f.ty, &quote!(#a), &quote!(state))
                });
                hash_arms.push(quote!(
                    #ident::#vident { #( #names: #a_bind ),* } => {
                        ::core::hash::Hash::hash(&#disc, state);
                        #( #hashes )*
                    }
                ));
            }
        }
    }

    let eq = quote!(
        match (self, other) {
            #( #eq_arms, )*
            #[allow(unreachable_patterns)]
            _ => false,
        }
    );
    let hash = quote!( match self { #( #hash_arms )* } );
    (eq, hash)
}

// ─── float-aware recursion ───────────────────────────────────────────────────

enum FloatBits {
    F32,
    F64,
}

enum Shape<'a> {
    Float(FloatBits),
    Opt(&'a Type),
    Seq(&'a Type),
    Tuple(Vec<&'a Type>),
    /// Hash/compare via the type's own `Hash`/`PartialEq`.
    Delegate,
}

fn classify(ty: &Type) -> Shape<'_> {
    match ty {
        Type::Array(a) => Shape::Seq(&a.elem),
        Type::Slice(s) => Shape::Seq(&s.elem),
        Type::Tuple(t) => Shape::Tuple(t.elems.iter().collect()),
        Type::Reference(r) => classify(&r.elem),
        Type::Paren(p) => classify(&p.elem),
        Type::Group(g) => classify(&g.elem),
        Type::Path(tp) => {
            let Some(seg) = tp.path.segments.last() else {
                return Shape::Delegate;
            };
            match seg.ident.to_string().as_str() {
                "f32" if seg.arguments.is_empty() => Shape::Float(FloatBits::F32),
                "f64" if seg.arguments.is_empty() => Shape::Float(FloatBits::F64),
                "Option" => single_type_arg(seg).map_or(Shape::Delegate, Shape::Opt),
                "Vec" => single_type_arg(seg).map_or(Shape::Delegate, Shape::Seq),
                _ => Shape::Delegate,
            }
        }
        _ => Shape::Delegate,
    }
}

fn single_type_arg(seg: &syn::PathSegment) -> Option<&Type> {
    let PathArguments::AngleBracketed(ab) = &seg.arguments else {
        return None;
    };
    ab.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    })
}

/// Whether `ty` reaches a bare `f32`/`f64` through transparent containers and
/// therefore needs bit-pattern handling rather than plain delegation.
fn needs_bit(ty: &Type) -> bool {
    match classify(ty) {
        Shape::Float(_) => true,
        Shape::Opt(inner) | Shape::Seq(inner) => needs_bit(inner),
        Shape::Tuple(elems) => elems.iter().any(|t| needs_bit(t)),
        Shape::Delegate => false,
    }
}

/// Equality expression for a value of type `ty`. Both `a` and `b` are `&T`
/// expressions.
fn eq_for(ty: &Type, a: &TokenStream2, b: &TokenStream2) -> TokenStream2 {
    if !needs_bit(ty) {
        return quote!( #a == #b );
    }
    match classify(ty) {
        Shape::Float(_) => quote!( #a.to_bits() == #b.to_bits() ),
        Shape::Opt(inner) => {
            let inner = eq_for(inner, &quote!(__a), &quote!(__b));
            quote!(
                match (#a, #b) {
                    (::core::option::Option::Some(__a), ::core::option::Option::Some(__b)) => #inner,
                    (::core::option::Option::None, ::core::option::Option::None) => true,
                    _ => false,
                }
            )
        }
        Shape::Seq(inner) => {
            let inner = eq_for(inner, &quote!(__a), &quote!(__b));
            quote!(
                #a.len() == #b.len()
                    && #a.iter().zip(#b.iter()).all(|(__a, __b)| #inner)
            )
        }
        Shape::Tuple(elems) => {
            let parts = elems.iter().enumerate().map(|(i, t)| {
                let idx = Index::from(i);
                eq_for(t, &quote!(&(#a).#idx), &quote!(&(#b).#idx))
            });
            quote!( true #( && (#parts) )* )
        }
        Shape::Delegate => unreachable!("needs_bit guards the delegate case"),
    }
}

/// Hashing statement(s) for a value of type `ty`. `v` is a `&T` expression.
fn hash_for(ty: &Type, v: &TokenStream2, state: &TokenStream2) -> TokenStream2 {
    if !needs_bit(ty) {
        return quote!( ::core::hash::Hash::hash(#v, #state); );
    }
    match classify(ty) {
        Shape::Float(FloatBits::F32) => {
            let core = crate::core();
            quote!( #core::dyn_compare::hash_f32(*#v, #state); )
        }
        Shape::Float(FloatBits::F64) => {
            quote!( ::core::hash::Hash::hash(&(*#v).to_bits(), #state); )
        }
        Shape::Opt(inner) => {
            let inner = hash_for(inner, &quote!(__v), state);
            quote!(
                match #v {
                    ::core::option::Option::Some(__v) => {
                        ::core::hash::Hash::hash(&1u8, #state);
                        #inner
                    }
                    ::core::option::Option::None => {
                        ::core::hash::Hash::hash(&0u8, #state);
                    }
                }
            )
        }
        Shape::Seq(inner) => {
            let inner = hash_for(inner, &quote!(__v), state);
            quote!(
                ::core::hash::Hash::hash(&#v.len(), #state);
                for __v in #v.iter() { #inner }
            )
        }
        Shape::Tuple(elems) => {
            let parts = elems.iter().enumerate().map(|(i, t)| {
                let idx = Index::from(i);
                hash_for(t, &quote!(&(#v).#idx), state)
            });
            quote!( #( #parts )* )
        }
        Shape::Delegate => unreachable!("needs_bit guards the delegate case"),
    }
}
