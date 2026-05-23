//! Attribute macros that turn a function-style component definition into a
//! struct + `VectorComponent` / `RasterComponent` impl.
//!
//! ```ignore
//! #[vector_component]
//! fn bouncing_dot(t: LocalTime, scene_width: f32) -> impl VectorComponent {
//!     // ...returns any VectorComponent...
//! }
//! ```
//!
//! Expands to a `BouncingDot` struct (PascalCase of the fn name) whose fields
//! are the function arguments. The function body becomes the component's
//! `body()` implementation, wrapped in `VectorBody::Of(Box::new(...))`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, FnArg, ItemFn, Pat, PatType};

/// Attribute macro for vector components. See crate-level docs.
#[proc_macro_attribute]
pub fn vector_component(_attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_entry(item, Kind::Vector)
}

/// Attribute macro for raster components. The runtime `target: Resolution`
/// argument is *not* surfaced in the function signature — it threads through
/// the generated default `render()` via `RasterBody::Of`.
#[proc_macro_attribute]
pub fn raster_component(_attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_entry(item, Kind::Raster)
}

#[derive(Clone, Copy)]
enum Kind {
    Vector,
    Raster,
}

fn expand_entry(item: TokenStream, kind: Kind) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    match expand(input, kind) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(func: ItemFn, kind: Kind) -> syn::Result<TokenStream2> {
    if let Some(constness) = func.sig.constness {
        return Err(syn::Error::new_spanned(
            constness,
            "component fn cannot be const",
        ));
    }
    if let Some(asyncness) = func.sig.asyncness {
        return Err(syn::Error::new_spanned(
            asyncness,
            "component fn cannot be async",
        ));
    }
    if !func.sig.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &func.sig.generics,
            "component fn cannot have generic parameters (use concrete types as fields)",
        ));
    }

    let vis = &func.vis;
    let fn_ident = &func.sig.ident;
    let struct_ident = snake_to_pascal_ident(fn_ident);

    let mut field_idents: Vec<&syn::Ident> = Vec::new();
    let mut field_types: Vec<&syn::Type> = Vec::new();
    for arg in &func.sig.inputs {
        match arg {
            FnArg::Receiver(r) => {
                return Err(syn::Error::new_spanned(
                    r,
                    "component fn must not take a self receiver",
                ));
            }
            FnArg::Typed(PatType { pat, ty, .. }) => {
                let Pat::Ident(pi) = pat.as_ref() else {
                    return Err(syn::Error::new_spanned(
                        pat,
                        "component fn argument must be a plain identifier",
                    ));
                };
                field_idents.push(&pi.ident);
                field_types.push(ty.as_ref());
            }
        }
    }

    let body = &func.block;

    let (trait_path, body_path, body_of_variant, vec2_path) = match kind {
        Kind::Vector => (
            quote!(::tellur_core::vector::VectorComponent),
            quote!(::tellur_core::vector::VectorBody),
            quote!(::tellur_core::vector::VectorBody::Of),
            quote!(::tellur_core::geometry::Vec2),
        ),
        Kind::Raster => (
            quote!(::tellur_core::raster::RasterComponent),
            quote!(::tellur_core::raster::RasterBody),
            quote!(::tellur_core::raster::RasterBody::Of),
            quote!(::tellur_core::geometry::Vec2),
        ),
    };

    let build_method = syn::Ident::new("__tellur_build", fn_ident.span());

    let output = quote! {
        #[derive(::core::clone::Clone)]
        #vis struct #struct_ident {
            #( pub #field_idents: #field_types, )*
        }

        impl #struct_ident {
            #[doc(hidden)]
            fn #build_method(&self) -> impl #trait_path + 'static {
                let Self { #( #field_idents ),* } = ::core::clone::Clone::clone(self);
                #body
            }
        }

        impl #trait_path for #struct_ident {
            fn view_box(&self) -> #vec2_path {
                #trait_path::view_box(&self.#build_method())
            }
            fn body(&self) -> #body_path {
                #body_of_variant(::std::boxed::Box::new(self.#build_method()))
            }
        }
    };

    Ok(output)
}

fn snake_to_pascal_ident(ident: &syn::Ident) -> syn::Ident {
    let s = ident.to_string();
    let mut out = String::with_capacity(s.len());
    let mut capitalize = true;
    for c in s.chars() {
        if c == '_' {
            capitalize = true;
        } else if capitalize {
            out.extend(c.to_uppercase());
            capitalize = false;
        } else {
            out.push(c);
        }
    }
    syn::Ident::new(&out, ident.span())
}
