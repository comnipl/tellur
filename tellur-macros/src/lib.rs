//! Attribute macros that turn a function-style component definition into a
//! struct + `VectorComponent` / `RasterComponent` impl.
//!
//! ```ignore
//! #[vector_component]
//! fn BouncingDot(#[available] available: Vec2, t: LocalTime) -> impl VectorComponent {
//!     // `available` is the parent-assigned size at render time.
//!     // `t` is a regular struct field.
//!     // The body returns a component tree.
//! }
//! ```
//!
//! Expands to a `BouncingDot` struct (PascalCase of the fn name) whose
//! fields are the non-`#[available]` function arguments. The function
//! body becomes a private `__tellur_build` helper; the trait impl
//! forwards `layout`, `paint_bounds`, and `render` to the built body.
//!
//! When an argument is annotated with `#[available]`, that argument is
//! *not* a struct field — instead it's threaded through the layout
//! protocol: `layout(c)` builds the body with `c.max` (collapsed to 0
//! on unbounded axes), `paint_bounds(size)` and `render(size, ...)`
//! build it with `size`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, FnArg, ItemFn, Pat, PatType};

/// Attribute macro for vector components. See crate-level docs.
#[proc_macro_attribute]
pub fn vector_component(_attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_entry(item, Kind::Vector)
}

/// Attribute macro for raster components. See crate-level docs.
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
    let mut available_ident: Option<&syn::Ident> = None;
    let mut available_type: Option<&syn::Type> = None;
    for arg in &func.sig.inputs {
        match arg {
            FnArg::Receiver(r) => {
                return Err(syn::Error::new_spanned(
                    r,
                    "component fn must not take a self receiver",
                ));
            }
            FnArg::Typed(PatType { pat, ty, attrs, .. }) => {
                let Pat::Ident(pi) = pat.as_ref() else {
                    return Err(syn::Error::new_spanned(
                        pat,
                        "component fn argument must be a plain identifier",
                    ));
                };
                let is_available = attrs.iter().any(|a| a.path().is_ident("available"));
                if is_available {
                    if available_ident.is_some() {
                        return Err(syn::Error::new_spanned(
                            pi,
                            "component fn can have at most one #[available] argument",
                        ));
                    }
                    available_ident = Some(&pi.ident);
                    available_type = Some(ty.as_ref());
                } else {
                    field_idents.push(&pi.ident);
                    field_types.push(ty.as_ref());
                }
            }
        }
    }

    let body = &func.block;

    let (trait_path, graphic_path, render_sig, render_args) = match kind {
        Kind::Vector => (
            quote!(::tellur_core::vector::VectorComponent),
            quote!(::tellur_core::vector::VectorGraphic),
            quote!(size: ::tellur_core::geometry::Vec2),
            quote!(size),
        ),
        Kind::Raster => (
            quote!(::tellur_core::raster::RasterComponent),
            quote!(::tellur_core::raster::RasterImage),
            quote!(
                size: ::tellur_core::geometry::Vec2,
                target: ::tellur_core::raster::Resolution
            ),
            quote!(size, target),
        ),
    };

    let build_method = syn::Ident::new("__tellur_build", fn_ident.span());

    let (build_fn, build_call_layout, build_call_render, build_call_paint_bounds) =
        if let (Some(av_ident), Some(av_type)) = (available_ident, available_type) {
            (
                quote! {
                    #[doc(hidden)]
                    fn #build_method(&self, #av_ident: #av_type) -> impl #trait_path + 'static {
                        let Self { #( #field_idents ),* } = ::core::clone::Clone::clone(self);
                        #body
                    }
                },
                // layout: the parent's max constraint is what's available
                // (collapsed to 0 on unbounded axes — see `finite_or_zero`).
                quote! {
                    let __available = ::tellur_core::geometry::Vec2(
                        if constraints.max.0.is_finite() { constraints.max.0 } else { 0.0 },
                        if constraints.max.1.is_finite() { constraints.max.1 } else { 0.0 },
                    );
                    let __child = self.#build_method(__available);
                    #trait_path::layout(&__child, constraints)
                },
                // render: the assigned size *is* the available size.
                quote! {
                    let __child = self.#build_method(size);
                    #trait_path::render(&__child, #render_args)
                },
                // paint_bounds: also use the assigned size.
                quote! {
                    let __child = self.#build_method(size);
                    #trait_path::paint_bounds(&__child, size)
                },
            )
        } else {
            (
                quote! {
                    #[doc(hidden)]
                    fn #build_method(&self) -> impl #trait_path + 'static {
                        let Self { #( #field_idents ),* } = ::core::clone::Clone::clone(self);
                        #body
                    }
                },
                quote! {
                    let __child = self.#build_method();
                    #trait_path::layout(&__child, constraints)
                },
                quote! {
                    let __child = self.#build_method();
                    #trait_path::render(&__child, #render_args)
                },
                quote! {
                    let __child = self.#build_method();
                    #trait_path::paint_bounds(&__child, size)
                },
            )
        };

    let output = quote! {
        #[derive(::core::clone::Clone)]
        #vis struct #struct_ident {
            #( pub #field_idents: #field_types, )*
        }

        impl #struct_ident {
            #build_fn
        }

        impl #trait_path for #struct_ident {
            fn layout(
                &self,
                constraints: ::tellur_core::geometry::Constraints,
            ) -> ::tellur_core::geometry::Vec2 {
                #build_call_layout
            }

            fn paint_bounds(
                &self,
                size: ::tellur_core::geometry::Vec2,
            ) -> ::tellur_core::geometry::Rect {
                #build_call_paint_bounds
            }

            fn render(&self, #render_sig) -> #graphic_path {
                #build_call_render
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
